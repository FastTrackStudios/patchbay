// patchbay — the scriptable / AI-friendly surface of the patchbay.
//
// Talks to a RUNNING Patchbay app over ws (default
// `ws://127.0.0.1:4046/vox`, override `PATCHBAY_ADDR`/`--url`); if none
// is up, spins its own in-process engine so it works headless too.
// Every read command takes `--json` for machine consumption; names
// accept node/port ALIASES everywhere, so "connect the Guitar channel
// into REAPER in 3" works without knowing `capture_23`.

mod cli_device;
mod cli_mix;

use std::collections::HashMap;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_proto::{
    ClockInfo, DanteDevice, DanteStatus, GraphSnapshot, LatencyRule, NamedRoute,
    PatchbayServiceClient, PortDirection, PwNode, RouteEndpoint, ServiceAction, ServiceStatus,
};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "patchbay",
    about = "Agent-friendly PipeWire and Dante studio routing control"
)]
struct Cli {
    /// ws endpoint of a running Patchbay app.
    #[arg(long, env = "PATCHBAY_ADDR", default_value = "ws://127.0.0.1:4046/vox")]
    url: String,

    /// Don't try `--url`; run a private in-process engine instead.
    /// Without this, failing to reach a running app is an ERROR — a
    /// silent fallback would quietly open a second `PipeWire` connection
    /// and edit the graph from a different process than you expected.
    #[arg(long, global = true)]
    local: bool,

    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Engine + graph + clock + dante overview.
    Status,
    /// Full graph snapshot, including aliases when `--json` is used.
    #[command(alias = "snapshot")]
    Graph,
    /// Read-only rig and Dante health check. Exits successfully after
    /// printing findings so agents can consume the JSON report directly.
    #[command(alias = "doctor")]
    Health {
        /// Return exit code 1 when an error-level finding is present.
        #[arg(long)]
        strict: bool,
    },
    /// List nodes (aliases shown; `--json` for the full record).
    Nodes,
    /// List a node's ports with aliases (`node` = name, label, or alias).
    Ports { node: String },
    /// List links in name form.
    Links,
    /// Link an output to an input: `connect <node>:<port> <node>:<port>`
    /// (names or aliases on both sides).
    Connect { output: String, input: String },
    /// Remove one link (same addressing as `connect`).
    Disconnect { output: String, input: String },
    /// Link every numeric-suffix channel of one node into another 1:1.
    Connect1to1 {
        output_node: String,
        input_node: String,
    },
    /// Remove every link from one node into another.
    DisconnectNodes {
        output_node: String,
        input_node: String,
    },
    /// Routing presets.
    Preset {
        #[command(subcommand)]
        cmd: PresetCmd,
    },
    /// Display aliases (`target` = `node` or `node:port`).
    Alias {
        #[command(subcommand)]
        cmd: AliasCmd,
    },
    /// Named auto-connect routes: explicit, alias-addressed links the
    /// engine re-creates whenever both ends appear.
    Route {
        #[command(subcommand)]
        cmd: RouteCmd,
    },
    /// Alias a node's ports from a REAPER `ChanMap` file (channel names
    /// → port aliases). Empty path = the host's default chanmap.
    Chanmap {
        /// Node to name (name, label, or alias).
        node: String,
        /// `ChanMap` path; empty = `~/.fasttrackstudio/Reaper/ChanMaps/<host>.ReaperChanMap`.
        #[arg(long, default_value = "")]
        path: String,
    },
    /// Alias a node's ports from a live Dante device's channel names
    /// over ARC (channel N → port `*_N` by numeric suffix). The device
    /// must be one from `dante list` whose numbering matches the node's
    /// ports 1:1. NOTE: the local Inferno soundcard isn't in mDNS
    /// discovery yet (only remote consoles/interfaces are), so this
    /// can't name the local proxy from its own labels — see `dante list`.
    InfernoNames {
        /// Node to name (any node with numbered ports).
        node: String,
        /// `rx` (received channels → capture/input ports) or
        /// `tx` (transmitted channels → playback/output ports).
        direction: String,
        /// Dante device name (from `dante list`); empty = first discovered.
        #[arg(long, default_value = "")]
        device: String,
    },
    /// Show or force the graph quantum (`auto` clears the force).
    Quantum { frames: Option<String> },
    /// Managed systemd units (status, or `restart|start|stop <unit>`).
    #[command(alias = "service")]
    Services {
        action: Option<String>,
        unit: Option<String>,
    },
    /// Dante network (ARC): devices, channels, subscriptions.
    Dante {
        #[command(subcommand)]
        cmd: DanteCmd,
    },
    /// Per-app latency rules.
    Latency {
        #[command(subcommand)]
        cmd: LatencyCmd,
    },
    /// External hardware (Antelope Galaxy32, …): list, params, routing,
    /// named snapshots. See `patchbay device --help`.
    Device {
        #[command(subcommand)]
        cmd: cli_device::DeviceCmd,
    },
    /// Loopback / OBS-style host audio mixes (macOS): apps, inputs and
    /// system audio summed into outputs like the "Broadcast" virtual mic.
    /// See `patchbay mix --help`.
    Mix {
        #[command(subcommand)]
        cmd: cli_mix::MixCmd,
    },
    /// What is making sound on this machine right now: every app with
    /// audio and the device it plays to, the devices, the mixes, and
    /// anything that needs attention. The dashboard, as text.
    Now {
        /// Include apps that hold an audio client but aren't playing.
        #[arg(long)]
        all: bool,
        /// Follow live app levels for this many seconds instead of
        /// printing once. Metering taps exist only while something is
        /// watching, so the first reading takes a moment to appear.
        #[arg(long, value_name = "SECONDS")]
        watch: Option<u64>,
    },
    /// Patchbay's virtual audio devices (macOS, `Patchbay.driver`):
    /// loopbacks created, renamed and removed at runtime.
    Virtual {
        #[command(subcommand)]
        cmd: cli_mix::VirtualCmd,
    },
    /// Where the RPC and the browser remote listen. With no argument,
    /// show it and the URLs other machines would use.
    ///
    /// The RPC is UNAUTHENTICATED: anything that can reach it can
    /// re-route this machine's audio and write to the consoles the
    /// device adapters are connected to. Only open it on a network you
    /// control.
    Listen {
        /// `lan` (every interface), `local` (this machine only), or an
        /// explicit `host:port`. Saved; takes effect on the next start.
        #[arg(value_name = "lan|local|HOST:PORT")]
        to: Option<String>,
    },
    /// macOS privacy permissions of the running app (System Audio
    /// Recording, Microphone, Local Network). `permissions request` asks
    /// the app to show the system prompts / System Settings alert again.
    Permissions {
        #[command(subcommand)]
        cmd: Option<PermissionsCmd>,
    },
    /// Run the engine headless (no window) and serve it at `--bind`
    /// (`/vox` ws for this CLI and remotes). Useful on hosts without a
    /// desktop session, and on macOS where only the device layer runs.
    Serve {
        /// Address to listen on (`0.0.0.0:4046` to expose on the LAN).
        #[arg(long, default_value = "127.0.0.1:4046")]
        bind: String,
    },
}

