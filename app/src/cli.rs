// Lint debt: workspace flipped dead_code/unused to warn (task cleanup);
// this crate predates that — burn down separately.
#![allow(dead_code, unused)]

//! patchbay-cli — the scriptable / AI-friendly surface of the patchbay.
//!
//! Talks to a RUNNING Patchbay app over ws (default
//! `ws://127.0.0.1:4046/vox`, override `PATCHBAY_ADDR`/`--url`); if none
//! is up, spins its own in-process engine so it works headless too.
//! Every read command takes `--json` for machine consumption; names
//! accept node/port ALIASES everywhere, so "connect the Guitar channel
//! into REAPER in 3" works without knowing `capture_23`.

use std::collections::HashMap;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use patchbay_proto::{
    GraphSnapshot, LatencyRule, NamedRoute, PatchbayError, PatchbayService, PatchbayServiceClient,
    PortDirection, PwNode, RouteEndpoint, ServiceAction,
};

#[derive(Parser)]
#[command(
    name = "patchbay-cli",
    about = "PipeWire studio-routing control (FTS Patchbay)"
)]
struct Cli {
    /// ws endpoint of a running Patchbay app; falls back to an
    /// in-process engine when unreachable.
    #[arg(long, env = "PATCHBAY_ADDR", default_value = "ws://127.0.0.1:4046/vox")]
    url: String,

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
    /// Alias a node's ports from a REAPER ChanMap file (channel names
    /// → port aliases). Empty path = the host's default chanmap.
    Chanmap {
        /// Node to name (name, label, or alias).
        node: String,
        /// ChanMap path; empty = `~/.fasttrackstudio/Reaper/ChanMaps/<host>.ReaperChanMap`.
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

/// Ws to the running app, else a local engine (~1.5s settle).
async fn client(url: &str) -> eyre::Result<PatchbayServiceClient> {
    if let Ok(link) = vox_websocket::WsLink::connect(url).await {
        if let Ok(c) = vox_core::initiator_on(link).establish().await {
            return Ok(c);
        }
    }
    eprintln!("(no running app at {url} — using an in-process engine)");
    let backend = patchbay::PatchbayBackend::new();
    let scope = architect::Scope::new();
    let server = architect::LocalServer::serve(backend.router(), Arc::clone(&scope));
    let caller = server
        .caller()
        .await
        .map_err(|e| eyre::eyre!("local caller: {e:?}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    // Leak so the acceptor + engine outlive this fn.
    Box::leak(Box::new((scope, server, backend)));
    Ok(PatchbayServiceClient::new(caller))
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
    match matches.len() {
        1 => Ok(matches[0]),
        0 => eyre::bail!("no node matches '{query}' (try `patchbay-cli nodes`)"),
        n => eyre::bail!("'{query}' is ambiguous ({n} nodes match) — use the exact node.name"),
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
    match matches.len() {
        1 => Ok(matches[0]),
        0 => eyre::bail!(
            "no {direction:?} port '{port_q}' on '{}' (try `patchbay-cli ports '{}'`)",
            node.name,
            node.name
        ),
        n => eyre::bail!("'{spec}' is ambiguous ({n} ports match)"),
    }
}

fn alias_map(entries: Vec<patchbay_proto::AliasEntry>) -> HashMap<String, String> {
    entries.into_iter().map(|a| (a.target, a.alias)).collect()
}

fn ok_or_msg<T, E: std::fmt::Display>(r: Result<T, E>) -> eyre::Result<T> {
    r.map_err(|e| eyre::eyre!("{e}"))
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    let c = client(&cli.url).await?;

    match cli.cmd {
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
            ports.sort_by(|a, b| (a.direction as u8, &a.name).cmp(&(b.direction as u8, &b.name)));
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
                    .map(|n| n.name.as_str())
                    .unwrap_or("?")
            };
            let port = |id: u32| {
                g.ports
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.as_str())
                    .unwrap_or("?")
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
            (None, _) => {
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
                                .map(|ch| ch.name.as_str())
                                .unwrap_or("?");
                        println!(
                            "   rx {:>3} {:<26} <- {}@{}  status={}",
                            s.rx_channel, rx_name, s.tx_channel, s.tx_device, s.status
                        );
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
                                .map(|ch| ch.name.as_str())
                                .unwrap_or("?");
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
