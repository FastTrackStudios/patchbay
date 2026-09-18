//! `patchbay device …` — external hardware (Antelope Galaxy32, …) over
//! the same `PatchbayService` the UI uses.
//!
//! Agent contract: every command takes `--json` and prints ONE JSON
//! document (`watch` prints one JSON object per line). Writes are always
//! explicit (`set`, `route`, `snapshot restore` without `--dry-run`) and
//! answer with the device's read-back, never with what was requested.
//! Device ids accept the full id, the config name, or a unique
//! substring of id / model / serial (e.g. `galaxy`, `4202524`).

use clap::Subcommand;
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_proto::{
    DeviceChannel, DeviceEventKind, DeviceEventWire, DeviceLinkState, DevicePortGroup,
    DeviceRestoreReport, DeviceRestoreStatus, DeviceSummary, DeviceView, ParamView,
    PatchbayServiceClient, source_label,
};

use crate::ok_or_msg;

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// Configured devices and their link state.
    List,
    /// Full device state: identity, router (per output group), param
    /// counts. `--json` = the complete `DeviceView`.
    Show { id: String },
    /// Params, optionally under a path prefix (whole segments:
    /// `mixer/1/strip/16`, `monitor`, `clock`).
    Params { id: String, prefix: Option<String> },
    /// One param by exact path.
    Get { id: String, path: String },
    /// Write one param, e.g. `set galaxy mixer/1/strip/16/level -12`.
    /// Values: levels in dB (`-12`, `-12dB`, `-inf`), pan `-1..1` or
    /// `L/C/R`, toggles `on/off`, enums by label or index, ints, text.
    Set {
        id: String,
        path: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
        /// Required for params flagged disruptive (clock, sample rate):
        /// they drop audio.
        #[arg(long)]
        allow_disruptive: bool,
    },
    /// Patch a router output: `route <id> <OUTPUT_GROUP:ch> <INPUT_GROUP:ch|none>`.
    /// Channels are 1-based; groups by id (`DIGI_OUT0`) or name
    /// (`"HDX OUT 1-32"`), case-insensitive.
    Route {
        id: String,
        output: String,
        source: String,
    },
    /// Stream this device's events until interrupted (`--json` = one
    /// JSON object per line). `all` watches every device.
    Watch { id: String },
    /// Named device snapshots (params + crosspoints, saved in config).
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
}

#[derive(Subcommand)]
pub enum SnapshotCmd {
    /// Save the device's writable params + crosspoints under `name`
    /// (replaces a snapshot of the same name).
    Save {
        id: String,
        name: String,
        /// Only these path prefixes (`mixer/1/strip/16`, `route/DIGI_OUT0`).
        #[arg(long, num_args = 1..)]
        include: Vec<String>,
        /// Leave these path prefixes out (`afx`, `trim`).
        #[arg(long, num_args = 1..)]
        exclude: Vec<String>,
    },
    List,
    /// What a restore would change now (reads the device, writes nothing).
    Diff {
        name: String,
        /// Narrow to path prefixes.
        #[arg(long, num_args = 1..)]
        only: Vec<String>,
        /// Plan disruptive params too (otherwise listed as skipped).
        #[arg(long)]
        allow_disruptive: bool,
    },
    /// Write the differences back (only what differs from live).
    Restore {
        name: String,
        /// Print the plan, write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Also restore disruptive params (clock, sample rate).
        #[arg(long)]
        allow_disruptive: bool,
        /// Narrow to path prefixes.
        #[arg(long, num_args = 1..)]
        only: Vec<String>,
    },
    Delete {
        name: String,
    },
}

fn print_json<T: serde::Serialize>(v: &T) -> eyre::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// A param as JSON plus `display`: the same human rendering as the text
/// output (enum label, `-inf dB`, …), so agents need not resolve enum
/// indices against `kind.options` themselves.
fn param_json(p: &ParamView) -> eyre::Result<serde_json::Value> {
    let mut v = serde_json::to_value(p)?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "display".to_owned(),
            serde_json::Value::String(p.value.display(Some(&p.kind))),
        );
    }
    Ok(v)
}

const fn state_str(s: DeviceLinkState) -> &'static str {
    match s {
        DeviceLinkState::Connecting => "connecting",
        DeviceLinkState::Online => "online",
        DeviceLinkState::Offline => "offline",
        DeviceLinkState::Disabled => "disabled",
        DeviceLinkState::Searching => "searching",
        DeviceLinkState::NotFound => "not found",
    }
}

