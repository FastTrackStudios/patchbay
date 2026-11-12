//! The Antelope family as Architect hosts it: linked in beside the
//! Patchbay core, its `build` adds the family to the core's engine, and
//! the core's actions reach a Galaxy32 like any device.
//!
//! The engine never touches the machine: a free loopback port, a scratch
//! config whose `devices` section lists only `galaxy32`, and no `PipeWire`
//! connection. The real family finds a Galaxy32 by the Manager Server's
//! multicast announces; a test must not put fake announces on the LAN
//! (the real Antelope panel listens there), so the test registers, for
//! the same kind, a family that connects the real `Galaxy32Adapter`
//! straight to a fake Manager Server on loopback. One test function: the
//! engine and the family registrations are one per process.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty
)]

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use architect::action::ActionList;
use architect_extensions::extensions::Extensions;
use architect_extensions::host::{Host, SharedSections};
use architect_extensions::install::Installs;
use architect_settings::Registry;
use common::fake_manager::{announce, fake};
use futures::executor::block_on;
use patchbay::family::{Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily};
use patchbay::proto::{DeviceConfig, FamilyInfo};
use patchbay_antelope::Galaxy32Adapter;
use patchbay_antelope::family::AntelopeGalaxy32Family;
use serde_json::{Value, json};

fn run(list: &ActionList, id: &str, args: &Value) -> Result<Value, String> {
    block_on(list.run_json_async(id, args.to_string().as_bytes()))
        .map(|out| serde_json::from_slice(&out).unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The Galaxy32 family, connecting to the fake server it was given
/// instead of discovering one.
struct AtTheFake(SocketAddr);

impl DeviceFamily for AtTheFake {
    fn kind(&self) -> &'static str {
        patchbay_antelope::KIND
    }

    fn info(&self) -> FamilyInfo {
        AntelopeGalaxy32Family.info()
    }

    fn connect(&self, _config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        let addr = self.0;
        Box::pin(async move {
            let adapter = Galaxy32Adapter::connect(addr, &announce(addr))
                .await
                .map_err(|e| ConnectError::Failed(e.to_string()))?;
            let adapter: Adapter = Arc::new(adapter);
            Ok(adapter)
        })
    }
}

#[test]
fn the_antelope_extension_adds_the_family_and_the_galaxy32_reads_through_the_core() {
    let (fake_addr, _state) = block_on(patchbay::server::spawn(fake())).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("patchbay.styx");
    std::fs::write(
        &config,
        "devices ({name galaxy32, kind antelope-galaxy32})\n",
    )
    .unwrap();
    let addr = format!("127.0.0.1:{}", free_port());
    // SAFETY: before anything in this process reads the environment
    // (nextest runs each test in a process of its own; the fake's runtime
    // reads none of these).
    unsafe {
        std::env::set_var("PATCHBAY_CONFIG", &config);
        std::env::set_var("PATCHBAY_ADDR", &addr);
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
    }

    let list = ActionList::new();
    let host = Host::new(list.clone(), SharedSections::new(Registry::new()));
    let ext = Extensions::new(host, Installs::new(tmp.path().join("installs")))
        .with_linked(&patchbay::ext::Patchbay)
        .unwrap()
        .with_linked(&patchbay_antelope::ext::Antelope)
        .unwrap();
    ext.install_contribution_actions().unwrap();
    let _ = ext.sync();
    // The extension registered the family; this one, for the same kind,
    // replaces it (and hands it back).
    let replaced = patchbay::family::register(Arc::new(AtTheFake(fake_addr)))
        .expect("the extension did not register its family");
    assert_eq!(replaced.kind(), "antelope-galaxy32");

    // The core's engine has the family, after the built-ins.
    let families = run(&list, "PATCHBAY_FAMILIES", &json!({})).unwrap();
    let kinds: Vec<&str> = families
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["system-audio", "dante", "antelope-galaxy32"]);
    let galaxy = &families[2];
    assert_eq!(galaxy["label"], "Antelope Galaxy32");
    assert_eq!(galaxy["discovers"], true);

    // The hub connects it.
    let start = Instant::now();
    let summary = loop {
        let devices = run(&list, "PATCHBAY_DEVICE_LIST", &json!({})).unwrap();
        let d = devices
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "galaxy32")
            .cloned()
            .unwrap();
        if d["state"] == "Online" || d["state"] == "online" {
            break d;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "galaxy32 never came online: {d}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(summary["id"], "antelope:galaxy32:4202524000109");

    // `device show galaxy32`: its router and params, through the core.
    let shown = run(&list, "PATCHBAY_DEVICE_SHOW", &json!({"id": "galaxy32"})).unwrap();
    assert!(!shown["outputs"].as_array().unwrap().is_empty());
    assert!(!shown["routes"].as_array().unwrap().is_empty());
    assert!(!shown["params"].as_array().unwrap().is_empty());
    // With PATCHBAY_TEST_DUMP set, the answer goes to that file, for a
    // byte comparison against an engine with Antelope built in.
    if let Ok(path) = std::env::var("PATCHBAY_TEST_DUMP") {
        let text = serde_json::to_string_pretty(&shown)
            .unwrap()
            .replace(&fake_addr.to_string(), "<fake-manager>");
        std::fs::write(path, text).unwrap();
    }
}
