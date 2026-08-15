//! Latency-rule → WirePlumber drop-in smoke: set a rule, print the
//! generated conf, remove it again (WirePlumber is never restarted, so
//! nothing live changes).
//!
//! PATCHBAY_CONFIG=<scratch> cargo run -p patchbay --example latency_smoke

use patchbay::proto::{LatencyRule, PatchbayService as _};

#[tokio::main]
async fn main() {
    assert!(
        std::env::var("PATCHBAY_CONFIG").is_ok(),
        "set PATCHBAY_CONFIG to a scratch path"
    );
    let backend = patchbay::PatchbayBackend::new();
    let dropin = dirs::config_dir()
        .unwrap()
        .join("wireplumber/wireplumber.conf.d/99-fts-patchbay-latency.conf");

    backend
        .set_latency_rule(LatencyRule {
            pattern: "REAPER".into(),
            quantum: 64,
            force: true,
        })
        .await
        .expect("set rule");
    backend
        .set_latency_rule(LatencyRule {
            pattern: "~Firefox.*".into(),
            quantum: 1024,
            force: false,
        })
        .await
        .expect("set rule 2");

    println!("--- {}:", dropin.display());
    print!(
        "{}",
        std::fs::read_to_string(&dropin).expect("dropin written")
    );

    let rules = backend.latency_rules().await.expect("rules");
    println!("--- {} rule(s) stored", rules.len());

    backend
        .remove_latency_rule("REAPER".into())
        .await
        .expect("rm");
    backend
        .remove_latency_rule("~Firefox.*".into())
        .await
        .expect("rm2");
    println!("--- after removal, dropin exists: {}", dropin.exists());
}