#[derive(Subcommand, Clone, Copy)]
enum PermissionsCmd {
    /// Show the current state (the default).
    Status,
    /// Ask the app to re-run its permission flow (returns at once; the
    /// prompts appear in the app — check again with `permissions`).
    Request,
}

#[derive(Subcommand)]
enum PresetCmd {
    List,
    /// Snapshot current connections under a name.
    Save {
        name: String,
    },
    /// Re-create a preset's links; `--exclusive` also removes others.
    Apply {
        name: String,
        #[arg(long)]
        exclusive: bool,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum AliasCmd {
    List,
    /// Empty alias clears.
    Set {
        target: String,
        alias: String,
    },
}

#[derive(Subcommand)]
enum RouteCmd {
    List,
    /// Add/replace a route: `<name> <from> <to>`, each endpoint written
    /// `Node:Port` (or just `Port`). Port is an alias or raw name,
    /// matched normalized — `"Engineer TB"` hits `"81 - Engineer TB
    /// [DSP]"`. E.g. `route set eng-tb "Inferno source:Engineer TB
    /// [DSP]" "REAPER:Engineer TB"`.
    Set {
        name: String,
        from: String,
        to: String,
        /// Store it disabled (won't auto-connect until re-set enabled).
        #[arg(long)]
        disabled: bool,
    },
    /// Bank route: wire a whole output node to a whole input node 1:1
    /// by channel number (`out<N>`/`capture_N` → `in<N>`/`playback_N`), and keep
    /// it wired. `<name> <output-node> <input-node>` (node name or alias).
    /// E.g. `route bank inferno-to-reaper "Inferno source" REAPER`.
    Bank {
        name: String,
        output_node: String,
        input_node: String,
    },
    Remove {
        name: String,
    },
    /// Apply all enabled routes now; prints links created.
    Apply,
}

/// Parse `Node:Port` (or bare `Port`) into a route endpoint. Splits on
/// the FIRST colon — node names and port aliases don't contain colons.
fn parse_endpoint(s: &str) -> RouteEndpoint {
    match s.split_once(':') {
        Some((node, port)) => RouteEndpoint {
            node: node.trim().to_string(),
            port: port.trim().to_string(),
        },
        None => RouteEndpoint {
            node: String::new(),
            port: s.trim().to_string(),
        },
    }
}

#[derive(Subcommand)]
enum DanteCmd {
    /// Discover devices + channels + subscriptions (slow: mDNS + ARC).
    List,
    /// Show the Dante stack and live subscription health.
    Health {
        /// Return exit code 1 when an error-level finding is present.
        #[arg(long)]
        strict: bool,
    },
    /// Explicitly repair selected Dante/rig problems.
    Repair {
        /// Start dante.target when it is installed but inactive.
        #[arg(long)]
        start_stack: bool,
        /// Restart managed units currently in the failed state.
        #[arg(long)]
        restart_failed: bool,
        /// Re-apply the saved, non-destructive Dante subscription snapshot.
        #[arg(long)]
        apply_config: bool,
        /// Run all of the repair actions above.
        #[arg(long)]
        all: bool,
    },
    /// Subscribe `<rx_device> <rx_channel> <tx_device> <tx_channel>`.
    Subscribe {
        rx_device: String,
        rx_channel: u32,
        tx_device: String,
        tx_channel: String,
    },
    Unsubscribe {
        rx_device: String,
        rx_channel: u32,
    },
    /// Scan + persist the Dante routing snapshot (device channel names +
    /// subscriptions) to config.
    Save,
    /// Show the saved Dante config (`--json` for the full record).
    Config,
    /// Re-apply saved subscriptions to the live network (non-destructive).
    Apply,
}

#[derive(Subcommand)]
enum LatencyCmd {
    List,
    /// Set a rule: `<node> <quantum>` (`--request` for a soft request
    /// instead of a hard pin).
    Set {
        pattern: String,
        quantum: u32,
        #[arg(long)]
        request: bool,
    },
    Remove {
        pattern: String,
    },
}

// ─── Client plumbing ────────────────────────────────────────────────────

/// Ws to the running app, or — with `--local` — a private in-process
/// engine (~1.5s settle).
async fn client(url: &str, local: bool) -> eyre::Result<PatchbayServiceClient> {
    if !local {
        match vox_websocket::WsLink::connect(url).await {
            Ok(link) => {
                return vox_core::initiator_on(link)
                    .establish()
                    .await
                    .map_err(|e| eyre::eyre!("handshake with {url} failed: {e:?}"));
            }
            Err(e) => {
                eyre::bail!(
                    "no Patchbay app at {url} ({e}).\n\
                     Start the app, or pass --local to run a private in-process engine."
                );
            }
        }
    }
    eprintln!("(--local: running a private in-process engine)");
    let backend = patchbay::PatchbayBackend::new();
    let scope = architect::Scope::new();
    let server = architect::LocalServer::serve(backend.router(), Arc::clone(&scope));
    let caller = server
        .caller()
        .await
        .map_err(|e| eyre::eyre!("local caller: {e:?}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    // Leak so the acceptor + engine outlive this fn; keep the server
    // reachable for stream clients (`device watch`).
    let leaked = Box::leak(Box::new((scope, server, backend)));
    let _ = LOCAL_SERVER.set(&leaked.1);
    Ok(PatchbayServiceClient::new(caller))
}

/// The `--local` in-process server, once `client` created it.
static LOCAL_SERVER: std::sync::OnceLock<&'static architect::LocalServer> =
    std::sync::OnceLock::new();

/// A `#[subscribe]` stream client on the same target as `client`.
async fn stream_client(url: String) -> eyre::Result<PatchbayServiceStreamClient> {
    if let Some(server) = LOCAL_SERVER.get() {
        return server
            .establish::<PatchbayServiceStreamClient>()
            .await
            .map_err(|e| eyre::eyre!("local stream client: {e:?}"));
    }
    let link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| eyre::eyre!("connect (stream) {url}: {e}"))?;
    vox_core::initiator_on(link)
        .establish()
        .await
        .map_err(|e| eyre::eyre!("stream handshake with {url} failed: {e:?}"))
}

/// `patchbay serve`: the engine without a window.
async fn serve(bind: String) -> eyre::Result<()> {
    let backend = patchbay::PatchbayBackend::new();
    eprintln!("patchbay engine serving ws://{bind}/vox (ctrl-c to stop)");
    architect::host::EngineHost::new(backend.router(), bind)
        .serve()
        .await;
    Ok(())
}

/// Resolve a node by name, label, or alias (case-insensitive; exact
/// name wins, then unique substring-ish matches error out loudly).
fn find_node<'a>(
    graph: &'a GraphSnapshot,
    aliases: &HashMap<String, String>,
    query: &str,
) -> eyre::Result<&'a PwNode> {
    let q = query.to_lowercase();
    if let Some(n) = graph.nodes.iter().find(|n| n.name == query) {
        return Ok(n);
    }
    let matches: Vec<&PwNode> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.name.to_lowercase() == q
                || n.label.to_lowercase() == q
                || aliases.get(&n.name).is_some_and(|a| a.to_lowercase() == q)
        })
        .collect();
    match matches.as_slice() {
        [only] => Ok(*only),
        [] => eyre::bail!("no node matches '{query}' (try `patchbay-cli nodes`)"),
        many => eyre::bail!(
            "'{query}' is ambiguous ({} nodes match) — use the exact node.name",
            many.len()
        ),
    }
}

