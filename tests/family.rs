//! A device family from outside the crate, as a family extension writes
//! one: implemented against `patchbay::family` alone, registered with
//! `family::register`, connected by the engine's hub (studio#415).
//!
//! The engine never touches the machine: a free loopback port, a scratch
//! config whose `devices` section lists only the outside family's device
//! (so no built-in family goes looking on the network), and no `PipeWire`
//! connection. A binary of its own: registrations and the engine are one
//! per process.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use patchbay::device::{
    ChannelRef, DeviceAdapter, DeviceError, DeviceEvent, DeviceId, DeviceInfo, DeviceSnapshot,
    ParamValue, Transport, WriteGuard,
};
use patchbay::family::{self, Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily};
use patchbay::proto::{DeviceConfig, DeviceLinkState, FamilyInfo};
use patchbay::server::{Engine, EngineOptions};
use tokio::sync::broadcast;

const KIND: &str = "outside-desk";
/// Where "discovery" finds the desk.
const FOUND_AT: &str = "mem://outside-desk-1";

struct Desk {
    addr: String,
    events: broadcast::Sender<DeviceEvent>,
}

impl DeviceAdapter for Desk {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::from_parts("outside", "desk", "1"),
            vendor: "Outside".to_owned(),
            model: "Desk".to_owned(),
            serial: Some("1".to_owned()),
            firmware: None,
            transport: Transport::Other {
                description: self.addr.clone(),
            },
            online: true,
        }
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        Ok(DeviceSnapshot {
            info: self.info(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            params: Vec::new(),
        })
    }

    async fn apply_param(
        &self,
        path: &str,
        _value: ParamValue,
        _guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported(path.to_owned()))
    }

    async fn set_route(
        &self,
        output: ChannelRef,
        _source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported(output.to_string()))
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.events.subscribe()
    }
}

struct OutsideFamily {
    label: &'static str,
}

impl DeviceFamily for OutsideFamily {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo {
            kind: KIND.to_owned(),
            label: self.label.to_owned(),
            platforms: Vec::new(),
            available: true,
            unavailable: String::new(),
            origin: "linked".to_owned(),
            discovers: true,
        }
    }

    fn connect(&self, config: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
        Box::pin(async move {
            // Where it was last found, else "discover" it and remember.
            let addr = match ctx.cache().get(&config.name) {
                Some(addr) => addr,
                None => {
                    ctx.cache().remember(&config.name, FOUND_AT);
                    FOUND_AT.to_owned()
                }
            };
            if addr != FOUND_AT {
                return Err(ConnectError::NotFound(format!("nothing at {addr}")));
            }
            let adapter: Adapter = Arc::new(Desk {
                addr,
                events: broadcast::channel(16).0,
            });
            Ok(adapter)
        })
    }
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(t) = f() {
            return t;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "waited for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_family_registered_from_outside_the_crate_connects_through_the_hub() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("patchbay.styx");
    std::fs::write(&config, format!("devices ({{name desk, kind {KIND}}})\n")).unwrap();
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    // SAFETY: before anything in this process reads the environment or
    // starts a thread (nextest runs each test in a process of its own).
    unsafe {
        std::env::set_var("PATCHBAY_CONFIG", &config);
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
    }

    // Registering twice for one kind keeps the second, and hands back the
    // first.
    assert!(family::register(Arc::new(OutsideFamily { label: "first" })).is_none());
    let replaced = family::register(Arc::new(OutsideFamily {
        label: "Outside desk",
    }))
    .expect("the first registration was not replaced");
    assert_eq!(replaced.info().label, "first");

    let mut opts = EngineOptions::new(addr);
    opts.discover_peers = false;
    let engine = Engine::start_or_attach_blocking(opts).unwrap();

    // The engine lists the family beside the built-ins.
    let families = block_on(engine.run(|c| async move { c.families().await }))
        .unwrap()
        .unwrap();
    let outside = families.iter().find(|f| f.kind == KIND).unwrap();
    assert_eq!(outside.label, "Outside desk");
    assert!(families.iter().any(|f| f.kind == "dante"));

    // The hub connects the configured device through it.
    let desk = until("the outside desk online", || {
        block_on(engine.run(|c| async move { c.list_devices().await }))
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "desk" && d.state == DeviceLinkState::Online)
    });
    assert_eq!(desk.kind, KIND);
    assert_eq!(desk.id, "outside:desk:1");

    // What the family remembered through the context's cache is the
    // engine's discovery cache, beside the config.
    let state = std::fs::read_to_string(config.with_extension("state.json")).unwrap();
    assert!(state.contains(FOUND_AT), "{state}");
}
