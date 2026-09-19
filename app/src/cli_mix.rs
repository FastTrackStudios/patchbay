//! `patchbay mix …` — Loopback / OBS-style host audio mixes (macOS).
//!
//! A mix sums sources (an app, an input device's channels, all system
//! audio) into outputs (e.g. the **Broadcast** virtual device that
//! Discord / `FaceTime` pick as their microphone, or spare playback
//! channels of an interface that loop back into a DAW).
//!
//! Sources and outputs name things the way people do: apps by name or
//! bundle id (`REAPER`, `com.brave.Browser`), devices by name or uid
//! (`Broadcast`, `Galaxy32`). A channel map follows `@`:
//! `input:Galaxy32@32:0,33:1` takes Galaxy inputs 33–34 into the mix's
//! L/R; `Galaxy32@0:32,1:33` sends the mix to Galaxy outputs 33–34.
//! Maps are 0-based `src:dst` pairs; the default is `0:0,1:1`.
//!
//! Agent contract: `--json` prints one JSON document; writes answer
//! with the engine's view after the change.

use std::fmt::Write as _;
use std::io::Write as _;

use clap::Subcommand;
use patchbay_proto::{
    HostTargets, MixConfig, MixMeters, MixOutputConfig, MixSourceConfig, MixView,
    PatchbayServiceClient, default_map, source_kind,
};

use crate::ok_or_msg;

