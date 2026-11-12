//! `PATCHBAY_LAN=off`: the TF's discovery sweeps and probes nothing.
//! A binary of its own: the switch is the process's environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use patchbay_yamaha_tf::{ScanOptions, discover_consoles_detail, find_by_mac};

#[tokio::test]
async fn with_the_lan_off_the_tf_sweeps_nothing() {
    // SAFETY: before anything in this process reads the environment
    // (nextest runs each test in a process of its own).
    unsafe {
        std::env::set_var("PATCHBAY_LAN", "off");
    }
    let started = Instant::now();
    let scan = discover_consoles_detail(&ScanOptions::default()).await;
    assert_eq!(scan.consoles.len(), 0);
    assert_eq!(scan.silent.len(), 0);
    assert_eq!(
        find_by_mac([0, 0x1b, 0x2c, 1, 2, 3], &ScanOptions::default()).await,
        None
    );
    // A sweep takes seconds (connect timeouts); a refusal is immediate.
    assert!(started.elapsed() < Duration::from_millis(500), "it swept");
}
