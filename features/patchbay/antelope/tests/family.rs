//! The family on its own, the way any family can test itself:
//! `ConnectCtx::in_memory()` (the core's `test-util` feature), no engine.
//!
//! The real family hears the Manager Server's multicast announces; here
//! it hears a fake [`AnnounceSource`], so nothing binds the announce port
//! or joins its group. `PATCHBAY_LAN=off` besides: if anything did reach
//! the real listener, it would be refused, not sent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::fake_manager::{announce, fake};
use patchbay::family::{ConnectCtx, ConnectError, DeviceFamily};
use patchbay::proto::DeviceConfig;
use patchbay_antelope::family::{AnnounceSource, AntelopeGalaxy32Family};
use patchbay_antelope::{Announce, AntelopeError, KIND};

/// Announces given up front, and how many times they were asked for.
struct FakeAnnounces {
    heard: Vec<Announce>,
    asked: AtomicUsize,
}

impl FakeAnnounces {
    fn of(heard: Vec<Announce>) -> Arc<Self> {
        Arc::new(Self {
            heard,
            asked: AtomicUsize::new(0),
        })
    }
}

impl AnnounceSource for FakeAnnounces {
    fn announces(
        &self,
        _timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Announce>, AntelopeError>> + Send + '_>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let heard = self.heard.clone();
        Box::pin(async move { Ok(heard) })
    }
}

fn lan_off() {
    // SAFETY: first thing in the test, on its runtime's only thread
    // (nextest runs each test in a process of its own).
    unsafe { std::env::set_var("PATCHBAY_LAN", "off") };
}

fn galaxy32(addr: &str) -> DeviceConfig {
    let mut config = DeviceConfig::new("galaxy32", KIND);
    addr.clone_into(&mut config.addr);
    config
}

fn family(source: &Arc<FakeAnnounces>) -> AntelopeGalaxy32Family {
    let source: Arc<FakeAnnounces> = Arc::clone(source);
    let source: Arc<dyn AnnounceSource> = source;
    AntelopeGalaxy32Family::with_announces(source)
}

#[tokio::test]
async fn a_pinned_address_nobody_announces_is_not_found() {
    lan_off();
    let source = FakeAnnounces::of(Vec::new());
    let err = family(&source)
        .connect(galaxy32("127.0.0.1:9"), ConnectCtx::in_memory())
        .await
        .err()
        .expect("connected without an announce");
    assert!(
        matches!(&err, ConnectError::NotFound(why) if why.contains("127.0.0.1:9")),
        "{err:?}"
    );
    assert_eq!(source.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_pinned_address_that_is_not_an_address_fails_before_listening() {
    lan_off();
    let source = FakeAnnounces::of(Vec::new());
    let err = family(&source)
        .connect(galaxy32("not an address"), ConnectCtx::in_memory())
        .await
        .err()
        .expect("connected to nonsense");
    assert!(
        matches!(&err, ConnectError::Failed(why) if why.contains("not an address")),
        "{err:?}"
    );
    assert_eq!(source.asked.load(Ordering::SeqCst), 0, "it listened first");
}

#[tokio::test]
async fn an_announced_galaxy32_connects_found_or_pinned() {
    lan_off();
    let (addr, _state) = fake().await;
    let source = FakeAnnounces::of(vec![announce(addr)]);
    // Found: no address in the config, the announce names it.
    let found = family(&source)
        .connect(galaxy32(""), ConnectCtx::in_memory())
        .await
        .unwrap();
    assert_eq!(
        found.info().id.to_string(),
        "antelope:galaxy32:4202524000109"
    );
    // Pinned to the announced endpoint.
    let pinned = family(&source)
        .connect(galaxy32(&addr.to_string()), ConnectCtx::in_memory())
        .await
        .unwrap();
    assert_eq!(pinned.info().id, found.info().id);
}

#[test]
fn it_says_what_it_is() {
    let info = AntelopeGalaxy32Family::default().info();
    assert_eq!(info.kind, "antelope-galaxy32");
    assert_eq!(info.label, "Antelope Galaxy32");
    assert!(info.available && info.discovers && info.platforms.is_empty());
    let defaults = AntelopeGalaxy32Family::default().defaults();
    assert_eq!(defaults.len(), 1);
    assert_eq!(
        (defaults[0].name.as_str(), defaults[0].kind.as_str()),
        ("galaxy32", KIND)
    );
}
