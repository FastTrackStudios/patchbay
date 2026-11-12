//! The hub, driven through a registered family that is not one of the
//! built-ins: what any family extension gets from it (studio#415).
//!
//! `FakeFamily` connects three devices by config name: `desk` (an
//! in-memory console with one level and one router output), `ghost`
//! (discovery finds nothing) and `down` (found, unreachable).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use super::DeviceHub;
use super::cache::DiscoveryCache;
use crate::device::{
    ChannelRef, Crosspoint, DeviceAdapter, DeviceError, DeviceEvent, DeviceId, DeviceInfo,
    DeviceSnapshot, Param, ParamKind, ParamValue, PortGroup, Transport, WriteGuard,
};
use crate::family::{Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily, Families};
use crate::proto::{
    DeviceConfig, DeviceEventKind, DeviceLinkState, DeviceParamValue, DeviceRestoreStatus,
    FamilyInfo,
};
use crate::server::presets::PresetStore;

const LEVEL: &str = "mixer/1/level";

/// The console's state, shared by every connection to it (a reconnect
/// reaches the same desk).
struct Desk {
    snap: parking_lot::Mutex<DeviceSnapshot>,
    events: broadcast::Sender<DeviceEvent>,
}

impl Desk {
    fn new() -> Arc<Self> {
        let info = DeviceInfo {
            id: DeviceId::from_parts("fake", "desk", "1"),
            vendor: "Fake".to_owned(),
            model: "Desk".to_owned(),
            serial: Some("1".to_owned()),
            firmware: None,
            transport: Transport::Other {
                description: "in memory".to_owned(),
            },
            online: true,
        };
        let snap = DeviceSnapshot {
            info,
            inputs: vec![PortGroup {
                id: "in".to_owned(),
                name: "IN".to_owned(),
                channels: 2,
            }],
            outputs: vec![PortGroup {
                id: "out".to_owned(),
                name: "OUT".to_owned(),
                channels: 1,
            }],
            routes: vec![Crosspoint {
                output: ChannelRef::new("out", 0),
                source: Some(ChannelRef::new("in", 0)),
            }],
            params: vec![Param {
                path: LEVEL.to_owned(),
                label: "Level".to_owned(),
                kind: ParamKind::Level {
                    min_db: -96.0,
                    max_db: 10.0,
                },
                value: ParamValue::Level(0.0),
                writable: true,
                disruptive: false,
            }],
        };
        Arc::new(Self {
            snap: parking_lot::Mutex::new(snap),
            events: broadcast::channel(64).0,
        })
    }

    fn level(&self) -> f64 {
        match self.snap.lock().param(LEVEL).map(|p| p.value.clone()) {
            Some(ParamValue::Level(db)) => db,
            other => panic!("level: {other:?}"),
        }
    }
}

struct FakeAdapter(Arc<Desk>);

impl DeviceAdapter for FakeAdapter {
    fn info(&self) -> DeviceInfo {
        self.0.snap.lock().info.clone()
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        Ok(self.0.snap.lock().clone())
    }

    async fn apply_param(
        &self,
        path: &str,
        value: ParamValue,
        _guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        {
            let mut snap = self.0.snap.lock();
            let p = snap
                .params
                .iter_mut()
                .find(|p| p.path == path)
                .ok_or_else(|| DeviceError::Unsupported(path.to_owned()))?;
            p.value = value.clone();
        }
        let _ = self.0.events.send(DeviceEvent::ParamChanged {
            path: path.to_owned(),
            value,
        });
        Ok(())
    }

    async fn set_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        let x = Crosspoint { output, source };
        {
            let mut snap = self.0.snap.lock();
            let c = snap
                .routes
                .iter_mut()
                .find(|c| c.output == x.output)
                .ok_or_else(|| DeviceError::Unsupported(x.output.to_string()))?;
            c.source.clone_from(&x.source);
        }
        let _ = self.0.events.send(DeviceEvent::RouteChanged(x));
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.0.events.subscribe()
    }
}

