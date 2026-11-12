//! `PATCHBAY_LAN=off` (`patchbay::lan`): with it, nothing in this
//! process reaches the local network. Every place that would refuses
//! at its lowest point. Nothing here sends anything: each call checked
//! is refused before it opens a socket.
//!
//! A binary of its own: the switch is the process's environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use patchbay::dante::{DanteControl, Endpoint, InfernoControl};
use patchbay::server::{Engine, EngineMode, EngineOptions};

#[test]
fn with_the_lan_off_nothing_reaches_the_network() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: before anything in this process reads the environment
    // (nextest runs each test in a process of its own).
    unsafe {
        std::env::set_var("PATCHBAY_LAN", "off");
        std::env::set_var("PATCHBAY_CONFIG", tmp.path().join("patchbay.styx"));
        std::env::set_var("PATCHBAY_DEVICES", "off");
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
    }
    assert!(!patchbay::lan::enabled());

    // Peer discovery (mDNS on `_patchbay._tcp`) starts off.
    assert!(!EngineOptions::new("127.0.0.1:0").discover_peers);
    assert!(!EngineOptions::machine().discover_peers);

    // The Dante network: no browse, no ARC request. A browse would take
    // its whole timeout; a refusal is immediate.
    let dante = InfernoControl::default();
    let started = Instant::now();
    let browse = block_on(dante.discover(Duration::from_secs(8)));
    assert!(started.elapsed() < Duration::from_secs(1), "it browsed");
    assert!(
        browse.unwrap_err().to_string().contains("PATCHBAY_LAN=off"),
        "the browse was not refused"
    );
    let device = Endpoint {
        name: "somewhere".to_owned(),
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 4440),
    };
    assert!(!block_on(dante.read(&device)).reachable);
    let write = block_on(dante.subscribe(&device, 1, "01", "elsewhere")).unwrap_err();
    assert!(write.to_string().contains("PATCHBAY_LAN=off"), "{write}");
    let clear = block_on(dante.unsubscribe(&device, 1)).unwrap_err();
    assert!(clear.to_string().contains("PATCHBAY_LAN=off"), "{clear}");

    // The Antelope announces: no bind, no multicast join.
    let bind = patchbay::antelope::Listener::bind()
        .err()
        .expect("it bound");
    assert!(bind.to_string().contains("PATCHBAY_LAN=off"), "{bind}");

    // An engine: its Dante scan (what `health` and the grid run) refuses
    // too.
    let mut opts = EngineOptions::new("127.0.0.1:0");
    opts.discover_peers = true; // asked for, and still refused below
    let engine = Engine::start_or_attach_blocking(opts).unwrap();
    assert!(matches!(engine.mode(), EngineMode::Owned { .. }));
    let scan = block_on(engine.run(|c| async move { c.dante_network().await }))
        .unwrap()
        .unwrap_err();
    assert!(scan.to_string().contains("PATCHBAY_LAN=off"), "{scan}");
    let hosts = block_on(engine.run(|c| async move { c.hosts().await }))
        .unwrap()
        .unwrap();
    assert!(
        format!("{hosts:?}").contains("PATCHBAY_LAN=off"),
        "peer discovery started: {hosts:?}"
    );
}