/// Resolve `<node>:<port>` (aliases OK on both halves) to a port id.
fn find_port(
    graph: &GraphSnapshot,
    aliases: &HashMap<String, String>,
    spec: &str,
    direction: PortDirection,
) -> eyre::Result<u32> {
    let (node_q, port_q) = spec
        .rsplit_once(':')
        .ok_or_else(|| eyre::eyre!("'{spec}' — expected <node>:<port>"))?;
    let node = find_node(graph, aliases, node_q)?;
    let pq = port_q.to_lowercase();
    let matches: Vec<u32> = graph
        .ports
        .iter()
        .filter(|p| p.node_id == node.id && p.direction == direction)
        .filter(|p| {
            p.name.to_lowercase() == pq
                || aliases
                    .get(&format!("{}:{}", node.name, p.name))
                    .is_some_and(|a| a.to_lowercase() == pq)
        })
        .map(|p| p.id)
        .collect();
    match matches.as_slice() {
        [only] => Ok(*only),
        [] => eyre::bail!(
            "no {direction:?} port '{port_q}' on '{}' (try `patchbay-cli ports '{}'`)",
            node.name,
            node.name
        ),
        many => eyre::bail!("'{spec}' is ambiguous ({} ports match)", many.len()),
    }
}

fn alias_map(entries: Vec<patchbay_proto::AliasEntry>) -> HashMap<String, String> {
    entries.into_iter().map(|a| (a.target, a.alias)).collect()
}

pub(crate) fn ok_or_msg<T, E: std::fmt::Display>(r: Result<T, E>) -> eyre::Result<T> {
    r.map_err(|e| eyre::eyre!("{e}"))
}