struct FakeFamily(Arc<Desk>);

impl DeviceFamily for FakeFamily {
    fn kind(&self) -> &'static str {
        "fake-desk"
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo {
            kind: self.kind().to_owned(),
            label: "Fake desk".to_owned(),
            platforms: Vec::new(),
            available: true,
            unavailable: String::new(),
            origin: "linked".to_owned(),
            discovers: true,
        }
    }

    fn defaults(&self) -> Vec<DeviceConfig> {
        ["desk", "ghost", "down"]
            .map(|name| DeviceConfig::new(name, self.kind()))
            .to_vec()
    }

    fn connect(&self, config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        let desk = Arc::clone(&self.0);
        Box::pin(async move {
            match config.name.as_str() {
                "desk" => {
                    let adapter: Adapter = Arc::new(FakeAdapter(desk));
                    Ok(adapter)
                }
                "ghost" => Err(ConnectError::NotFound("nothing answered".to_owned())),
                _ => Err(ConnectError::Failed("refused".to_owned())),
            }
        })
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "waited for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn state(hub: &DeviceHub, name: &str) -> (DeviceLinkState, String) {
    let d = hub.list().into_iter().find(|d| d.name == name).unwrap();
    (d.state, d.error)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hub_runs_a_registered_family() {
    let dir = std::env::temp_dir().join(format!("patchbay-family-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let presets = Arc::new(PresetStore::open_at(dir.join("patchbay.styx")));
    let desk = Desk::new();
    let families = Families::new().with(FakeFamily(Arc::clone(&desk)));
    let hub = DeviceHub::start_with(
        presets,
        &families,
        &ConnectCtx::new(Arc::new(DiscoveryCache::memory())),
    );
    let events = hub.events().subscribe(64);

    // Connect: the family's defaults are the device list; each one's
    // link says what its connect said.
    until("the desk online", || {
        state(&hub, "desk").0 == DeviceLinkState::Online
    })
    .await;
    until("the ghost not found", || {
        state(&hub, "ghost").0 == DeviceLinkState::NotFound
    })
    .await;
    until("the down device offline", || {
        state(&hub, "down").0 == DeviceLinkState::Offline
    })
    .await;
    assert_eq!(state(&hub, "ghost").1, "nothing answered");
    assert_eq!(state(&hub, "down").1, "refused");
    let kinds: Vec<String> = hub.family_infos().into_iter().map(|i| i.kind).collect();
    assert_eq!(kinds, ["fake-desk"]);

    let id = DeviceId::from_parts("fake", "desk", "1").to_string();
    let view = hub.view(&id).await.unwrap();
    assert_eq!(view.summary.state, DeviceLinkState::Online);
    assert_eq!(view.params.len(), 1);

    // A write goes to the device and comes back as its event.
    let p = hub
        .set_param(&id, LEVEL, &DeviceParamValue::Level(-6.0), false)
        .await
        .unwrap();
    assert_eq!(p.value, DeviceParamValue::Level(-6.0));
    assert!((desk.level() + 6.0).abs() < 1e-9);
    let mut saw_param = false;
    while let Ok(ev) = tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
        let ev = ev.unwrap();
        if ev.device == id && matches!(ev.event, DeviceEventKind::ParamChanged { .. }) {
            saw_param = true;
            break;
        }
    }
    assert!(saw_param, "the param change never reached the hub's events");

    // Snapshot: save, change, diff, restore.
    hub.save_snapshot(&id, "show", Vec::new(), Vec::new())
        .await
        .unwrap();
    hub.set_param(&id, LEVEL, &DeviceParamValue::Level(-20.0), false)
        .await
        .unwrap();
    let diff = hub.restore("show", &[], true, false).await.unwrap();
    assert_eq!(diff.count(DeviceRestoreStatus::Planned), 1, "{diff:?}");
    assert!(
        (desk.level() + 20.0).abs() < 1e-9,
        "a diff wrote to the device"
    );
    let done = hub.restore("show", &[], false, false).await.unwrap();
    assert_eq!(done.count(DeviceRestoreStatus::Applied), 1, "{done:?}");
    assert!((desk.level() + 6.0).abs() < 1e-9);

    // Offline, then back: the hub reconnects through the family.
    let _ = desk.events.send(DeviceEvent::Offline);
    until("the desk offline", || {
        state(&hub, "desk").0 == DeviceLinkState::Offline
    })
    .await;
    until("the desk back online", || {
        state(&hub, "desk").0 == DeviceLinkState::Online
    })
    .await;

    let _ = std::fs::remove_dir_all(&dir);
}

/// Where [`PanickyAdapter`] panics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Boom {
    Subscribe,
    Info,
}

/// A connected device whose `subscribe` or `info` panics.
struct PanickyAdapter(Arc<Desk>, Boom);

impl DeviceAdapter for PanickyAdapter {
    fn info(&self) -> DeviceInfo {
        assert!(self.1 != Boom::Info, "the device's info panicked");
        self.0.snap.lock().info.clone()
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        Ok(self.0.snap.lock().clone())
    }

    async fn apply_param(
        &self,
        _path: &str,
        _value: ParamValue,
        _guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        Ok(())
    }

    async fn set_route(
        &self,
        _output: ChannelRef,
        _source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        assert!(self.1 != Boom::Subscribe, "the device's subscribe panicked");
        self.0.events.subscribe()
    }
}

/// A family that panics everywhere it can, by device name: `connect`
/// itself, the future it returns, the adapter's `subscribe` and `info`.
/// `flaky` panics on its first connect and connects on the next.
struct PanickyFamily {
    desk: Arc<Desk>,
    tries: Arc<std::sync::atomic::AtomicUsize>,
}

impl DeviceFamily for PanickyFamily {
    fn kind(&self) -> &'static str {
        "panicky"
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo {
            kind: self.kind().to_owned(),
            label: "Panicky".to_owned(),
            platforms: Vec::new(),
            available: true,
            unavailable: String::new(),
            origin: "linked".to_owned(),
            discovers: false,
        }
    }

    fn connect(&self, config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        let desk = Arc::clone(&self.desk);
        match config.name.as_str() {
            "in-connect" => panic!("connect panicked before its future"),
            "flaky" => {
                let first = self.tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                Box::pin(async move {
                    assert!(!first, "the first connect panicked");
                    let adapter: Adapter = Arc::new(FakeAdapter(desk));
                    Ok(adapter)
                })
            }
            name => {
                let name = name.to_owned();
                Box::pin(async move {
                    let adapter: Adapter = match name.as_str() {
                        "in-future" => panic!("the connect future panicked"),
                        "in-subscribe" => Arc::new(PanickyAdapter(desk, Boom::Subscribe)),
                        _ => Arc::new(PanickyAdapter(desk, Boom::Info)),
                    };
                    Ok(adapter)
                })
            }
        }
    }
}

