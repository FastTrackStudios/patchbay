//! The Yamaha TF as Architect hosts it: linked in beside the Patchbay
//! core, its `build` adds the family to the core's engine, and the core's
//! actions reach a console like any device.
//!
//! The engine keeps off the machine's audio and its network: a free
//! loopback port, a scratch config whose `devices` section lists only
//! `tf1`, pinned to a fake TF1 on another loopback port, no `PipeWire`
//! connection, and nothing onto the LAN (`PATCHBAY_LAN=off`: no peer
//! mDNS, no sweep). One test function: the engine and the family
//! registrations are one per process.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use architect::action::ActionList;
use architect_extensions::extensions::Extensions;
use architect_extensions::host::{Host, SharedSections};
use architect_extensions::install::Installs;
use architect_settings::Registry;
use common::fake_tf1::Fake;
use futures::executor::block_on;
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

#[test]
fn the_tf_extension_adds_the_family_and_its_console_reads_through_the_core() {
    // The fake console, on the engine's runtime (it outlives this call).
    let fake: Fake = block_on(patchbay::server::spawn(Fake::start())).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("patchbay.styx");
    std::fs::write(
        &config,
        format!(
            "devices ({{name tf1, kind yamaha-tf, addr \"{}\"}})\n",
            fake.addr
        ),
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
        std::env::set_var("PATCHBAY_LAN", "off");
    }

    let list = ActionList::new();
    let host = Host::new(list.clone(), SharedSections::new(Registry::new()));
    let ext = Extensions::new(host, Installs::new(tmp.path().join("installs")))
        .with_linked(&patchbay::ext::Patchbay)
        .unwrap()
        .with_linked(&patchbay_yamaha_tf::ext::YamahaTf)
        .unwrap();
    ext.install_contribution_actions().unwrap();
    let _ = ext.sync();

    // The core's engine has the family: after the built-ins (whichever
    // are still built in), so the default device list ends with `tf1`.
    let families = run(&list, "PATCHBAY_FAMILIES", &json!({})).unwrap();
    let kinds: Vec<&str> = families
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"system-audio"), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"yamaha-tf"), "{kinds:?}");
    let tf = families.as_array().unwrap().last().unwrap();
    assert_eq!(tf["label"], "Yamaha TF");
    assert_eq!(tf["origin"], "linked");
    assert_eq!(tf["available"], true);
    assert_eq!(tf["discovers"], true);

    // The hub connects the console through it.
    let start = Instant::now();
    let tf1 = loop {
        let devices = run(&list, "PATCHBAY_DEVICE_LIST", &json!({})).unwrap();
        let tf1 = devices
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "tf1")
            .cloned()
            .unwrap();
        if tf1["state"] == "Online" || tf1["state"] == "online" {
            break tf1;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "tf1 never came online: {tf1}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(tf1["kind"], "yamaha-tf");
    assert_eq!(tf1["vendor"], "Yamaha");

    // `device show tf1`: the console's params, read through the core.
    let shown = run(&list, "PATCHBAY_DEVICE_SHOW", &json!({"id": "tf1"})).unwrap();
    let params = shown["params"].as_array().unwrap();
    assert!(params.len() > 1000, "{} params", params.len());
    assert!(
        params.iter().any(|p| p["path"] == "in/1/level"),
        "no in/1/level"
    );
    assert_eq!(shown["summary"]["id"], tf1["id"]);
    // With PATCHBAY_TEST_DUMP set, the answer goes to that file, for a
    // byte comparison against an engine with the TF built in.
    if let Ok(path) = std::env::var("PATCHBAY_TEST_DUMP") {
        let text = serde_json::to_string_pretty(&shown)
            .unwrap()
            .replace(&fake.addr.to_string(), "<fake-tf1>");
        std::fs::write(path, text).unwrap();
    }
}
