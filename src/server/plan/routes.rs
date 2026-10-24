//! Named-route planning: the explicit auto-connect.
//!
//! A route says "keep this output linked to this input whenever both
//! resolve", addressed by SEMANTIC name so it survives the channel being
//! renumbered. Applying is idempotent and strictly additive — it only
//! ever creates a missing link, never tears anything down, because this
//! edits a live production graph.

use std::collections::HashMap;

use crate::proto::{NamedRoute, PortDirection, PwNode, RouteEndpoint};

use crate::server::engine::Command;
use crate::server::plan::cycles::CycleGuard;
use crate::server::plan::pairing;
use crate::server::store::GraphStore;

/// The port sentinel that turns a route into a whole-node BANK route:
/// pair the two nodes' ports 1:1 by channel instead of matching a single
/// named port.
pub(crate) const BANK_PORT: &str = "*";

/// What an apply would do.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RoutePlan {
    /// Links to create, in route order.
    pub commands: Vec<Command>,
    /// Enabled routes whose endpoints both resolved.
    pub resolved: u32,
    /// Names of enabled routes that did NOT resolve — the "why is my rig
    /// not wired" answer, and the reason this is a plan rather than a
    /// bare `Vec<Command>`.
    pub unresolved: Vec<String>,
    /// Routes that resolved but were REFUSED because applying them would
    /// close a feedback loop. Distinct from `unresolved`: the endpoints
    /// exist, the wiring is just dangerous.
    pub rejected: Vec<String>,
}

/// Normalize an alias/port name for route matching: lowercase, drop a
/// leading `"N - "` channel-number prefix and a trailing `[DSP]`,
/// collapse whitespace.
///
/// So `"81 - Engineer Vocal [DSP]"` and `"Engineer Vocal"` compare equal
/// — but `" L"` / `" R"` is kept, because stereo halves are genuinely
/// different channels and merging them would cross-wire a mix.
pub(crate) fn norm_name(s: &str) -> String {
    let s = s.trim();
    // Strip a leading "<digits> - ".
    let s = match s.split_once(" - ") {
        Some((pre, rest)) if !pre.is_empty() && pre.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => s,
    };
    let s = s.trim();
    // Strip a trailing "[DSP]" (any case). Split on a char boundary
    // rather than subtracting a length from a lowercased copy —
    // `to_lowercase` can change byte length.
    let s = s
        .char_indices()
        .rev()
        .nth("[dsp]".len().saturating_sub(1))
        .map_or(s, |(i, _)| {
            let (head, tail) = s.split_at(i);
            if tail.eq_ignore_ascii_case("[dsp]") {
                head
            } else {
                s
            }
        });
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Resolve a node by `node.name` / label / alias (case-insensitive).
pub(crate) fn resolve_node<'a>(
    store: &'a GraphStore,
    aliases: &HashMap<String, String>,
    query: &str,
) -> Option<&'a PwNode> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    store.nodes.values().find(|n| {
        n.name.to_lowercase() == q
            || n.label.to_lowercase() == q
            || aliases.get(&n.name).map(|s| s.to_lowercase()).as_deref() == Some(q.as_str())
    })
}

/// Resolve one route endpoint to a live port id of the given direction,
/// matching the port's ALIAS (or raw name) normalized. `ep.node`, when
/// set, narrows to a node whose name / label / alias matches.
pub(crate) fn resolve_endpoint(
    store: &GraphStore,
    aliases: &HashMap<String, String>,
    ep: &RouteEndpoint,
    dir: PortDirection,
) -> Option<u32> {
    let want_port = norm_name(&ep.port);
    if want_port.is_empty() {
        return None;
    }
    let want_node = ep.node.trim().to_lowercase();
    // Deterministic across HashMap orders: lowest matching port id wins.
    let mut best: Option<u32> = None;
    for p in store.ports.values() {
        if p.direction != dir {
            continue;
        }
        let Some(node) = store.nodes.get(&p.node_id) else {
            continue;
        };
        if !want_node.is_empty() {
            let node_alias = aliases
                .get(&node.name)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if node.name.to_lowercase() != want_node
                && node.label.to_lowercase() != want_node
                && node_alias != want_node
            {
                continue;
            }
        }
        let alias = aliases.get(&format!("{}:{}", node.name, p.name));
        let cand = alias.map_or(p.name.as_str(), String::as_str);
        if norm_name(cand) == want_port {
            best = Some(best.map_or(p.id, |b: u32| b.min(p.id)));
        }
    }
    best
}

/// Outcome of considering one `out → inp` pair.
enum Considered {
    /// Emit this command.
    Create(Command),
    /// Nothing to do — the link is already there, or an endpoint's node
    /// has gone.
    Skip,
    /// Refused: applying it would close a feedback loop.
    Cycle,
}

/// Decide whether `out → inp` should be linked, consulting `guard` and
/// recording an accepted link back into it.
fn consider(store: &GraphStore, guard: &mut CycleGuard, out: u32, inp: u32) -> Considered {
    if store.link_between(out, inp).is_some() {
        return Considered::Skip;
    }
    let (Some(on), Some(inn)) = (store.node_of_port(out), store.node_of_port(inp)) else {
        return Considered::Skip;
    };
    if guard.would_cycle(on.id, inn.id) {
        return Considered::Cycle;
    }
    guard.accept(on.id, inn.id);
    Considered::Create(Command::CreateLink {
        output_node: on.id,
        output_port: out,
        input_node: inn.id,
        input_port: inp,
    })
}

/// Plan every enabled route against the live graph.
///
/// Pure and idempotent: re-planning against the graph these commands
/// produce yields an empty command list.
pub(crate) fn plan(
    store: &GraphStore,
    routes: &[NamedRoute],
    aliases: &HashMap<String, String>,
) -> RoutePlan {
    let mut out = RoutePlan::default();
    let mut guard = CycleGuard::new(store);
    for route in routes.iter().filter(|r| r.enabled) {
        let pairs = if route.from.port.trim() == BANK_PORT || route.to.port.trim() == BANK_PORT {
            // Whole-node bank route: pair 1:1 by channel.
            match (
                resolve_node(store, aliases, &route.from.node),
                resolve_node(store, aliases, &route.to.node),
            ) {
                (Some(on), Some(inn)) => pairing::pair_nodes(store, on.id, inn.id),
                _ => Vec::new(),
            }
        } else {
            match (
                resolve_endpoint(store, aliases, &route.from, PortDirection::Output),
                resolve_endpoint(store, aliases, &route.to, PortDirection::Input),
            ) {
                (Some(o), Some(i)) => vec![(o, i)],
                _ => Vec::new(),
            }
        };

        if pairs.is_empty() {
            out.unresolved.push(route.name.clone());
            continue;
        }
        out.resolved = out.resolved.saturating_add(1);
        let mut refused = false;
        for (o, i) in pairs {
            match consider(store, &mut guard, o, i) {
                Considered::Create(cmd) => out.commands.push(cmd),
                Considered::Skip => {}
                Considered::Cycle => refused = true,
            }
        }
        if refused {
            out.rejected.push(route.name.clone());
        }
    }
    out
}
