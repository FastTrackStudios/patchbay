//! Time TF console discovery on this machine's networks. Read-only: it
//! sends only `devinfo productname` to open RCP ports.
//!
//! Run: `cargo run -p patchbay-yamaha --example tf_discover`.

use std::time::Instant;

use patchbay_yamaha::{
    ScanOptions, discover_consoles, local_networks, neighbors, plan_stages, scan_targets,
};

#[tokio::main]
async fn main() {
    let opts = ScanOptions::default();
    let nets = local_networks();
    let sweep = scan_targets(&nets, opts.max_hosts);
    let [yamaha, others, rest] = plan_stages(&neighbors().await, &sweep);
    println!(
        "networks {} | stages: yamaha-oui {} · neighbors {} · sweep {}",
        nets.len(),
        yamaha.len(),
        others.len(),
        rest.len()
    );
    let started = Instant::now();
    let found = discover_consoles(&opts).await;
    println!("found {found:?} in {:?}", started.elapsed());
}
