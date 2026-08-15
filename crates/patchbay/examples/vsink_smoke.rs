//! Virtual-sink smoke: create a scratch bus, verify it lands in the
//! graph tagged `patchbay.virtual`, then remove it again.
//!
//! Uses a scratch config (never the real one):
//! `PATCHBAY_CONFIG=/tmp/pb-vsink.json cargo run -p patchbay --example vsink_smoke`

use patchbay::PatchbayBackend;
use patchbay::proto::{PatchbayService as _, VirtualSink, sink_node_name};

#[tokio::main]
async fn main() {
    assert!(
        std::env::var("PATCHBAY_CONFIG").is_ok(),
        "set PATCHBAY_CONFIG to a scratch path — this example writes config"
    );
    let backend = PatchbayBackend::new();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let name = "Vsink Smoke Test";
    backend
        .add_virtual_sink(VirtualSink {
            name: name.into(),
            channels: 2,
        })
        .await
        .expect("add_virtual_sink");

    // The adapter takes a moment to appear in the registry.
    let node_name = sink_node_name(name);
    let mut found = false;
    for attempt in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let graph = backend.graph().await.expect("graph");
        if attempt % 4 == 0 {
            println!(
                "poll {attempt}: {} nodes in mirror, looking for {node_name}",
                graph.nodes.len()
            );
        }
        if let Some(n) = graph.nodes.iter().find(|n| n.name == node_name) {
            println!(
                "created: [{}] {} virtual={} ports={}",
                n.id,
                n.name,
                n.virtual_sink,
                graph.ports.iter().filter(|p| p.node_id == n.id).count()
            );
            assert!(n.virtual_sink, "node must carry the patchbay.virtual tag");
            found = true;
            break;
        }
    }
    assert!(found, "virtual sink never appeared in the graph");

    backend
        .remove_virtual_sink(name.into())
        .await
        .expect("remove_virtual_sink");
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let graph = backend.graph().await.expect("graph");
        if !graph.nodes.iter().any(|n| n.name == node_name) {
            println!("removed: {node_name} gone from graph — OK");
            return;
        }
    }
    panic!("virtual sink still in the graph after remove");
}
