//! The family's `connect`, tested the way any family can test its own:
//! `ConnectCtx::in_memory()` (the core's `test-util` feature), no engine,
//! against the fake TF1. Nothing here scans the network: every case
//! either pins `addr` or seeds the cache with the fake's address.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::fake_tf1::Fake;
use patchbay::family::{ConnectCtx, ConnectError, DeviceFamily};
use patchbay::proto::DeviceConfig;
use patchbay_yamaha_tf::family::{KIND, YamahaTfFamily};

fn tf1(addr: &str) -> DeviceConfig {
    let mut config = DeviceConfig::new("tf1", KIND);
    addr.clone_into(&mut config.addr);
    config
}

#[tokio::test]
async fn a_pinned_console_connects_and_is_named_for_its_config_entry() {
    let fake = Fake::start().await;
    let adapter = YamahaTfFamily
        .connect(tf1(&fake.addr.to_string()), ConnectCtx::in_memory())
        .await
        .unwrap();
    let info = adapter.info();
    assert_eq!(info.vendor, "Yamaha");
    // The TF reports no serial: the id carries the config name.
    assert!(info.id.to_string().ends_with(":tf1"), "{}", info.id);
}

#[tokio::test]
async fn a_cached_address_is_connected_without_a_scan() {
    let fake = Fake::start().await;
    let ctx = ConnectCtx::in_memory();
    ctx.cache().remember("tf1", &fake.addr.to_string());
    YamahaTfFamily.connect(tf1(""), ctx).await.unwrap();
    // The cached address is the probe: one connection, no sweep.
    assert_eq!(fake.state.lock().await.connections, 1);
}

#[tokio::test]
async fn a_pinned_address_that_refuses_is_offline_not_missing() {
    // A port nothing listens on.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let err = YamahaTfFamily
        .connect(tf1(&closed.to_string()), ConnectCtx::in_memory())
        .await
        .err()
        .expect("connected to a closed port");
    assert!(matches!(err, ConnectError::Failed(_)), "{err:?}");
}

#[test]
fn it_says_what_it_is() {
    let info = YamahaTfFamily.info();
    assert_eq!(info.kind, "yamaha-tf");
    assert_eq!(info.label, "Yamaha TF");
    assert!(info.available && info.discovers && info.platforms.is_empty());
    let defaults = YamahaTfFamily.defaults();
    assert_eq!(defaults.len(), 1);
    assert_eq!(
        (defaults[0].name.as_str(), defaults[0].kind.as_str()),
        ("tf1", KIND)
    );
}
