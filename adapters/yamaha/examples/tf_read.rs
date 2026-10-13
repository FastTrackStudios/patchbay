//! READ-ONLY Yamaha TF smoke: connect → print device info, the current
//! scene and every channel's name / colour / icon / level / on.
//!
//! Sends only `devinfo`, `prminfo`, `get`, `sscurrent_ex`, `ssinfo_ex` and
//! `devstatus runmode` pings. It does **not** send `scpmode keepalive`
//! (a session setting) and never `set`s anything. Safe against a live show.
//!
//! ```bash
//! cargo run -p patchbay-yamaha --example tf_read -- 192.168.1.214:49280
//! ```

use std::net::SocketAddr;
use std::time::Instant;

use patchbay_device::{DeviceAdapter, DeviceSnapshot, ParamKind, ParamValue};
use patchbay_yamaha::{ClientOptions, RCP_PORT, TfAdapter, TfOptions};

fn show(snap: &DeviceSnapshot, path: &str) -> String {
    let Some(p) = snap.param(path) else {
        return "-".to_owned();
    };
    match (&p.value, &p.kind) {
        (ParamValue::Level(db), _) if db.is_infinite() => "-inf".to_owned(),
        (ParamValue::Level(db), _) => format!("{db:+.2}"),
        (ParamValue::Toggle(b), _) => if *b { "ON" } else { "off" }.to_owned(),
        (ParamValue::Enum(i), ParamKind::Enum { options }) => usize::try_from(*i)
            .ok()
            .and_then(|i| options.get(i))
            .cloned()
            .unwrap_or_else(|| format!("#{i}")),
        (ParamValue::Text(s), _) => format!("{s:?}"),
        (v, _) => format!("{v:?}"),
    }
}

fn print_block(snap: &DeviceSnapshot, title: &str, prefix: &str, instances: &[String]) {
    println!("\n== {title}");
    println!(
        "  {:<10} {:<22} {:<9} {:<13} {:<9} {:>8}  on",
        "path", "name", "color", "icon", "category", "level"
    );
    for inst in instances {
        let base = if inst.is_empty() {
            prefix.to_owned()
        } else {
            format!("{prefix}/{inst}")
        };
        if snap.param(&format!("{base}/level")).is_none()
            && snap.param(&format!("{base}/on")).is_none()
        {
            continue;
        }
        println!(
            "  {:<10} {:<22} {:<9} {:<13} {:<9} {:>8}  {}",
            base,
            show(snap, &format!("{base}/name")),
            show(snap, &format!("{base}/color")),
            show(snap, &format!("{base}/icon")),
            show(snap, &format!("{base}/category")),
            show(snap, &format!("{base}/level")),
            show(snap, &format!("{base}/on")),
        );
    }
}

fn nums(n: u16) -> Vec<String> {
    (1..=n).map(|i| i.to_string()).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.1.214".to_owned());
    let addr: SocketAddr = arg
        .parse()
        .or_else(|_| format!("{arg}:{RCP_PORT}").parse())?;

    let opts = TfOptions {
        client: ClientOptions {
            // Leave the console's session keepalive setting alone.
            keepalive: None,
            reconnect: false,
            ..ClientOptions::default()
        },
        ..TfOptions::default()
    };
    let t0 = Instant::now();
    let adapter = TfAdapter::connect_with(addr, opts).await?;
    let elapsed = t0.elapsed();

    let info = DeviceAdapter::info(&adapter);
    println!("== device");
    println!("  id        {}", info.id);
    println!("  model     {} ({})", info.model, info.vendor);
    println!("  firmware  {}", info.firmware.as_deref().unwrap_or("?"));
    println!(
        "  serial    {}",
        info.serial.as_deref().unwrap_or("(none reported)")
    );
    println!("  transport {:?}", info.transport);
    let table = adapter.table();
    println!(
        "  params    {} mapped ({} prminfo rows skipped: {})",
        table.len(),
        table.skipped().len(),
        table.skipped().join(", ")
    );

    let snap = adapter.cached_snapshot();
    println!(
        "  read      {} values in {:.2?} (connect + prminfo + full read)",
        snap.params.len(),
        elapsed
    );
    println!(
        "  routing   {} input groups, {} output groups, {} crosspoints (none over RCP)",
        snap.inputs.len(),
        snap.outputs.len(),
        snap.routes.len()
    );

    println!("\n== scene");
    for p in ["scene/current", "scene/title", "scene/modified"] {
        println!("  {p:<15} {}", show(&snap, p));
    }

    print_block(&snap, "input channels", "in", &nums(32));
    print_block(&snap, "stereo inputs", "stin", &nums(4));
    print_block(&snap, "fx returns", "fxrtn", &nums(4));
    print_block(&snap, "aux", "aux", &nums(20));
    print_block(&snap, "matrix", "matrix", &nums(4));
    print_block(&snap, "stereo", "stereo", &["l".to_owned(), "r".to_owned()]);
    print_block(&snap, "sub", "sub", &[String::new()]);
    print_block(&snap, "dca", "dca", &nums(8));
    println!("\n== mute groups");
    for i in 1..=6 {
        println!(
            "  mutegroup/{i}  {:<10} {}",
            show(&snap, &format!("mutegroup/{i}/name")),
            show(&snap, &format!("mutegroup/{i}/on"))
        );
    }
    adapter.client().close().await;
    Ok(())
}
