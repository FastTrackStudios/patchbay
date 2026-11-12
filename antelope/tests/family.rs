//! The family on its own, the way any family can test itself:
//! `ConnectCtx::in_memory()` (the core's `test-util` feature), no engine.
//!
//! The Galaxy32 is found by the Manager Server's multicast announces, so
//! `connect` always listens for them (receive only; nothing here sends on
//! the network). A pinned loopback address no Manager Server announces is
//! "not found", the way a Galaxy32 that is switched off reads.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty
)]

use patchbay::family::{ConnectCtx, ConnectError, DeviceFamily};
use patchbay::proto::DeviceConfig;
use patchbay_antelope::KIND;
use patchbay_antelope::family::AntelopeGalaxy32Family;

fn galaxy32(addr: &str) -> DeviceConfig {
    let mut config = DeviceConfig::new("galaxy32", KIND);
    addr.clone_into(&mut config.addr);
    config
}

#[tokio::test]
async fn a_pinned_address_nobody_announces_is_not_found() {
    let err = AntelopeGalaxy32Family
        .connect(galaxy32("127.0.0.1:9"), ConnectCtx::in_memory())
        .await
        .err()
        .expect("connected without an announce");
    match err {
        ConnectError::NotFound(why) => assert!(why.contains("127.0.0.1:9"), "{why}"),
        // A machine whose multicast socket can't be opened says so.
        ConnectError::Failed(why) => assert!(!why.is_empty()),
    }
}

#[tokio::test]
async fn a_pinned_address_that_is_not_an_address_fails() {
    let err = AntelopeGalaxy32Family
        .connect(galaxy32("not an address"), ConnectCtx::in_memory())
        .await
        .err()
        .expect("connected to nonsense");
    assert!(
        matches!(&err, ConnectError::Failed(why) if why.contains("not an address")),
        "{err:?}"
    );
}

#[test]
fn it_says_what_it_is() {
    let info = AntelopeGalaxy32Family.info();
    assert_eq!(info.kind, "antelope-galaxy32");
    assert_eq!(info.label, "Antelope Galaxy32");
    assert!(info.available && info.discovers && info.platforms.is_empty());
    let defaults = AntelopeGalaxy32Family.defaults();
    assert_eq!(defaults.len(), 1);
    assert_eq!(
        (defaults[0].name.as_str(), defaults[0].kind.as_str()),
        ("galaxy32", KIND)
    );
}