#[derive(Debug, Serialize)]
struct HealthIssue {
    severity: String,
    code: String,
    target: String,
    detail: String,
    remediation: Option<String>,
}

#[derive(Debug, Serialize)]
struct GraphHealth {
    nodes: usize,
    ports: usize,
    links: usize,
    active_links: usize,
}

#[derive(Debug, Serialize)]
struct HealthReport {
    ok: bool,
    graph: GraphHealth,
    clock: ClockInfo,
    dante: DanteStatus,
    services: Vec<ServiceStatus>,
    dante_devices: Vec<DanteDevice>,
    dante_scan_error: Option<String>,
    issues: Vec<HealthIssue>,
}

fn issue(
    severity: &str,
    code: &str,
    target: impl Into<String>,
    detail: impl Into<String>,
    remediation: Option<&str>,
) -> HealthIssue {
    HealthIssue {
        severity: severity.to_owned(),
        code: code.to_owned(),
        target: target.into(),
        detail: detail.into(),
        remediation: remediation.map(str::to_owned),
    }
}

fn add_dante_issues(
    dante: &DanteStatus,
    devices: &[DanteDevice],
    scan_error: Option<&str>,
    issues: &mut Vec<HealthIssue>,
) {
    if !dante.installed {
        issues.push(issue(
            "warning",
            "dante_not_installed",
            "dante.target",
            "the managed Dante stack is not installed on this host",
            Some("install/deploy the Dante stack, then run `patchbay dante health` again"),
        ));
    } else if !dante.active {
        issues.push(issue(
            "error",
            "dante_stack_inactive",
            "dante.target",
            "the managed Dante stack is installed but inactive",
            Some("patchbay dante repair --start-stack"),
        ));
    }

    if let Some(error) = scan_error {
        issues.push(issue(
            if dante.active { "error" } else { "warning" },
            "dante_scan_failed",
            "dante network",
            error,
            Some("verify mDNS/ARC reachability, then retry `patchbay dante health`"),
        ));
        return;
    }

    let device_names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
    for device in devices {
        if device.unreachable {
            issues.push(issue(
                "error",
                "dante_device_unreachable",
                &device.name,
                format!(
                    "{} is visible by mDNS at {} but did not answer ARC",
                    device.name, device.ip
                ),
                Some("check network/VLAN reachability and the device's Dante control service"),
            ));
        }
        for subscription in &device.subscriptions {
            if subscription.status != 1 {
                issues.push(issue(
                    "error",
                    "dante_subscription_unhealthy",
                    format!("{}:rx{}", device.name, subscription.rx_channel),
                    format!(
                        "subscription to {}@{} has ARC status {} (1 is healthy)",
                        subscription.tx_channel, subscription.tx_device, subscription.status
                    ),
                    Some("compare with the saved snapshot, then run `patchbay dante repair --apply-config`"),
                ));
            }
            if !subscription.tx_device.is_empty()
                && !device_names.contains(&subscription.tx_device.as_str())
            {
                issues.push(issue(
                    "warning",
                    "dante_source_not_discovered",
                    format!("{}:rx{}", device.name, subscription.rx_channel),
                    format!(
                        "source device '{}' was not discovered in this scan",
                        subscription.tx_device
                    ),
                    Some("check the source device's network/VLAN and repeat the scan"),
                ));
            }
        }
    }
}

async fn collect_health(c: &PatchbayServiceClient) -> eyre::Result<HealthReport> {
    let (graph, clock, dante, services, network) = tokio::join!(
        c.graph(),
        c.clock(),
        c.dante_status(),
        c.services(),
        c.dante_network(),
    );
    let graph = ok_or_msg(graph)?;
    let clock = ok_or_msg(clock)?;
    let dante = ok_or_msg(dante)?;
    let services = ok_or_msg(services)?;

    let (dante_devices, dante_scan_error) = match network {
        Ok(devices) => (devices, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };

    let mut issues = Vec::new();
    for service in &services {
        if service.unit == "dante.target" {
            continue;
        }
        let critical = matches!(
            service.unit.as_str(),
            "pipewire.service" | "wireplumber.service" | "pipewire-pulse.service"
        );
        if !service.present {
            issues.push(issue(
                if critical { "error" } else { "warning" },
                "managed_service_missing",
                &service.unit,
                format!("{} is not installed", service.label),
                Some("deploy the managed audio stack or remove this optional service from the rig"),
            ));
        } else if service.state != "active" {
            issues.push(issue(
                if critical { "error" } else { "warning" },
                "managed_service_unhealthy",
                &service.unit,
                format!(
                    "{} is {}/{}",
                    service.label, service.state, service.sub_state
                ),
                Some("inspect `patchbay services --json`, then restart the affected managed unit"),
            ));
        }
    }
    add_dante_issues(
        &dante,
        &dante_devices,
        dante_scan_error.as_deref(),
        &mut issues,
    );

    let report = HealthReport {
        ok: !issues.iter().any(|finding| finding.severity == "error"),
        graph: GraphHealth {
            nodes: graph.nodes.len(),
            ports: graph.ports.len(),
            links: graph.links.len(),
            active_links: graph.links.iter().filter(|link| link.active).count(),
        },
        clock,
        dante,
        services,
        dante_devices,
        dante_scan_error,
        issues,
    };
    Ok(report)
}

fn print_health(report: &HealthReport, json: bool) -> eyre::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!(
        "health: {}",
        if report.ok {
            "ok"
        } else {
            "attention required"
        }
    );
    println!(
        "graph: {} nodes / {} ports / {} links ({} active)",
        report.graph.nodes, report.graph.ports, report.graph.links, report.graph.active_links
    );
    println!(
        "clock: {} Hz, quantum {} (force {})",
        report.clock.rate, report.clock.quantum, report.clock.force_quantum
    );
    println!(
        "dante: stack {} / {} device(s) / {} subscription(s)",
        if report.dante.active {
            "active"
        } else {
            "inactive"
        },
        report.dante_devices.len(),
        report
            .dante_devices
            .iter()
            .map(|device| device.subscriptions.len())
            .sum::<usize>()
    );
    for finding in &report.issues {
        println!(
            "  [{}] {}: {} — {}",
            finding.severity, finding.target, finding.code, finding.detail
        );
    }
    if report.issues.is_empty() {
        println!("  no findings");
    }
    Ok(())
}

