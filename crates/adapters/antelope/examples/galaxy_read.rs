//! READ-ONLY Galaxy32 smoke: discover → connect → print device info,
//! clock, every routing page and MIXER 1.
//!
//! Sends only the `initialize_format` handshake and `get_*` reads — never
//! a `set_*` call. Safe against a live rig.
//!
//! ```bash
//! cargo run -p patchbay-antelope --example galaxy_read
//! ```

use std::time::Duration;

use patchbay_antelope::tables::{self, OUTPUT_PAGES, SAMPLE_RATES, SYNC_SOURCES};
use patchbay_antelope::{Galaxy32Adapter, RouteSlot, control_endpoints, discover};
use patchbay_device::{DeviceAdapter, ParamValue};

fn slot_label(s: RouteSlot) -> String {
    tables::source_type(s.ty).map_or_else(
        || "  -  ".to_owned(),
        |t| format!("{}:{}", t.id, u16::from(s.ch).saturating_add(1)),
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let announces = discover(Duration::from_millis(1500)).await?;
    println!("== announces");
    for a in &announces {
        println!(
            "  {:<15} {:>5}  {:<32} {} {}",
            a.ip,
            a.port,
            a.service_type,
            a.properties.device_name.as_deref().unwrap_or("-"),
            a.properties.serial_number.as_deref().unwrap_or(""),
        );
    }
    let candidates = control_endpoints(&announces, None);
    println!(
        "  control candidates (best first): {}",
        candidates
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );

    let adapter = Galaxy32Adapter::discover_and_connect(None, Duration::from_millis(1500)).await?;
    let info = DeviceAdapter::info(&adapter);
    println!("\n== device");
    println!("  id        {}", info.id);
    println!("  model     {} ({})", info.model, info.vendor);
    println!("  serial    {}", info.serial.as_deref().unwrap_or("?"));
    println!("  firmware  {}", info.firmware.as_deref().unwrap_or("?"));
    println!("  transport {:?}", info.transport);

    print_state(&adapter).await;
    print_routing(&adapter).await?;
    print_mixer(&adapter).await?;
    print_afx(&adapter).await?;
    print_snapshot(&adapter).await?;
    Ok(())
}

type Res = Result<(), Box<dyn std::error::Error>>;

async fn print_state(adapter: &Galaxy32Adapter) {
    let dev = adapter.device();
    if let Some(s) = dev.wait_state(Duration::from_secs(2), |_| true).await {
        println!("\n== clock / monitor (cyclic 115)");
        println!(
            "  sample rate {} Hz (index {:?} of {SAMPLE_RATES:?})",
            s.sample_rate,
            tables::sample_rate_index(s.sample_rate)
        );
        println!(
            "  sync source {} = {}   locked {}",
            s.sync_source,
            SYNC_SOURCES.get(usize::from(s.sync_source)).unwrap_or(&"?"),
            s.locked
        );
        println!(
            "  monitor volume {} mute {} dim {} mono {}",
            s.monitor.volume, s.monitor.mute, s.monitor.dim, s.monitor.mono
        );
    }
}

async fn print_routing(adapter: &Galaxy32Adapter) -> Res {
    let dev = adapter.device();
    println!("\n== routing (output ← source, 1-based)");
    for p in &OUTPUT_PAGES {
        let page = dev.get_routing(p.page).await?;
        let cells: Vec<String> = page
            .slots
            .iter()
            .take(usize::from(p.channels))
            .map(|s| slot_label(*s))
            .collect();
        println!("  [{:>2}] {:<16} {}", p.page, p.name, cells.join(" "));
    }
    Ok(())
}

async fn print_mixer(adapter: &Galaxy32Adapter) -> Res {
    let dev = adapter.device();
    println!("\n== MIXER 1 (level/send = dB, pan -1..1)");
    let strips = dev.get_mixer(0).await?;
    for (ch, s) in strips.iter().enumerate() {
        let who = if ch == 0 {
            "master".to_owned()
        } else {
            format!("ch {ch:>2}")
        };
        println!(
            "  {who:<7} level {:>4} dB  pan {:>3}  mute {}  solo {}  send {}",
            0i32.saturating_sub(i32::from(s.level)),
            s.pan,
            s.mute,
            s.solo,
            if s.send >= tables::ATTENUATION_OFF {
                "-inf".to_owned()
            } else {
                format!("{} dB", 0i32.saturating_sub(i32::from(s.send)))
            }
        );
    }
    Ok(())
}

async fn print_afx(adapter: &Galaxy32Adapter) -> Res {
    let dev = adapter.device();
    println!("\n== AFX chains (non-empty)");
    for strip in 0..tables::AFX_STRIPS {
        let chain = dev.get_afx_strip(strip).await?;
        if chain.is_empty() {
            continue;
        }
        let names: Vec<String> = chain
            .iter()
            .map(|s| {
                let n = adapter
                    .afx_catalog()
                    .by_type(s.effect)
                    .map_or_else(|| format!("type{}", s.effect), |e| e.name.clone());
                format!("{n}#{}", s.inst)
            })
            .collect();
        println!(
            "  AFX {:>2}: {}",
            u16::from(strip).saturating_add(1),
            names.join(" → ")
        );
    }
    Ok(())
}

async fn print_snapshot(adapter: &Galaxy32Adapter) -> Res {
    let snap = adapter.snapshot().await?;
    println!(
        "\n== generic snapshot: {} inputs, {} outputs, {} crosspoints, {} params",
        snap.inputs.len(),
        snap.outputs.len(),
        snap.routes.len(),
        snap.params.len()
    );
    for path in [
        "clock/sample_rate",
        "clock/sync_source",
        "monitor/dim",
        "trim/line_in/control",
        "mixer/1/reverb/on",
    ] {
        if let Some(p) = snap.param(path) {
            let v = match (&p.value, &p.kind) {
                (ParamValue::Enum(i), patchbay_device::ParamKind::Enum { options }) => {
                    format!(
                        "{i} ({})",
                        options
                            .get(usize::try_from(*i)?)
                            .map_or("?", String::as_str)
                    )
                }
                (v, _) => format!("{v:?}"),
            };
            println!(
                "  {path:<22} {v}{}",
                if p.disruptive { "  [disruptive]" } else { "" }
            );
        }
    }
    Ok(())
}