fn summary_line(d: &DeviceSummary) -> String {
    use std::fmt::Write as _;
    let id = if d.id.is_empty() {
        "(not yet identified)"
    } else {
        &d.id
    };
    let mut s = format!(
        "{id}  [{}]  name={} kind={}",
        state_str(d.state),
        d.name,
        d.kind
    );
    if !d.model.is_empty() {
        let _ = write!(s, "\n    {} {}", d.vendor, d.model);
        if !d.serial.is_empty() {
            let _ = write!(s, " · serial {}", d.serial);
        }
        if !d.firmware.is_empty() {
            let _ = write!(s, " · fw {}", d.firmware);
        }
    }
    if !d.transport.is_empty() {
        let _ = write!(s, "\n    {}", d.transport);
    }
    if !d.error.is_empty() {
        let _ = write!(s, "\n    error: {}", d.error);
    }
    s
}

fn param_line(p: &ParamView) -> String {
    let mut flags = String::new();
    if !p.writable {
        flags.push_str(" (read-only)");
    }
    if p.disruptive {
        flags.push_str(" (DISRUPTIVE)");
    }
    format!(
        "{:<36} {:<18} {:<6} {}{flags}",
        p.path,
        p.value.display(Some(&p.kind)),
        p.kind.name(),
        p.label
    )
}

