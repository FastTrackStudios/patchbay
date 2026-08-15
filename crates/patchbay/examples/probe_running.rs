//! Interrogate a RUNNING patchbay app over its ws endpoint: what does
//! ITS graph mirror contain? (Diagnoses UI-vs-engine discrepancies.)
//!
//! cargo run -p patchbay --example probe_running [ws://127.0.0.1:4046/vox]

use patchbay::proto::PatchbayServiceClient;

#[tokio::main]
async fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:4046/vox".to_string());
    let link = vox_websocket::WsLink::connect(&url)
        .await
        .expect("ws connect");
    let client: PatchbayServiceClient = vox_core::initiator_on(link)
        .establish()
        .await
        .expect("establish");
    let snap = client.graph().await.expect("graph");
    println!(
        "running app mirror: nodes={} ports={} links={}",
        snap.nodes.len(),
        snap.ports.len(),
        snap.links.len()
    );
    for name in ["REAPER", "Inferno source", "Inferno sink"] {
        match snap.nodes.iter().find(|n| n.name == name) {
            Some(n) => {
                let ports = snap.ports.iter().filter(|p| p.node_id == n.id).count();
                println!(
                    "  {name}: id={} class={:?} ports={ports}",
                    n.id, n.media_class
                );
            }
            None => println!("  {name}: MISSING from app mirror"),
        }
    }
}
