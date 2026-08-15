//! 1:1 bulk-connect smoke on two scratch null sinks (creates them via
//! pw-cli, links A's monitors to B's inputs, verifies, tears down).
//!
//! `cargo run -p patchbay --example bulk_smoke`

use std::process::Command;

use patchbay::proto::PatchbayService as _;

fn spawn_sink(name: &str) {
    let args = format!(
        "{{ factory.name=support.null-audio-sink node.name={name} media.class=Audio/Sink \
         audio.channels=4 audio.position=[UNK UNK UNK UNK] object.linger=true }}"
    );
    let st = Command::new("pw-cli")
        .args(["create-node", "adapter", &args])
        .status()
        .expect("pw-cli");
    assert!(st.success(), "create-node {name}");
}

#[tokio::main]
async fn main() {
    let backend = patchbay::PatchbayBackend::new();
    spawn_sink("pbtest_a");
    spawn_sink("pbtest_b");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let created = backend
        .connect_one_to_one("pbtest_a".into(), "pbtest_b".into())
        .await
        .expect("connect_one_to_one");
    println!("created {created} links");
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let snap = backend.graph().await.expect("graph");
    let a = snap.nodes.iter().find(|n| n.name == "pbtest_a").unwrap().id;
    let b = snap.nodes.iter().find(|n| n.name == "pbtest_b").unwrap().id;
    let live = snap
        .links
        .iter()
        .filter(|l| l.output_node == a && l.input_node == b)
        .count();
    println!("live links a->b: {live}");

    let removed = backend
        .disconnect_nodes("pbtest_a".into(), "pbtest_b".into())
        .await
        .expect("disconnect_nodes");
    println!("removed {removed} links");
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // Scratch nodes die with this process (pw-cli create-node proxies
    // are owned by the pw-cli process, which exited) — but they were
    // created with create-node from a short-lived pw-cli, so clean up
    // explicitly by global id to be safe.
    let snap = backend.graph().await.expect("graph2");
    for n in snap.nodes.iter().filter(|n| n.name.starts_with("pbtest_")) {
        let _ = Command::new("pw-cli")
            .args(["destroy", &n.id.to_string()])
            .status();
    }
    println!("cleaned up");
    assert_eq!(created as usize, live, "created links all appeared");
    assert_eq!(created, removed, "removed the same set");
}
