//! `PATCHBAY_LAN=off`: the Antelope family binds no announce port and
//! joins no multicast group. Every check here is refused before a socket
//! opens. A binary of its own: the switch is the process's environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use patchbay::family::{ConnectCtx, DeviceFamily};
use patchbay::proto::DeviceConfig;
use patchbay_antelope::family::AntelopeGalaxy32Family;
use patchbay_antelope::{KIND, Listener, discover};

#[tokio::test]
async fn with_the_lan_off_nothing_listens_for_announces() {
    // SAFETY: first thing in the test, on its runtime's only thread
    // (nextest runs each test in a process of its own).
    unsafe { std::env::set_var("PATCHBAY_LAN", "off") };

    let bind = Listener::bind().expect_err("it bound the announce port");
    assert!(bind.to_string().contains("PATCHBAY_LAN=off"), "{bind}");

    // Discovery, asked for 5 s, is refused at once (a listen would take
    // the whole window).
    let started = Instant::now();
    let found = discover(Duration::from_secs(5)).await;
    assert!(started.elapsed() < Duration::from_secs(1), "it listened");
    assert!(found.unwrap_err().to_string().contains("PATCHBAY_LAN=off"));

    // The real family, found or pinned: refused the same way.
    for addr in ["", "127.0.0.1:9"] {
        let mut config = DeviceConfig::new("galaxy32", KIND);
        addr.clone_into(&mut config.addr);
        let started = Instant::now();
        let err = AntelopeGalaxy32Family::default()
            .connect(config, ConnectCtx::in_memory())
            .await
            .err()
            .expect("it connected");
        assert!(started.elapsed() < Duration::from_secs(1), "it listened");
        assert!(err.message().contains("PATCHBAY_LAN=off"), "{err:?}");
    }
}
