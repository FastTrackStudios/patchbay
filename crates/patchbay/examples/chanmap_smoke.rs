//! Live chanmap→alias→chanmap smoke against the running PipeWire graph.
//!
//! PATCHBAY_CONFIG=<scratch> cargo run -p patchbay --example chanmap_smoke [node]
//!
//! Imports the host's default ChanMap onto `node` (default
//! "Inferno source"), prints a few aliases, then exports them to a
//! scratch chanmap and reports the count.

use patchbay::proto::PatchbayService as _;

#[tokio::main]
async fn main() {
    assert!(
        std::env::var("PATCHBAY_CONFIG").is_ok(),
        "set PATCHBAY_CONFIG to a scratch path — this smoke writes aliases"
    );
    let node = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Inferno source".to_string());

    let backend = patchbay::PatchbayBackend::new();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let imported = backend
        .import_chanmap(node.clone(), String::new())
        .await
        .expect("import_chanmap");
    println!("imported {imported} aliases onto '{node}'");

    let aliases = backend.aliases().await.expect("aliases");
    for a in aliases.iter().take(8) {
        println!("  {} -> {}", a.target, a.alias);
    }

    let out = std::env::temp_dir().join("patchbay-smoke-export.ReaperChanMap");
    let exported = backend
        .export_chanmap(node.clone(), out.to_string_lossy().into_owned())
        .await
        .expect("export_chanmap");
    println!("exported {exported} names to {}", out.display());
}