#[derive(Subcommand)]
pub enum MixCmd {
    /// Saved mixes and whether they're running.
    List,
    /// One mix in detail.
    Show { name: String },
    /// Create (or replace) a mix. Example:
    /// `mix create Discord --source app:REAPER --output Broadcast`
    Create {
        name: String,
        /// `app:<name|bundle>[@map]` (stereo mixdown),
        /// `app:<name>:<output device>[@map]` (what the app plays to that
        /// device, every channel — e.g. `app:REAPER:Galaxy32@32:0,33:1`),
        /// `input:<device>[@map]`, `system[@map]`.
        #[arg(long = "source")]
        sources: Vec<String>,
        /// `<device>[@map]` (mix channel → output channel).
        #[arg(long = "output")]
        outputs: Vec<String>,
        /// Mix channel count.
        #[arg(long, default_value_t = 2)]
        channels: u32,
    },
    /// Add a source to a mix.
    AddSource {
        name: String,
        source: String,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        gain: f64,
    },
    /// Add an output to a mix.
    AddOutput { name: String, output: String },
    /// Remove a source (1-based index, as `show` lists them).
    RemoveSource { name: String, index: usize },
    /// Remove an output (1-based index).
    RemoveOutput { name: String, index: usize },
    /// Set a source's level: `mix level Discord 1 -6`, or `mute` / `unmute`.
    Level {
        name: String,
        /// 1-based source index.
        index: usize,
        /// dB (`-6`), `mute`, or `unmute`.
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Set an output's level (same values as `level`).
    OutputLevel {
        name: String,
        index: usize,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Start a stopped mix.
    Enable { name: String },
    /// Stop a mix but keep it saved.
    Disable { name: String },
    /// Stop and delete a mix.
    Delete { name: String },
    /// Peak levels of running mixes (`--watch` for a live readout).
    Meters {
        #[arg(long)]
        watch: bool,
    },
    /// Apps and devices mixes can use on this machine.
    Targets,
}

fn print_json<T: serde::Serialize>(v: &T) -> eyre::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Split `target@map`.
fn split_map(s: &str) -> (&str, String) {
    s.split_once('@')
        .map_or_else(|| (s, default_map()), |(t, m)| (t, m.to_owned()))
}

fn resolve_app(targets: &HostTargets, q: &str) -> eyre::Result<String> {
    let ql = q.to_lowercase();
    if let Some(a) = targets
        .apps
        .iter()
        .find(|a| a.bundle_id == q || a.name.to_lowercase() == ql)
    {
        return Ok(a.bundle_id.clone());
    }
    let hits: Vec<_> = targets
        .apps
        .iter()
        .filter(|a| a.bundle_id.to_lowercase().contains(&ql) || a.name.to_lowercase().contains(&ql))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.bundle_id.clone()),
        // Not running now: take it as a bundle id (the mix waits for it).
        [] if q.contains('.') => Ok(q.to_owned()),
        [] => eyre::bail!(
            "no running app matches '{q}' — start it, or give its bundle id (e.g. com.cockos.reaper)"
        ),
        many => eyre::bail!(
            "'{q}' matches {}: {}",
            many.len(),
            many.iter()
                .map(|a| format!("{} ({})", a.name, a.bundle_id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn resolve_device(targets: &HostTargets, q: &str, output: bool) -> eyre::Result<String> {
    let ql = q.to_lowercase();
    let usable = |d: &&patchbay_proto::HostDevice| {
        if output {
            d.output_channels > 0
        } else {
            d.input_channels > 0
        }
    };
    if let Some(d) = targets
        .devices
        .iter()
        .filter(usable)
        .find(|d| d.uid == q || d.name.to_lowercase() == ql)
    {
        return Ok(d.uid.clone());
    }
    let hits: Vec<_> = targets
        .devices
        .iter()
        .filter(usable)
        .filter(|d| d.name.to_lowercase().contains(&ql) || d.uid.to_lowercase().contains(&ql))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.uid.clone()),
        [] => eyre::bail!(
            "no {} device matches '{q}' (see `patchbay mix targets`)",
            if output { "output" } else { "input" }
        ),
        many => eyre::bail!(
            "'{q}' matches {}: {}",
            many.len(),
            many.iter()
                .map(|d| format!("{} ({})", d.name, d.uid))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn parse_source(targets: &HostTargets, spec: &str, gain_db: f64) -> eyre::Result<MixSourceConfig> {
    let (head, map) = split_map(spec);
    let (kind, rest) = head.split_once(':').unwrap_or((head, ""));
    let (kind, target) = match kind {
        source_kind::APP => {
            // `app:REAPER` (stereo mixdown) or `app:REAPER:Galaxy32` (what
            // REAPER plays to the Galaxy32, every channel — pick with @map).
            let (app, device) = rest.split_once(':').unwrap_or((rest, ""));
            let device = if device.is_empty() {
                String::new()
            } else {
                resolve_device(targets, device, true)?
            };
            return Ok(MixSourceConfig {
                kind: kind.to_owned(),
                target: resolve_app(targets, app)?,
                device,
                map,
                gain_db,
                muted: false,
            });
        }
        source_kind::INPUT => (kind, resolve_device(targets, rest, false)?),
        source_kind::SYSTEM => (kind, String::new()),
        other => eyre::bail!("source kind '{other}': use app:<name>, input:<device> or system"),
    };
    Ok(MixSourceConfig {
        kind: kind.to_owned(),
        target,
        device: String::new(),
        map,
        gain_db,
        muted: false,
    })
}

fn parse_output(targets: &HostTargets, spec: &str) -> eyre::Result<MixOutputConfig> {
    let (device, map) = split_map(spec);
    Ok(MixOutputConfig {
        device: resolve_device(targets, device, true)?,
        map,
        gain_db: 0.0,
        muted: false,
    })
}

/// `-6` / `-6dB` → (dB, unmuted); `mute` / `unmute` keep the level.
fn parse_level(value: &str, current_db: f64) -> eyre::Result<(f64, bool)> {
    match value.trim().to_lowercase().as_str() {
        "mute" | "off" => Ok((current_db, true)),
        "unmute" | "on" => Ok((current_db, false)),
        v => {
            let db: f64 = v
                .trim_end_matches("db")
                .trim()
                .parse()
                .map_err(|_| eyre::eyre!("level '{value}': a dB number, mute or unmute"))?;
            Ok((db, false))
        }
    }
}

fn describe(targets: Option<&HostTargets>, kind: &str, target: &str) -> String {
    let name = targets.and_then(|t| match kind {
        source_kind::APP => t
            .apps
            .iter()
            .find(|a| a.bundle_id == target)
            .map(|a| a.name.clone()),
        _ => t
            .devices
            .iter()
            .find(|d| d.uid == target)
            .map(|d| d.name.clone()),
    });
    match kind {
        source_kind::SYSTEM => "system audio".to_owned(),
        _ => name.unwrap_or_else(|| target.to_owned()),
    }
}

fn print_mix(v: &MixView, targets: Option<&HostTargets>) {
    let state = if !v.config.is_enabled() {
        "disabled".to_owned()
    } else if v.running {
        "running".to_owned()
    } else {
        format!("stopped: {}", v.error)
    };
    println!("{}  [{state}]  {} ch", v.config.name, v.config.channels);
    for (i, s) in v.config.sources.iter().enumerate() {
        let st = v.sources.get(i);
        let live = match st {
            Some(st) if st.active => "live".to_owned(),
            Some(st) => format!("waiting: {}", st.reason),
            None => String::new(),
        };
        println!(
            "  source {}: {:<6} {:<28} @{:<12} {:>6.1} dB{}  {live}",
            i.saturating_add(1),
            s.kind,
            if s.device.is_empty() {
                describe(targets, &s.kind, &s.target)
            } else {
                format!(
                    "{} → {}",
                    describe(targets, &s.kind, &s.target),
                    describe(targets, "output", &s.device)
                )
            },
            s.map,
            s.gain_db,
            if s.muted { " MUTED" } else { "" },
        );
    }
    for (i, o) in v.config.outputs.iter().enumerate() {
        let clock = v.outputs.get(i).is_some_and(|o| o.clock);
        println!(
            "  output {}: {:<35} @{:<12} {:>6.1} dB{}{}",
            i.saturating_add(1),
            describe(targets, "output", &o.device),
            o.map,
            o.gain_db,
            if o.muted { " MUTED" } else { "" },
            if clock { "  (clock)" } else { "" },
        );
    }
}

async fn find(c: &PatchbayServiceClient, name: &str) -> eyre::Result<MixView> {
    ok_or_msg(c.list_mixes().await)?
        .into_iter()
        .find(|m| m.config.name == name)
        .ok_or_else(|| eyre::eyre!("no mix '{name}' (see `patchbay mix list`)"))
}

async fn save(
    c: &PatchbayServiceClient,
    cfg: MixConfig,
    json: bool,
    targets: Option<&HostTargets>,
) -> eyre::Result<()> {
    let v = ok_or_msg(c.save_mix(cfg).await)?;
    if json {
        return print_json(&v);
    }
    print_mix(&v, targets);
    Ok(())
}

fn index0(index: usize) -> eyre::Result<usize> {
    index
        .checked_sub(1)
        .ok_or_else(|| eyre::eyre!("indices are 1-based"))
}

fn meter_bar(p: f32) -> String {
    let db = if p > 0.0 { 20.0 * p.log10() } else { -120.0 };
    let filled = ((db + 60.0) / 3.0).clamp(0.0, 20.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::as_conversions
    )]
    let n = filled as usize;
    format!(
        "{}{} {db:>6.1}",
        "█".repeat(n),
        "·".repeat(20_usize.saturating_sub(n))
    )
}

fn print_meters(meters: &[MixMeters]) {
    for m in meters {
        println!("{}", m.name);
        for (i, s) in m.sources.iter().enumerate() {
            let peak = s.iter().copied().fold(0.0_f32, f32::max);
            println!("  source {}  {}", i.saturating_add(1), meter_bar(peak));
        }
        for (i, o) in m.outputs.iter().enumerate() {
            let peak = o.iter().copied().fold(0.0_f32, f32::max);
            println!("  output {}  {}", i.saturating_add(1), meter_bar(peak));
        }
    }
}

/// Run one `mix` subcommand.
///
/// # Errors
/// RPC failures, unknown mixes, unresolvable apps/devices.
#[allow(clippy::too_many_lines)]
pub async fn run(c: &PatchbayServiceClient, cmd: MixCmd, json: bool) -> eyre::Result<()> {
    let targets = || async { ok_or_msg(c.host_targets().await) };
    match cmd {
        MixCmd::List => {
            let mixes = ok_or_msg(c.list_mixes().await)?;
            if json {
                return print_json(&mixes);
            }
            let t = targets().await.ok();
            if mixes.is_empty() {
                println!(
                    "no mixes (create one: patchbay mix create Discord --source app:REAPER --output Broadcast)"
                );
            }
            for m in &mixes {
                print_mix(m, t.as_ref());
            }
        }
        MixCmd::Show { name } => {
            let m = find(c, &name).await?;
            if json {
                return print_json(&m);
            }
            print_mix(&m, targets().await.ok().as_ref());
        }
        MixCmd::Create {
            name,
            sources,
            outputs,
            channels,
        } => {
            let t = targets().await?;
            let cfg = MixConfig {
                name,
                channels,
                sources: sources
                    .iter()
                    .map(|s| parse_source(&t, s, 0.0))
                    .collect::<eyre::Result<_>>()?,
                outputs: outputs
                    .iter()
                    .map(|o| parse_output(&t, o))
                    .collect::<eyre::Result<_>>()?,
                enabled: None,
            };
            save(c, cfg, json, Some(&t)).await?;
        }
        MixCmd::AddSource { name, source, gain } => {
            let t = targets().await?;
            let mut cfg = find(c, &name).await?.config;
            cfg.sources.push(parse_source(&t, &source, gain)?);
            save(c, cfg, json, Some(&t)).await?;
        }
        MixCmd::AddOutput { name, output } => {
            let t = targets().await?;
            let mut cfg = find(c, &name).await?.config;
            cfg.outputs.push(parse_output(&t, &output)?);
            save(c, cfg, json, Some(&t)).await?;
        }
        MixCmd::RemoveSource { name, index } => {
            let mut cfg = find(c, &name).await?.config;
            let i = index0(index)?;
            eyre::ensure!(i < cfg.sources.len(), "no source {index}");
            cfg.sources.remove(i);
            save(c, cfg, json, targets().await.ok().as_ref()).await?;
        }
        MixCmd::RemoveOutput { name, index } => {
            let mut cfg = find(c, &name).await?.config;
            let i = index0(index)?;
            eyre::ensure!(i < cfg.outputs.len(), "no output {index}");
            cfg.outputs.remove(i);
            save(c, cfg, json, targets().await.ok().as_ref()).await?;
        }
        MixCmd::Level { name, index, value } => {
            set_level(c, &name, index, &value, false, json).await?;
        }
        MixCmd::OutputLevel { name, index, value } => {
            set_level(c, &name, index, &value, true, json).await?;
        }
        MixCmd::Enable { name } => {
            let mut cfg = find(c, &name).await?.config;
            cfg.enabled = None;
            save(c, cfg, json, targets().await.ok().as_ref()).await?;
        }
        MixCmd::Disable { name } => {
            let mut cfg = find(c, &name).await?.config;
            cfg.enabled = Some(false);
            save(c, cfg, json, targets().await.ok().as_ref()).await?;
        }
        MixCmd::Delete { name } => {
            ok_or_msg(c.delete_mix(name.clone()).await)?;
            if json {
                return print_json(&serde_json::json!({ "deleted": name }));
            }
            println!("deleted mix '{name}'");
        }
        MixCmd::Meters { watch } => loop {
            let m = ok_or_msg(c.mix_meters().await)?;
            if json {
                println!("{}", serde_json::to_string(&m)?);
            } else {
                if watch {
                    print!("\x1b[2J\x1b[H");
                }
                print_meters(&m);
            }
            if !watch {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        },
        MixCmd::Targets => {
            let t = targets().await?;
            if json {
                return print_json(&t);
            }
            if !t.supported {
                println!("mixes aren't supported on this host (macOS only)");
            }
            println!("apps:");
            for a in &t.apps {
                println!(
                    "  {:<28} {:<40} {}",
                    a.name,
                    a.bundle_id,
                    if a.playing { "playing" } else { "" }
                );
            }
            println!("devices:");
            for d in &t.devices {
                println!(
                    "  {:<32} in {:>3}  out {:>3}   {}",
                    d.name, d.input_channels, d.output_channels, d.uid
                );
            }
        }
    }
    Ok(())
}

/// `level` / `output-level`: live change, saved.
async fn set_level(
    c: &PatchbayServiceClient,
    name: &str,
    index: usize,
    value: &str,
    output: bool,
    json: bool,
) -> eyre::Result<()> {
    let cfg = find(c, name).await?.config;
    let i = index0(index)?;
    let current = if output {
        cfg.outputs.get(i).map(|o| o.gain_db)
    } else {
        cfg.sources.get(i).map(|s| s.gain_db)
    }
    .ok_or_else(|| eyre::eyre!("no {} {index}", if output { "output" } else { "source" }))?;
    let (db, muted) = parse_level(value, current)?;
    let idx = u32::try_from(i)?;
    if output {
        ok_or_msg(c.set_mix_output(name.to_owned(), idx, db, muted).await)?;
    } else {
        ok_or_msg(c.set_mix_source(name.to_owned(), idx, db, muted).await)?;
    }
    if json {
        return print_json(&serde_json::json!({
            "mix": name, "output": output, "index": index, "gain_db": db, "muted": muted
        }));
    }
    println!(
        "{name} {} {index}: {db:.1} dB{}",
        if output { "output" } else { "source" },
        if muted { " (muted)" } else { "" }
    );
    Ok(())
}

/// `patchbay virtual …` — Patchbay's virtual audio devices (macOS).
#[derive(Subcommand)]
pub enum VirtualCmd {
    /// Virtual devices and whether the driver is loaded.
    List,
    /// Create a device, e.g. `virtual create "Stream Mix" --channels 2`.
    Create {
        name: String,
        #[arg(long, default_value_t = 2)]
        channels: u32,
    },
    /// Rename a device (by uid or name); apps keep their selection.
    Rename { device: String, name: String },
    /// Remove a device (by uid or name).
    Remove { device: String },
    /// Public aggregate devices (e.g. Galaxy32 + Patchbay for a DAW that
    /// opens one device): `aggregate list | create <name> <dev> <dev>… |
    /// remove <name|uid>`.
    Aggregate {
        #[command(subcommand)]
        cmd: AggregateCmd,
    },
}

#[derive(Subcommand)]
pub enum AggregateCmd {
    /// Aggregates Patchbay made.
    List,
    /// Create one: `aggregate create "REAPER I/O" Galaxy32 Patchbay`
    /// (the first device clocks it; devices by name or uid).
    Create {
        name: String,
        #[arg(required = true, num_args = 2..)]
        devices: Vec<String>,
    },
    /// Remove one (by name or uid).
    Remove { aggregate: String },
}

/// Run one `virtual` subcommand.
///
/// # Errors
/// RPC failures; driver missing; unknown device.
pub async fn run_virtual(
    c: &PatchbayServiceClient,
    cmd: VirtualCmd,
    json: bool,
) -> eyre::Result<()> {
    match cmd {
        VirtualCmd::List => {
            let st = ok_or_msg(c.virtual_devices().await)?;
            if json {
                return print_json(&st);
            }
            if !st.driver_loaded {
                println!(
                    "Patchbay.driver is not loaded (install: packaging/macos/install-driver.sh)"
                );
            }
            for d in &st.devices {
                println!("{:<28} {:>3} ch   {}", d.name, d.channels, d.uid);
            }
        }
        VirtualCmd::Create { name, channels } => {
            let d = ok_or_msg(c.create_virtual_device(name, channels).await)?;
            if json {
                return print_json(&d);
            }
            println!("created '{}' ({} ch, uid {})", d.name, d.channels, d.uid);
        }
        VirtualCmd::Rename { device, name } => {
            ok_or_msg(c.rename_virtual_device(device.clone(), name.clone()).await)?;
            if json {
                return print_json(&serde_json::json!({ "renamed": device, "name": name }));
            }
            println!("renamed '{device}' → '{name}'");
        }
        VirtualCmd::Remove { device } => {
            ok_or_msg(c.remove_virtual_device(device.clone()).await)?;
            if json {
                return print_json(&serde_json::json!({ "removed": device }));
            }
            println!("removed '{device}'");
        }
        VirtualCmd::Aggregate { cmd } => run_aggregate(c, cmd, json).await?,
    }
    Ok(())
}

async fn run_aggregate(
    c: &PatchbayServiceClient,
    cmd: AggregateCmd,
    json: bool,
) -> eyre::Result<()> {
    match cmd {
        AggregateCmd::List => {
            let list = ok_or_msg(c.aggregates().await)?;
            if json {
                return print_json(&list);
            }
            if list.is_empty() {
                println!("no Patchbay aggregates");
            }
            for a in &list {
                println!(
                    "{:<28} in {:>3}  out {:>3}   {}",
                    a.name, a.input_channels, a.output_channels, a.uid
                );
            }
        }
        AggregateCmd::Create { name, devices } => {
            let t = ok_or_msg(c.host_targets().await)?;
            let uids = devices
                .iter()
                .map(|d| {
                    let dl = d.to_lowercase();
                    t.devices
                        .iter()
                        .find(|x| x.uid == *d || x.name.to_lowercase() == dl)
                        .map(|x| x.uid.clone())
                        .ok_or_else(|| eyre::eyre!("no device '{d}' (see `patchbay mix targets`)"))
                })
                .collect::<eyre::Result<Vec<_>>>()?;
            let a = ok_or_msg(c.create_aggregate(name, uids).await)?;
            if json {
                return print_json(&a);
            }
            println!(
                "created aggregate '{}' (in {}, out {}) — pick it as the DAW's device",
                a.name, a.input_channels, a.output_channels
            );
        }
        AggregateCmd::Remove { aggregate } => {
            let list = ok_or_msg(c.aggregates().await)?;
            let q = aggregate.to_lowercase();
            let uid = list
                .iter()
                .find(|a| a.uid == aggregate || a.name.to_lowercase() == q)
                .map(|a| a.uid.clone())
                .ok_or_else(|| eyre::eyre!("no Patchbay aggregate '{aggregate}'"))?;
            ok_or_msg(c.remove_aggregate(uid).await)?;
            if json {
                return print_json(&serde_json::json!({ "removed": aggregate }));
            }
            println!("removed aggregate '{aggregate}'");
        }
    }
    Ok(())
}

/// `patchbay now` — the dashboard as text.
///
/// # Errors
/// When the RPC fails.
pub async fn run_now(c: &PatchbayServiceClient, all: bool, json: bool) -> eyre::Result<()> {
    let o = ok_or_msg(c.host_overview().await)?;
    if json {
        return print_json(&o);
    }
    if !o.supported {
        println!("host audio needs macOS (on Linux the PipeWire graph is the router)");
        return Ok(());
    }
    for p in &o.problems {
        let mark = if p.severity == "error" { "!!" } else { " !" };
        println!("{mark} {}", p.summary);
        if !p.detail.is_empty() {
            println!("   {}", p.detail);
        }
    }
    if !o.problems.is_empty() {
        println!();
    }

    let name_of = |uid: &str| {
        o.devices
            .iter()
            .find(|d| d.uid == uid)
            .map_or_else(|| uid.to_owned(), |d| d.name.clone())
    };
    let shown: Vec<&patchbay_proto::HostApp> = o
        .apps
        .iter()
        .filter(|a| all || a.playing || a.recording)
        .collect();
    println!("APPS");
    if shown.is_empty() {
        println!("  (nothing is playing — `--all` lists apps holding an audio client)");
    }
    for a in shown {
        let mark = if a.playing { "*" } else { " " };
        let mut where_to: Vec<String> = a.output_devices.iter().map(|u| name_of(u)).collect();
        for uid in &a.input_devices {
            where_to.push(format!("<- {}", name_of(uid)));
        }
        let dest = if where_to.is_empty() {
            String::new()
        } else {
            format!("  -> {}", where_to.join(", "))
        };
        println!("  {mark} {}{dest}", a.name);
    }

    println!("\nDEVICES");
    for d in &o.devices {
        let mut tags = Vec::new();
        if d.is_default_output() {
            tags.push("default out");
        }
        if d.is_default_input() {
            tags.push("default in");
        }
        if d.in_use {
            tags.push("in use");
        }
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        };
        println!(
            "  {}  {} in / {} out  {}{tags}",
            d.name, d.input_channels, d.output_channels, d.kind
        );
    }

    if !o.virtual_devices.devices.is_empty() {
        println!("\nVIRTUAL DEVICES (Patchbay.driver)");
        for v in &o.virtual_devices.devices {
            println!("  {}  {} ch", v.name, v.channels);
        }
    }
    if !o.aggregates.is_empty() {
        println!("\nAGGREGATES");
        for a in &o.aggregates {
            println!(
                "  {}  {} in / {} out",
                a.name, a.input_channels, a.output_channels
            );
        }
    }

    println!("\nMIXES");
    if o.mixes.is_empty() {
        println!("  (none — `patchbay mix new <name>`)");
    }
    for m in &o.mixes {
        print_mix(m, None);
    }
    Ok(())
}

/// `patchbay now --watch` — live app levels.
///
/// # Errors
/// When the RPC fails.
pub async fn watch_now(c: &PatchbayServiceClient, all: bool, seconds: u64) -> eyre::Result<()> {
    const TICK_MS: u64 = 250;
    /// Matches the UI's meter floor.
    const FLOOR_DB: f64 = -60.0;
    let ticks = seconds
        .saturating_mul(1000)
        .checked_div(TICK_MS)
        .unwrap_or(0);
    for _ in 0..ticks.max(1) {
        let (o, meters) = (
            ok_or_msg(c.host_overview().await)?,
            ok_or_msg(c.app_meters().await)?,
        );
        let mut line = String::new();
        for a in o.apps.iter().filter(|a| all || a.playing || a.recording) {
            let peak = meters
                .iter()
                .find(|m| m.bundle_id == a.bundle_id)
                .map_or(0.0, |m| m.peak);
            // Below the meter floor is silence, not a number worth
            // reading: a tap that is running but idle reports a
            // denormal, which would print as "-168.6".
            let db = f64::from(peak).log10() * 20.0;
            let db = if peak > 0.0 && db > FLOOR_DB {
                format!("{db:6.1}")
            } else {
                "     -".to_owned()
            };
            let _ = write!(line, "{} {db}  ", a.name);
        }
        // One rewritten line: a watcher reads levels, not a scrollback.
        print!("\r\x1b[2K{line}");
        let _ = std::io::stdout().flush();
        tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
    }
    println!();
    Ok(())
}

/// `patchbay listen` — where the RPC and the browser remote listen.
///
/// # Errors
/// When the RPC fails, or the address doesn't parse.
pub async fn run_listen(
    c: &PatchbayServiceClient,
    to: Option<String>,
    json: bool,
) -> eyre::Result<()> {
    let listen = match to.as_deref() {
        None => ok_or_msg(c.listen_address().await)?,
        Some(to) => {
            let bind = match to {
                "lan" | "all" => "0.0.0.0:4046".to_owned(),
                "local" | "loopback" => "127.0.0.1:4046".to_owned(),
                other => other.to_owned(),
            };
            ok_or_msg(c.set_listen_address(bind).await)?
        }
    };
    if json {
        return print_json(&listen);
    }
    println!("listening on: {}", listen.current);
    if listen.configured != listen.current {
        println!(
            "next start:   {}  (restart Patchbay to apply)",
            listen.configured
        );
    }
    if !listen.web_note.is_empty() {
        println!("note: {}", listen.web_note);
    }
    if listen.lan {
        println!("\nreachable from other machines on this network:");
        for u in &listen.urls {
            println!("  {u}");
        }
        println!(
            "\nThe RPC is unauthenticated — anything that can reach these can re-route\n\
             this machine's audio and write to the consoles Patchbay is connected to.\n\
             Close it again with `patchbay listen local`."
        );
    } else {
        println!("\nthis machine only — `patchbay listen lan` opens it to the network");
    }
    Ok(())
}
