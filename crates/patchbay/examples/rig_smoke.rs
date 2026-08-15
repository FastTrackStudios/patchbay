//! Read-only smoke for the rig-health + Dante surfaces:
//! service states, then an mDNS + ARC scan of the Dante network.
//!
//! `cargo run -p patchbay --example rig_smoke`

use patchbay::proto::PatchbayService as _;

#[tokio::main]
async fn main() {
    let backend = patchbay::PatchbayBackend::new();

    let services = backend.services().await.expect("services");
    println!("services:");
    for s in &services {
        println!(
            "  [{}] {:<28} {}/{}",
            if !s.present {
                "?"
            } else if s.state == "active" {
                "+"
            } else {
                "-"
            },
            s.label,
            s.state,
            s.sub_state
        );
    }

    println!("\nscanning dante network…");
    let devices = backend.dante_network().await.expect("dante_network");
    for d in &devices {
        println!(
            "  {} @ {}:{} — {} tx / {} rx, {} subscription(s){}",
            d.name,
            d.ip,
            d.arc_port,
            d.tx.len(),
            d.rx.len(),
            d.subscriptions.len(),
            if d.unreachable {
                " [ARC unreachable]"
            } else {
                ""
            }
        );
        for s in d.subscriptions.iter().take(6) {
            let rx_name =
                d.rx.iter()
                    .find(|c| c.number == s.rx_channel)
                    .map(|c| c.name.as_str())
                    .unwrap_or("?");
            println!(
                "     rx {} ({}) <- {}@{} [status {}]",
                s.rx_channel, rx_name, s.tx_channel, s.tx_device, s.status
            );
        }
    }
}
