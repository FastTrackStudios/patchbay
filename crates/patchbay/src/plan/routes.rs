//! Named-route planning: the explicit auto-connect.
//!
//! A route says "keep this output linked to this input whenever both
//! resolve", addressed by SEMANTIC name so it survives the channel being
//! renumbered. Applying is idempotent and strictly additive — it only
//! ever creates a missing link, never tears anything down, because this
//! edits a live production graph.

use std::collections::HashMap;

use patchbay_proto::{NamedRoute, PortDirection, PwNode, RouteEndpoint};

use crate::engine::Command;
use crate::plan::cycles::CycleGuard;
use crate::plan::pairing;
use crate::store::GraphStore;

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

#[cfg(test)]
mod normalization {
    use super::norm_name;

    #[test]
    fn strips_channel_prefix_and_dsp_suffix() {
        assert_eq!(norm_name("81 - Engineer Vocal [DSP]"), "engineer vocal");
        assert_eq!(norm_name("42 - Engineer Vocal"), "engineer vocal");
        assert_eq!(norm_name("Engineer Vocal [dsp]"), "engineer vocal");
        assert_eq!(norm_name("  Engineer   Vocal  "), "engineer vocal");
    }

    /// Stereo halves must stay distinct or a route cross-wires a mix.
    #[test]
    fn keeps_left_and_right_apart() {
        assert_ne!(norm_name("Vocal 1 Mix L"), norm_name("Vocal 1 Mix R"));
    }

    /// A name that is only digits is not a channel prefix.
    #[test]
    fn does_not_eat_a_numeric_name() {
        assert_eq!(norm_name("808"), "808");
    }

    /// Regression: this used to truncate by a length measured on a
    /// lowercased copy, which is not guaranteed to be a char boundary.
    #[test]
    fn non_ascii_names_do_not_panic() {
        for name in ["Ünïcødé [DSP]", "İstanbul", "ß", "🎛 Kick [dsp]", "[DSP]"] {
            let _ = norm_name(name);
        }
        assert_eq!(norm_name("Ünïcødé [DSP]"), "ünïcødé");
    }
}

#[cfg(test)]
mod planning {
    use super::*;
    use crate::plan::fixtures::{aliases as alias_map, insert_link, node, port, store_with};

    fn route(name: &str, from: (&str, &str), to: (&str, &str)) -> NamedRoute {
        NamedRoute {
            name: name.to_owned(),
            from: RouteEndpoint {
                node: from.0.to_owned(),
                port: from.1.to_owned(),
            },
            to: RouteEndpoint {
                node: to.0.to_owned(),
                port: to.1.to_owned(),
            },
            enabled: true,
        }
    }

    /// The headline case: an Inferno channel and a REAPER input carry
    /// the same NAME but different channel numbers, and the route must
    /// still find them.
    fn rig() -> (GraphStore, HashMap<String, String>) {
        let store = store_with(
            &[node(1, "Inferno source"), node(2, "REAPER")],
            &[
                port(100, 1, "capture_96", PortDirection::Output),
                port(200, 2, "in5", PortDirection::Input),
            ],
            &[],
        );
        let aliases = alias_map(&[
            ("Inferno source:capture_96", "96 - Engineer Vocal [DSP]"),
            ("REAPER:in5", "5 - Engineer Vocal"),
        ]);
        (store, aliases)
    }

    #[test]
    fn resolves_by_alias_across_channel_numbers() {
        let (store, aliases) = rig();
        let routes = [route(
            "Vocal",
            ("Inferno source", "Engineer Vocal"),
            ("REAPER", "Engineer Vocal"),
        )];
        let plan = plan(&store, &routes, &aliases);
        assert_eq!(plan.resolved, 1);
        assert!(plan.unresolved.is_empty());
        assert_eq!(
            plan.commands,
            vec![Command::CreateLink {
                output_node: 1,
                output_port: 100,
                input_node: 2,
                input_port: 200,
            }]
        );
    }

    /// Idempotence is the safety property: this runs on every graph
    /// settle, so a second pass must be a no-op.
    #[test]
    fn is_idempotent_once_the_link_exists() {
        let (mut store, aliases) = rig();
        insert_link(&mut store, 7, 100, 200);
        let routes = [route(
            "Vocal",
            ("Inferno source", "Engineer Vocal"),
            ("REAPER", "Engineer Vocal"),
        )];
        let plan = plan(&store, &routes, &aliases);
        assert_eq!(plan.resolved, 1, "still resolves");
        assert!(plan.commands.is_empty(), "but creates nothing");
    }

    #[test]
    fn disabled_routes_are_skipped_entirely() {
        let (store, aliases) = rig();
        let mut r = route(
            "Vocal",
            ("Inferno source", "Engineer Vocal"),
            ("REAPER", "Engineer Vocal"),
        );
        r.enabled = false;
        let plan = plan(&store, &[r], &aliases);
        assert_eq!(plan, RoutePlan::default());
    }

    #[test]
    fn an_unresolved_route_is_named_not_silently_dropped() {
        let (store, aliases) = rig();
        let routes = [route(
            "Guitar",
            ("Inferno source", "Guitar 1"),
            ("REAPER", "Guitar 1"),
        )];
        let plan = plan(&store, &routes, &aliases);
        assert_eq!(plan.resolved, 0);
        assert_eq!(plan.unresolved, vec!["Guitar".to_owned()]);
        assert!(plan.commands.is_empty());
    }