fn print_dante_health(
    dante: &DanteStatus,
    devices: &[DanteDevice],
    scan_error: Option<&str>,
    issues: &[HealthIssue],
    json: bool,
) -> eyre::Result<()> {
    let ok = !issues.iter().any(|finding| finding.severity == "error");
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": ok,
                "dante": dante,
                "devices": devices,
                "scan_error": scan_error,
                "issues": issues,
            }))?
        );
        return Ok(());
    }
    println!(
        "dante health: {} (stack {}, {} device(s))",
        if ok { "ok" } else { "attention required" },
        if dante.active { "active" } else { "inactive" },
        devices.len()
    );
    if let Some(error) = scan_error {
        println!("  [error] network scan failed: {error}");
    }
    for device in devices {
        let unhealthy = device
            .subscriptions
            .iter()
            .filter(|subscription| subscription.status != 1)
            .count();
        println!(
            "  {} @ {} — {} tx / {} rx / {} sub(s), {} unhealthy{}",
            device.name,
            device.ip,
            device.tx.len(),
            device.rx.len(),
            device.subscriptions.len(),
            unhealthy,
            if device.unreachable {
                " [ARC unreachable]"
            } else {
                ""
            }
        );
    }
    for finding in issues {
        println!(
            "  [{}] {}: {} — {}",
            finding.severity, finding.target, finding.code, finding.detail
        );
    }
    Ok(())
}