/// Resolve a human group reference (id or display name) + 1-based
/// channel against the device's groups.
fn resolve_channel(
    groups: &[DevicePortGroup],
    spec: &str,
    side: &str,
) -> eyre::Result<DeviceChannel> {
    if groups.is_empty() {
        eyre::bail!("this device has no router (params only)");
    }
    let (g, n) = spec
        .trim()
        .rsplit_once(':')
        .ok_or_else(|| eyre::eyre!("'{spec}': expected <{side}-group>:<channel> (1-based)"))?;
    let n: u16 = n
        .trim()
        .parse()
        .map_err(|_| eyre::eyre!("'{spec}': channel must be a number (1-based)"))?;
    let group = groups
        .iter()
        .find(|x| x.id.eq_ignore_ascii_case(g.trim()) || x.name.eq_ignore_ascii_case(g.trim()))
        .ok_or_else(|| {
            eyre::eyre!(
                "no {side} group '{}' (have: {})",
                g.trim(),
                groups
                    .iter()
                    .map(|x| x.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    if n == 0 || n > group.channels {
        eyre::bail!("{} has channels 1..={} (got {n})", group.id, group.channels);
    }
    Ok(DeviceChannel::new(group.id.clone(), n.saturating_sub(1)))
}

/// Resolve a device reference to its full id the way the engine does
/// (exact id / config name, else a unique substring of id / name /
/// model / serial) so output always names the device unambiguously.
async fn resolve_id(c: &PatchbayServiceClient, query: &str) -> eyre::Result<String> {
    let list = ok_or_msg(c.list_devices().await)?;
    let q = query.trim().to_lowercase();
    if let Some(d) = list.iter().find(|d| d.id == query || d.name == query) {
        return Ok(d.id.clone());
    }
    let hits: Vec<&DeviceSummary> = list
        .iter()
        .filter(|d| {
            [&d.id, &d.name, &d.model, &d.serial]
                .iter()
                .any(|f| f.to_lowercase().contains(&q))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => eyre::bail!("no device matches '{query}' (try `patchbay device list`)"),
        many => eyre::bail!("'{query}' matches {} devices; use the full id", many.len()),
    }
}

async fn exact_param(c: &PatchbayServiceClient, id: &str, path: &str) -> eyre::Result<ParamView> {
    let params = ok_or_msg(c.device_params(id.to_owned(), path.to_owned()).await)?;
    params
        .into_iter()
        .find(|p| p.path == path)
        .ok_or_else(|| eyre::eyre!("no param '{path}' on {id} (try `patchbay device params {id}`)"))
}

fn print_report(r: &DeviceRestoreReport, json: bool) -> eyre::Result<()> {
    if json {
        return print_json(r);
    }
    let mode = if r.dry_run {
        "plan (dry run)"
    } else {
        "restore"
    };
    println!(
        "{mode} of '{}' onto {}: {} differing, {} unchanged",
        r.snapshot,
        r.device,
        r.items.len(),
        r.unchanged
    );
    for i in &r.items {
        let status = match i.status {
            DeviceRestoreStatus::Planned => "planned",
            DeviceRestoreStatus::Applied => "applied",
            DeviceRestoreStatus::Failed => "FAILED",
            DeviceRestoreStatus::SkippedDisruptive => "skipped (disruptive; --allow-disruptive)",
            DeviceRestoreStatus::SkippedReadOnly => "skipped (read-only)",
            DeviceRestoreStatus::SkippedMissing => "skipped (not on device)",
        };
        println!("  {:<36} {} -> {}  [{status}]", i.path, i.current, i.target);
        if !i.error.is_empty() {
            println!("      error: {}", i.error);
        }
    }
    Ok(())
}

fn print_show(v: &DeviceView) {
    println!("{}", summary_line(&v.summary));
    println!(
        "router: {} input group(s), {} output group(s), {} crosspoint(s)",
        v.inputs.len(),
        v.outputs.len(),
        v.routes.len()
    );
    for g in &v.outputs {
        let cells: Vec<String> = v
            .routes
            .iter()
            .filter(|r| r.output.group == g.id)
            .map(|r| {
                format!(
                    "{}={}",
                    u32::from(r.output.channel).saturating_add(1),
                    r.source
                        .as_ref()
                        .map_or_else(|| "·".to_owned(), DeviceChannel::label)
                )
            })
            .collect();
        println!("  {:<13} {:<16} {}", g.id, g.name, cells.join(" "));
    }
    let mut sections: Vec<(String, usize)> = Vec::new();
    for p in &v.params {
        let head = p.path.split('/').next().unwrap_or_default().to_owned();
        match sections.iter_mut().find(|(h, _)| *h == head) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => sections.push((head, 1)),
        }
    }
    println!(
        "params: {} ({})",
        v.params.len(),
        sections
            .iter()
            .map(|(h, n)| format!("{h} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "use `patchbay device params {} <prefix>` for values",
        v.summary.name
    );
}

/// With `--local`, device supervisors start with the engine: give the
/// first connect / discovery attempts a moment to finish before
/// answering (Dante's mDNS browse alone takes 8 s).
pub async fn settle_local(c: &PatchbayServiceClient) {
    for _ in 0..75 {
        match c.list_devices().await {
            Ok(list)
                if list.iter().all(|d| {
                    !matches!(
                        d.state,
                        DeviceLinkState::Connecting | DeviceLinkState::Searching
                    )
                }) =>
            {
                return;
            }
            Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
            Err(_) => return,
        }
    }
}

// One arm per subcommand, like the top-level dispatch.
#[allow(clippy::too_many_lines)]
pub async fn run(
    c: &PatchbayServiceClient,
    stream: impl AsyncFnOnce() -> eyre::Result<PatchbayServiceStreamClient>,
    cmd: DeviceCmd,
    json: bool,
) -> eyre::Result<()> {
    match cmd {
        DeviceCmd::List => {
            let list = ok_or_msg(c.list_devices().await)?;
            if json {
                return print_json(&list);
            }
            if list.is_empty() {
                println!("no devices configured (PATCHBAY_DEVICES=off?)");
            }
            for d in &list {
                println!("{}", summary_line(d));
            }
        }
        DeviceCmd::Show { id } => {
            let v = ok_or_msg(c.device(id).await)?;
            if json {
                return print_json(&v);
            }
            print_show(&v);
        }
        DeviceCmd::Params { id, prefix } => {
            let params = ok_or_msg(c.device_params(id, prefix.unwrap_or_default()).await)?;
            if json {
                let out = params
                    .iter()
                    .map(param_json)
                    .collect::<eyre::Result<Vec<_>>>()?;
                return print_json(&out);
            }
            for p in &params {
                println!("{}", param_line(p));
            }
            println!("({} param(s))", params.len());
        }
        DeviceCmd::Get { id, path } => {
            let p = exact_param(c, &id, &path).await?;
            if json {
                return print_json(&param_json(&p)?);
            }
            println!("{}", param_line(&p));
        }
        DeviceCmd::Set {
            id,
            path,
            value,
            allow_disruptive,
        } => {
            let id = resolve_id(c, &id).await?;
            let before = exact_param(c, &id, &path).await?;
            let v = before
                .kind
                .parse_value(&value)
                .map_err(|e| eyre::eyre!("{path}: {e}"))?;
            let after = ok_or_msg(
                c.set_device_param(id.clone(), path.clone(), v.clone(), allow_disruptive)
                    .await,
            )?;
            if json {
                return print_json(&serde_json::json!({
                    "device": id,
                    "path": path,
                    "requested": v,
                    "previous": before.value,
                    "param": after,
                }));
            }
            println!(
                "{path}: {} -> {} (read back from device)",
                before.value.display(Some(&before.kind)),
                after.value.display(Some(&after.kind))
            );
        }
        DeviceCmd::Route { id, output, source } => {
            let v = ok_or_msg(c.device(id.clone()).await)?;
            let out = resolve_channel(&v.outputs, &output, "output")?;
            let src = match source.trim().to_ascii_lowercase().as_str() {
                "none" | "-" | "" => None,
                _ => Some(resolve_channel(&v.inputs, &source, "input")?),
            };
            let previous = v
                .routes
                .iter()
                .find(|r| r.output == out)
                .ok_or_else(|| eyre::eyre!("{} is not a router output on {id}", out.label()))?
                .source
                .clone();
            let cp = ok_or_msg(
                c.set_device_route(v.summary.id.clone(), out.clone(), src)
                    .await,
            )?;
            if json {
                return print_json(&serde_json::json!({
                    "device": v.summary.id,
                    "output": out.label(),
                    "previous": previous.as_ref().map(DeviceChannel::label),
                    "source": cp.source.as_ref().map(DeviceChannel::label),
                    "crosspoint": cp,
                }));
            }
            println!(
                "{}: {} -> {} (read back from device)",
                out.label(),
                source_label(previous.as_ref()),
                source_label(cp.source.as_ref())
            );
        }
        DeviceCmd::Watch { id } => {
            let filter = if id == "all" {
                None
            } else {
                // Events carry the full id; resolve the alias once.
                Some(resolve_id(c, &id).await?)
            };
            let sc = stream().await?;
            let (tx, mut rx) = vox::channel::<DeviceEventWire>();
            tokio::spawn(async move {
                if let Err(e) = sc.device_events(tx).await {
                    eprintln!("device event stream ended: {e:?}");
                }
            });
            if !json {
                eprintln!(
                    "watching {} (ctrl-c to stop)",
                    filter.as_deref().unwrap_or("all devices")
                );
            }
            while let Ok(Some(ev)) = rx.recv().await {
                let ev = ev.get();
                if filter.as_ref().is_some_and(|f| *f != ev.device) {
                    continue;
                }
                if json {
                    println!("{}", serde_json::to_string(ev)?);
                    continue;
                }
                let what = match &ev.event {
                    DeviceEventKind::ParamChanged { path, value } => {
                        format!("param {path} = {}", value.display(None))
                    }
                    DeviceEventKind::RouteChanged { crosspoint } => format!(
                        "route {} <- {}",
                        crosspoint.output.label(),
                        source_label(crosspoint.source.as_ref())
                    ),
                    DeviceEventKind::Online => "online".to_owned(),
                    DeviceEventKind::Offline => "OFFLINE".to_owned(),
                    DeviceEventKind::SnapshotReplaced => "state replaced (re-read)".to_owned(),
                };
                println!("{} {what}", ev.device);
            }
        }
        DeviceCmd::Snapshot { cmd } => match cmd {
            SnapshotCmd::Save {
                id,
                name,
                include,
                exclude,
            } => {
                let info = ok_or_msg(c.save_device_snapshot(id, name, include, exclude).await)?;
                if json {
                    return print_json(&info);
                }
                println!(
                    "saved '{}' from {}: {} param(s), {} route(s){}",
                    info.name,
                    info.device,
                    info.params,
                    info.routes,
                    if info.include.is_empty() {
                        String::new()
                    } else {
                        format!(" (include {})", info.include.join(", "))
                    }
                );
            }
            SnapshotCmd::List => {
                let list = ok_or_msg(c.list_device_snapshots().await)?;
                if json {
                    return print_json(&list);
                }
                for s in &list {
                    println!(
                        "{:<24} {:<36} {:>5} param(s) {:>4} route(s)  include [{}] exclude [{}]",
                        s.name,
                        s.device,
                        s.params,
                        s.routes,
                        s.include.join(", "),
                        s.exclude.join(", ")
                    );
                }
                if list.is_empty() {
                    println!("no device snapshots");
                }
            }
            SnapshotCmd::Diff {
                name,
                only,
                allow_disruptive,
            } => {
                let r = ok_or_msg(c.diff_device_snapshot(name, only, allow_disruptive).await)?;
                print_report(&r, json)?;
            }
            SnapshotCmd::Restore {
                name,
                dry_run,
                allow_disruptive,
                only,
            } => {
                let r = ok_or_msg(
                    c.restore_device_snapshot(name, only, dry_run, allow_disruptive)
                        .await,
                )?;
                print_report(&r, json)?;
                if r.count(DeviceRestoreStatus::Failed) > 0 {
                    eyre::bail!(
                        "{} item(s) failed to restore",
                        r.count(DeviceRestoreStatus::Failed)
                    );
                }
            }
            SnapshotCmd::Delete { name } => {
                ok_or_msg(c.delete_device_snapshot(name.clone()).await)?;
                if json {
                    return print_json(&serde_json::json!({ "deleted": name }));
                }
                println!("deleted device snapshot '{name}'");
            }
        },
    }
    Ok(())
}