/// A family whose `info` and `defaults` panic.
struct BrokenFamily;

impl DeviceFamily for BrokenFamily {
    fn kind(&self) -> &'static str {
        "broken"
    }

    fn info(&self) -> FamilyInfo {
        panic!("info panicked")
    }

    fn defaults(&self) -> Vec<DeviceConfig> {
        panic!("defaults panicked")
    }

    fn connect(&self, _config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        Box::pin(async { Err(ConnectError::Failed("never asked".to_owned())) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_family_that_panics_takes_down_only_its_own_device() {
    let dir = std::env::temp_dir().join(format!("patchbay-panic-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("patchbay.styx");
    let names = [
        "in-connect",
        "in-future",
        "in-subscribe",
        "in-info",
        "flaky",
    ];
    let entries: Vec<String> = names
        .iter()
        .map(|n| format!("{{name {n}, kind panicky}}"))
        .chain(["{name desk, kind fake-desk}".to_owned()])
        .collect();
    std::fs::write(&config, format!("devices ({})\n", entries.join(" "))).unwrap();
    let presets = Arc::new(PresetStore::open_at(config));
    assert_eq!(presets.device_configs().len(), 6, "the config did not load");
    let families = Families::new()
        .with(BrokenFamily)
        .with(PanickyFamily {
            desk: Desk::new(),
            tries: Arc::default(),
        })
        .with(FakeFamily(Desk::new()));

    // Starting asks every family's info and defaults: a panic there is
    // reported, never the hub's end.
    let hub = DeviceHub::start_with(
        presets,
        &families,
        &ConnectCtx::new(Arc::new(DiscoveryCache::memory())),
    );
    let infos = hub.family_infos();
    let broken = infos.iter().find(|i| i.kind == "broken").unwrap();
    assert!(!broken.available);
    assert!(broken.unavailable.contains("info panicked"), "{broken:?}");
    assert!(infos.iter().any(|i| i.kind == "panicky" && i.available));

    // Each panic is a failed connect, retried; the other devices connect.
    until("the healthy desk online", || {
        state(&hub, "desk").0 == DeviceLinkState::Online
    })
    .await;
    for (name, why) in [
        ("in-connect", "connect panicked before its future"),
        ("in-future", "the connect future panicked"),
        ("in-subscribe", "the device's subscribe panicked"),
        ("in-info", "the device's info panicked"),
    ] {
        until(name, || {
            let (link, error) = state(&hub, name);
            link == DeviceLinkState::Offline && error.contains(why)
        })
        .await;
    }
    // A panic is retried like any failure: the next try connects.
    until("the flaky device online on its retry", || {
        state(&hub, "flaky").0 == DeviceLinkState::Online
    })
    .await;

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_family_whose_defaults_panic_adds_no_devices() {
    // No config file at all: no `devices` section, so the device list is
    // the families' defaults, which is where `defaults()` is asked.
    let dir = std::env::temp_dir().join(format!("patchbay-defaults-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let presets = Arc::new(PresetStore::open_at(dir.join("absent.styx")));
    assert_eq!(presets.device_configs().len(), 0, "a devices section");
    let families = Families::new()
        .with(BrokenFamily)
        .with(FakeFamily(Desk::new()));
    let hub = DeviceHub::start_with(
        presets,
        &families,
        &ConnectCtx::new(Arc::new(DiscoveryCache::memory())),
    );
    let names: Vec<String> = hub.list().into_iter().map(|d| d.name).collect();
    assert_eq!(
        names,
        ["desk", "ghost", "down"],
        "the broken family added devices"
    );
    // It is still listed, as unavailable.
    let broken = hub
        .family_infos()
        .into_iter()
        .find(|i| i.kind == "broken")
        .unwrap();
    assert!(!broken.available);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A family whose connect always fails, counting its attempts.
struct CountingFamily(Arc<std::sync::atomic::AtomicUsize>);

impl DeviceFamily for CountingFamily {
    fn kind(&self) -> &'static str {
        "counting"
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo::linked("counting", "Counting", &[], false)
    }

    fn connect(&self, _config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(ConnectError::Failed("refused".to_owned())) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_device_is_retried_with_backoff_not_in_a_loop() {
    let dir = std::env::temp_dir().join(format!("patchbay-backoff-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("patchbay.styx");
    std::fs::write(&config, "devices ({name c, kind counting})\n").unwrap();
    let presets = Arc::new(PresetStore::open_at(config));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let families = Families::new().with(CountingFamily(Arc::clone(&attempts)));
    let _hub = DeviceHub::start_with(
        presets,
        &families,
        &ConnectCtx::new(Arc::new(DiscoveryCache::memory())),
    );
    // The first try at once, the next after the 2 s minimum backoff, the
    // one after that 4 s later: two in three seconds.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let n = attempts.load(std::sync::atomic::Ordering::SeqCst);
    assert!((2..=3).contains(&n), "{n} connect attempts in 3 s");
    let _ = std::fs::remove_dir_all(&dir);
}