// One arm per subcommand: a flat dispatch table is the clearest shape
// for a CLI, and splitting it into 30 one-call helpers would only move
// the length around.
#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    if let Cmd::Serve { bind } = cli.cmd {
        return serve(bind).await;
    }
    let c = client(&cli.url, cli.local).await?;

    match cli.cmd {
        Cmd::Serve { .. } => {}
        Cmd::Device { cmd } => {
            if cli.local {
                cli_device::settle_local(&c).await;
            }
            let url = cli.url.clone();
            cli_device::run(&c, || stream_client(url), cmd, cli.json).await?;
        }
        Cmd::Status => {
            let g = ok_or_msg(c.graph().await)?;
            let clock = ok_or_msg(c.clock().await)?;
            let dante = ok_or_msg(c.dante_status().await)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "nodes": g.nodes.len(), "ports": g.ports.len(), "links": g.links.len(),
                        "clock": clock, "dante_active": dante.active,
                    })
                );
            } else {
                println!(
                    "graph: {} nodes / {} ports / {} links",
                    g.nodes.len(),
                    g.ports.len(),
                    g.links.len()
                );
                println!(
                    "clock: {} Hz, quantum {} (force {}), range {}–{}",
                    clock.rate,
                    clock.quantum,
                    clock.force_quantum,
                    clock.min_quantum,
                    clock.max_quantum
                );
                println!(
                    "dante stack: {}",
                    if dante.active { "active" } else { "inactive" }
                );
            }
        }
        Cmd::Graph => {
            let graph = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "graph": graph,
                        "aliases": aliases,
                    }))?
                );
            } else {
                println!(
                    "graph: {} nodes / {} ports / {} links",
                    graph.nodes.len(),
                    graph.ports.len(),
                    graph.links.len()
                );
                println!("use `patchbay nodes`, `patchbay ports <node>`, or `patchbay links`");
            }
        }
        Cmd::Mix { cmd } => {
            Box::pin(cli_mix::run(&c, cmd, cli.json)).await?;
        }
        Cmd::Virtual { cmd } => {
            Box::pin(cli_mix::run_virtual(&c, cmd, cli.json)).await?;
        }
        Cmd::Now { all, watch } => match watch {
            Some(secs) => Box::pin(cli_mix::watch_now(&c, all, secs)).await?,
            None => Box::pin(cli_mix::run_now(&c, all, cli.json)).await?,
        },
        Cmd::Listen { to } => {
            Box::pin(cli_mix::run_listen(&c, to, cli.json)).await?;
        }
        Cmd::Permissions { cmd } => {
            let status = match cmd.unwrap_or(PermissionsCmd::Status) {
                PermissionsCmd::Status => ok_or_msg(c.permissions().await)?,
                PermissionsCmd::Request => ok_or_msg(c.request_permissions().await)?,
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "platform: {}{}",
                    status.platform,
                    if status.bundled {
                        " (Patchbay.app)"
                    } else {
                        ""
                    }
                );
                println!("system audio recording: {}", status.system_audio_recording);
                println!("microphone:             {}", status.microphone);
                println!("local network:          {}", status.local_network);
                if status.requesting {
                    println!("request in progress:    yes");
                }
                println!("{}", status.note);
            }
        }
        Cmd::Health { strict } => {
            let report = collect_health(&c).await?;
            print_health(&report, cli.json)?;
            if strict && !report.ok {
                eyre::bail!("health check has error-level findings");
            }
        }
        Cmd::Nodes => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&g.nodes)?);
                return Ok(());
            }
            let mut nodes = g.nodes.clone();
            nodes.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
            for n in nodes {
                let ins = g
                    .ports
                    .iter()
                    .filter(|p| p.node_id == n.id && p.direction == PortDirection::Input)
                    .count();
                let outs = g
                    .ports
                    .iter()
                    .filter(|p| p.node_id == n.id && p.direction == PortDirection::Output)
                    .count();
                let alias = aliases
                    .get(&n.name)
                    .map(|a| format!(" (alias: {a})"))
                    .unwrap_or_default();
                println!(
                    "[{:>4}] {:<44} {:<22} in:{:<4} out:{:<4}{}",
                    n.id, n.label, n.media_class, ins, outs, alias
                );
                if n.label != n.name {
                    println!("       node.name = {}", n.name);
                }
            }
        }
        Cmd::Ports { node } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let n = find_node(&g, &aliases, &node)?;
            let mut ports: Vec<_> = g.ports.iter().filter(|p| p.node_id == n.id).collect();
            // Inputs before outputs, then by name.
            let dir_rank = |d: PortDirection| u8::from(d == PortDirection::Output);
            ports.sort_by(|a, b| {
                (dir_rank(a.direction), &a.name).cmp(&(dir_rank(b.direction), &b.name))
            });
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&ports)?);
                return Ok(());
            }
            println!("{} [{}] — {} ports", n.label, n.name, ports.len());
            for p in ports {
                let alias = aliases
                    .get(&format!("{}:{}", n.name, p.name))
                    .map(|a| format!("  → {a}"))
                    .unwrap_or_default();
                println!(
                    "  [{:>4}] {:<4} {:<28}{}",
                    p.id,
                    if p.direction == PortDirection::Input {
                        "in"
                    } else {
                        "out"
                    },
                    p.name,
                    alias
                );
            }
        }
        Cmd::Links => {
            let g = ok_or_msg(c.graph().await)?;
            let node = |id: u32| {
                g.nodes
                    .iter()
                    .find(|n| n.id == id)
                    .map_or("?", |n| n.name.as_str())
            };
            let port = |id: u32| {
                g.ports
                    .iter()
                    .find(|p| p.id == id)
                    .map_or("?", |p| p.name.as_str())
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&g.links)?);
                return Ok(());
            }
            for l in &g.links {
                println!(
                    "[{:>4}] {}:{} -> {}:{}{}",
                    l.id,
                    node(l.output_node),
                    port(l.output_port),
                    node(l.input_node),
                    port(l.input_port),
                    if l.active { "" } else { "  (inactive)" }
                );
            }
        }
        Cmd::Connect { output, input } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let out = find_port(&g, &aliases, &output, PortDirection::Output)?;
            let inp = find_port(&g, &aliases, &input, PortDirection::Input)?;
            ok_or_msg(c.create_link(out, inp).await)?;
            println!("linked {output} -> {input}");
        }
        Cmd::Disconnect { output, input } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let out = find_port(&g, &aliases, &output, PortDirection::Output)?;
            let inp = find_port(&g, &aliases, &input, PortDirection::Input)?;
            let link = g
                .links
                .iter()
                .find(|l| l.output_port == out && l.input_port == inp)
                .ok_or_else(|| eyre::eyre!("no link between {output} and {input}"))?;
            ok_or_msg(c.destroy_link(link.id).await)?;
            println!("unlinked {output} -> {input}");
        }
        Cmd::Connect1to1 {
            output_node,
            input_node,
        } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let on = find_node(&g, &aliases, &output_node)?.name.clone();
            let inn = find_node(&g, &aliases, &input_node)?.name.clone();
            let n = ok_or_msg(c.connect_one_to_one(on, inn).await)?;
            println!("created {n} link(s)");
        }
        Cmd::DisconnectNodes {
            output_node,
            input_node,
        } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let on = find_node(&g, &aliases, &output_node)?.name.clone();
            let inn = find_node(&g, &aliases, &input_node)?.name.clone();
            let n = ok_or_msg(c.disconnect_nodes(on, inn).await)?;
            println!("removed {n} link(s)");
        }
        Cmd::Preset { cmd } => match cmd {
            PresetCmd::List => {
                let presets = ok_or_msg(c.list_presets().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&presets)?);
                } else {
                    for p in presets {
                        println!("{:<28} {} link(s)", p.name, p.links.len());
                    }
                }
            }
            PresetCmd::Save { name } => {
                let p = ok_or_msg(c.save_preset(name, String::new()).await)?;
                println!("saved '{}' with {} link(s)", p.name, p.links.len());
            }
            PresetCmd::Apply { name, exclusive } => {
                let r = ok_or_msg(c.apply_preset(name, exclusive).await)?;
                println!(
                    "created {} / kept {} / removed {} / missing {}",
                    r.created,
                    r.existing,
                    r.destroyed,
                    r.missing.len()
                );
                for m in r.missing.iter().take(10) {
                    println!(
                        "  missing: {}:{} -> {}:{}",
                        m.output_node, m.output_port, m.input_node, m.input_port
                    );
                }
            }
            PresetCmd::Delete { name } => {
                ok_or_msg(c.delete_preset(name.clone()).await)?;
                println!("deleted '{name}'");
            }
        },
        Cmd::Alias { cmd } => match cmd {
            AliasCmd::List => {
                let aliases = ok_or_msg(c.aliases().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&aliases)?);
                } else {
                    for a in aliases {
                        println!("{:<52} → {}", a.target, a.alias);
                    }
                }
            }
            AliasCmd::Set { target, alias } => {
                ok_or_msg(c.set_alias(target.clone(), alias.clone()).await)?;
                println!("{target} → {alias}");
            }
        },
        Cmd::Route { cmd } => match cmd {
            RouteCmd::List => {
                let routes = ok_or_msg(c.routes().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&routes)?);
                } else if routes.is_empty() {
                    println!("(no routes)");
                } else {
                    for r in routes {
                        let fmt = |e: &RouteEndpoint| {
                            if e.node.is_empty() {
                                e.port.clone()
                            } else {
                                format!("{}:{}", e.node, e.port)
                            }
                        };
                        let flag = if r.enabled { "" } else { "  (disabled)" };
                        println!("{:<24} {}  →  {}{}", r.name, fmt(&r.from), fmt(&r.to), flag);
                    }
                }
            }
            RouteCmd::Set {
                name,
                from,
                to,
                disabled,
            } => {
                let route = NamedRoute {
                    name: name.clone(),
                    from: parse_endpoint(&from),
                    to: parse_endpoint(&to),
                    enabled: !disabled,
                };
                ok_or_msg(c.set_route(route).await)?;
                println!("route '{name}' set");
            }
            RouteCmd::Bank {
                name,
                output_node,
                input_node,
            } => {
                let route = NamedRoute {
                    name: name.clone(),
                    from: RouteEndpoint {
                        node: output_node,
                        port: "*".into(),
                    },
                    to: RouteEndpoint {
                        node: input_node,
                        port: "*".into(),
                    },
                    enabled: true,
                };
                ok_or_msg(c.set_route(route).await)?;
                println!("bank route '{name}' set (whole-node 1:1)");
            }
            RouteCmd::Remove { name } => {
                ok_or_msg(c.delete_route(name.clone()).await)?;
                println!("route '{name}' removed");
            }
            RouteCmd::Apply => {
                let n = ok_or_msg(c.apply_routes().await)?;
                println!("applied routes: {n} link(s) created");
            }
        },
        Cmd::Chanmap { node, path } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let n = find_node(&g, &aliases, &node)?.name.clone();
            let written = ok_or_msg(c.import_chanmap(n.clone(), path).await)?;
            println!("aliased {written} port(s) on {n} from the ChanMap");
        }
        Cmd::InfernoNames {
            node,
            direction,
            device,
        } => {
            let g = ok_or_msg(c.graph().await)?;
            let aliases = alias_map(ok_or_msg(c.aliases().await)?);
            let n = find_node(&g, &aliases, &node)?.name.clone();
            let written = ok_or_msg(c.import_inferno_names(n.clone(), device, direction).await)?;
            println!("aliased {written} port(s) on {n} from Inferno ARC names");
        }
        Cmd::Quantum { frames } => match frames {
            None => {
                let clock = ok_or_msg(c.clock().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&clock)?);
                } else {
                    println!(
                        "{} Hz, quantum {} (force {}), range {}–{}",
                        clock.rate,
                        clock.quantum,
                        clock.force_quantum,
                        clock.min_quantum,
                        clock.max_quantum
                    );
                }
            }
            Some(f) => {
                let frames = if f == "auto" { 0 } else { f.parse()? };
                ok_or_msg(c.force_quantum(frames).await)?;
                println!(
                    "force-quantum = {}",
                    if frames == 0 {
                        "auto".into()
                    } else {
                        frames.to_string()
                    }
                );
            }
        },
        Cmd::Services { action, unit } => match (action.as_deref(), unit) {
            (None, _) | (Some("status"), None) => {
                let services = ok_or_msg(c.services().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&services)?);
                } else {
                    for s in services {
                        println!(
                            "[{}] {:<28} {}/{}",
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
                }
            }
            (Some(verb), Some(unit)) => {
                let action = match verb {
                    "start" => ServiceAction::Start,
                    "stop" => ServiceAction::Stop,
                    "restart" => ServiceAction::Restart,
                    other => eyre::bail!("unknown action '{other}' (start|stop|restart)"),
                };
                ok_or_msg(c.service_action(unit.clone(), action).await)?;
                println!("{verb} {unit}: ok");
            }
            (Some(_), None) => eyre::bail!("usage: services <start|stop|restart> <unit>"),
        },
        Cmd::Dante { cmd } => match cmd {
            DanteCmd::List => {
                let devices = ok_or_msg(c.dante_network().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&devices)?);
                    return Ok(());
                }
                for d in &devices {
                    println!(
                        "{} @ {}:{} — {} tx / {} rx / {} sub(s){}",
                        d.name,
                        d.ip,
                        d.arc_port,
                        d.tx.len(),
                        d.rx.len(),
                        d.subscriptions.len(),
                        if d.unreachable {
                            "  [ARC unreachable]"
                        } else {
                            ""
                        }
                    );
                    for s in &d.subscriptions {
                        let rx_name =
                            d.rx.iter()
                                .find(|ch| ch.number == s.rx_channel)
                                .map_or("?", |ch| ch.name.as_str());
                        println!(
                            "   rx {:>3} {:<26} <- {}@{}  status={}",
                            s.rx_channel, rx_name, s.tx_channel, s.tx_device, s.status
                        );
                    }
                }
            }
            DanteCmd::Health { strict } => {
                let dante = ok_or_msg(c.dante_status().await)?;
                let network = c.dante_network().await;
                let (devices, scan_error) = match network {
                    Ok(devices) => (devices, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                let mut issues = Vec::new();
                add_dante_issues(&dante, &devices, scan_error.as_deref(), &mut issues);
                print_dante_health(&dante, &devices, scan_error.as_deref(), &issues, cli.json)?;
                if strict && issues.iter().any(|finding| finding.severity == "error") {
                    eyre::bail!("Dante health has error-level findings");
                }
            }
            DanteCmd::Repair {
                start_stack,
                restart_failed,
                apply_config,
                all,
            } => {
                if !(all || start_stack || restart_failed || apply_config) {
                    eyre::bail!(
                        "choose at least one repair action: --start-stack, --restart-failed, --apply-config, or --all"
                    );
                }
                let mut actions = Vec::new();
                let dante = ok_or_msg(c.dante_status().await)?;
                if (all || start_stack) && dante.installed && !dante.active {
                    ok_or_msg(c.set_dante(true).await)?;
                    actions.push("started dante.target".to_owned());
                }
                if all || restart_failed {
                    let services = ok_or_msg(c.services().await)?;
                    for service in services
                        .iter()
                        .filter(|service| service.present && service.state == "failed")
                    {
                        if service.unit == "dante.target" {
                            if !actions
                                .iter()
                                .any(|action| action == "started dante.target")
                            {
                                ok_or_msg(c.set_dante(true).await)?;
                                actions.push("started dante.target".to_owned());
                            }
                        } else {
                            ok_or_msg(
                                c.service_action(service.unit.clone(), ServiceAction::Restart)
                                    .await,
                            )?;
                            actions.push(format!("restarted {}", service.unit));
                        }
                    }
                }
                if all || apply_config {
                    let applied = ok_or_msg(c.apply_dante_config().await)?;
                    actions.push(format!("applied {applied} saved subscription(s)"));
                }
                if cli.json {
                    println!("{}", serde_json::json!({"actions": actions}));
                } else if actions.is_empty() {
                    println!("no Dante repair actions were needed");
                } else {
                    for action in actions {
                        println!("{action}");
                    }
                }
            }
            DanteCmd::Subscribe {
                rx_device,
                rx_channel,
                tx_device,
                tx_channel,
            } => {
                ok_or_msg(
                    c.dante_subscribe(rx_device, rx_channel, tx_device, tx_channel)
                        .await,
                )?;
                println!("subscribed");
            }
            DanteCmd::Unsubscribe {
                rx_device,
                rx_channel,
            } => {
                ok_or_msg(c.dante_unsubscribe(rx_device, rx_channel).await)?;
                println!("unsubscribed");
            }
            DanteCmd::Save => {
                let n = ok_or_msg(c.save_dante_config().await)?;
                println!("saved Dante config: {n} device(s)");
            }
            DanteCmd::Config => {
                let devices = ok_or_msg(c.dante_config().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&devices)?);
                    return Ok(());
                }
                if devices.is_empty() {
                    println!("(no saved Dante config — run `dante save`)");
                }
                for d in &devices {
                    let subs = d
                        .subscriptions
                        .iter()
                        .filter(|s| !s.tx_channel.is_empty())
                        .count();
                    println!(
                        "{} — {} tx / {} rx / {} sub(s)",
                        d.name,
                        d.tx.len(),
                        d.rx.len(),
                        subs
                    );
                    for s in d.subscriptions.iter().filter(|s| !s.tx_channel.is_empty()) {
                        let rx_name =
                            d.rx.iter()
                                .find(|ch| ch.number == s.rx_channel)
                                .map_or("?", |ch| ch.name.as_str());
                        println!(
                            "   rx {:>3} {:<26} <- {}@{}",
                            s.rx_channel, rx_name, s.tx_channel, s.tx_device
                        );
                    }
                }
            }
            DanteCmd::Apply => {
                let n = ok_or_msg(c.apply_dante_config().await)?;
                println!("applied Dante config: {n} subscription(s) (re)set");
            }
        },
        Cmd::Latency { cmd } => match cmd {
            LatencyCmd::List => {
                let rules = ok_or_msg(c.latency_rules().await)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&rules)?);
                } else {
                    for r in rules {
                        println!(
                            "{:<32} {} frames ({})",
                            r.pattern,
                            r.quantum,
                            if r.force { "pin" } else { "request" }
                        );
                    }
                }
            }
            LatencyCmd::Set {
                pattern,
                quantum,
                request,
            } => {
                ok_or_msg(
                    c.set_latency_rule(LatencyRule {
                        pattern: pattern.clone(),
                        quantum,
                        force: !request,
                    })
                    .await,
                )?;
                println!("{pattern} → {quantum} frames (restart the app or WirePlumber to apply)");
            }
            LatencyCmd::Remove { pattern } => {
                ok_or_msg(c.remove_latency_rule(pattern.clone()).await)?;
                println!("removed rule for {pattern}");
            }
        },
    }
    Ok(())
}