    #[test]
    fn direction_is_enforced() {
        let (store, aliases) = rig();
        // Both endpoints name the OUTPUT side; the input must not match.
        let routes = [route(
            "Backwards",
            ("Inferno source", "Engineer Vocal"),
            ("Inferno source", "Engineer Vocal"),
        )];
        let plan = plan(&store, &routes, &aliases);
        assert_eq!(plan.unresolved, vec!["Backwards".to_owned()]);
    }

    #[test]
    fn node_filter_disambiguates_a_shared_alias() {
        let store = store_with(
            &[node(1, "Inferno source"), node(2, "Other Card")],
            &[
                port(100, 1, "capture_1", PortDirection::Output),
                port(101, 2, "out_1", PortDirection::Output),
                port(200, 3, "in1", PortDirection::Input),
            ],
            &[],
        );
        let aliases = alias_map(&[
            ("Inferno source:capture_1", "Talkback"),
            ("Other Card:out_1", "Talkback"),
        ]);
        let ep = RouteEndpoint {
            node: "Other Card".into(),
            port: "Talkback".into(),
        };
        assert_eq!(
            resolve_endpoint(&store, &aliases, &ep, PortDirection::Output),
            Some(101)
        );
    }

    #[test]
    fn bank_route_wires_a_whole_node_by_channel() {
        let store = store_with(
            &[node(1, "Card"), node(2, "REAPER")],
            &[
                port(10, 1, "capture_1", PortDirection::Output),
                port(11, 1, "capture_2", PortDirection::Output),
                port(20, 2, "in1", PortDirection::Input),
                port(21, 2, "in2", PortDirection::Input),
            ],
            &[],
        );
        let routes = [route("Bank", ("Card", BANK_PORT), ("REAPER", BANK_PORT))];
        let plan = plan(&store, &routes, &HashMap::new());
        assert_eq!(plan.resolved, 1);
        assert_eq!(plan.commands.len(), 2);
        assert_eq!(
            plan.commands[0],
            Command::CreateLink {
                output_node: 1,
                output_port: 10,
                input_node: 2,
                input_port: 20,
            }
        );
    }

    /// A bank route whose nodes exist but share no channel resolves to
    /// nothing, and must be reported rather than counted as applied.
    #[test]
    fn bank_route_with_no_shared_channels_is_unresolved() {
        let store = store_with(
            &[node(1, "Card"), node(2, "REAPER")],
            &[
                port(10, 1, "capture_1", PortDirection::Output),
                port(20, 2, "in9", PortDirection::Input),
            ],
            &[],
        );
        let routes = [route("Bank", ("Card", BANK_PORT), ("REAPER", BANK_PORT))];
        let plan = plan(&store, &routes, &HashMap::new());
        assert_eq!(plan.unresolved, vec!["Bank".to_owned()]);
    }

    /// The rig-safety property: a route that would feed a bus back into
    /// itself is refused, and named, rather than quietly howling.
    #[test]
    fn a_route_that_would_close_a_loop_is_rejected() {
        let mut store = store_with(
            &[node(1, "Bus"), node(2, "REAPER")],
            &[
                port(10, 1, "monitor_1", PortDirection::Output),
                port(11, 1, "playback_1", PortDirection::Input),
                port(20, 2, "out1", PortDirection::Output),
                port(21, 2, "in1", PortDirection::Input),
            ],
            &[],
        );
        // REAPER already feeds the bus.
        insert_link(&mut store, 50, 20, 11);

        // A route sending the bus monitor back into REAPER closes it.
        let routes = [route("Loop", ("Bus", "monitor_1"), ("REAPER", "in1"))];
        let plan = plan(&store, &routes, &HashMap::new());

        assert_eq!(plan.resolved, 1, "the endpoints do resolve");
        assert!(plan.commands.is_empty(), "but nothing is created");
        assert_eq!(plan.rejected, vec!["Loop".to_owned()]);
        assert!(plan.unresolved.is_empty(), "not an addressing failure");
    }

    /// Two routes that are each safe alone must not close a loop
    /// together — the guard has to accumulate across the batch.
    #[test]
    fn a_loop_formed_across_two_routes_is_caught() {
        let store = store_with(
            &[node(1, "A"), node(2, "B")],
            &[
                port(10, 1, "out1", PortDirection::Output),
                port(11, 1, "in1", PortDirection::Input),
                port(20, 2, "out1", PortDirection::Output),
                port(21, 2, "in1", PortDirection::Input),
            ],
            &[],
        );
        let routes = [
            route("A to B", ("A", "out1"), ("B", "in1")),
            route("B to A", ("B", "out1"), ("A", "in1")),
        ];
        let plan = plan(&store, &routes, &HashMap::new());
        assert_eq!(plan.commands.len(), 1, "only the first route survives");
        assert_eq!(plan.rejected, vec!["B to A".to_owned()]);
    }

    #[test]
    fn no_routes_is_an_empty_plan() {
        let (store, aliases) = rig();
        assert_eq!(plan(&store, &[], &aliases), RoutePlan::default());
    }
}
