//! Preset application — connection memory, à la `RaySession`'s jackpatch.
//!
//! A preset remembers links by stable `(node.name, port.name)` pairs and
//! re-applies them against whatever half of the graph currently exists.
//! Endpoints that aren't present are REPORTED, not fatal.
//!
//! `exclusive` additionally tears down live links the preset doesn't
//! contain. That is the one destructive operation in the whole crate, so
//! it is planned here where it can be tested exhaustively rather than
//! decided inline against a live graph.

use std::collections::HashSet;

use crate::proto::{ApplyReport, PresetLink, RoutingPreset};

use crate::server::engine::Command;
use crate::server::store::GraphStore;

/// What applying a preset would do.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct PresetPlan {
    /// Creates first, then (in exclusive mode) destroys.
    pub commands: Vec<Command>,
    /// Counts + the links whose endpoints aren't in the graph.
    pub report: ApplyReport,
}

/// Plan `preset` against the live graph.
///
/// Pure and idempotent: re-planning against the resulting graph yields
/// no commands and moves every link from `created` to `existing`.
pub(crate) fn plan(store: &GraphStore, preset: &RoutingPreset, exclusive: bool) -> PresetPlan {
    let mut out = PresetPlan::default();

    for link in &preset.links {
        let resolved = store
            .port_by_names(&link.output_node, &link.output_port)
            .zip(store.port_by_names(&link.input_node, &link.input_port));
        let Some((out_port, in_port)) = resolved else {
            out.report.missing.push(link.clone());
            continue;
        };
        if store.link_between(out_port, in_port).is_some() {
            out.report.existing = out.report.existing.saturating_add(1);
            continue;
        }
        let (Some(on), Some(inn)) = (store.node_of_port(out_port), store.node_of_port(in_port))
        else {
            out.report.missing.push(link.clone());
            continue;
        };
        out.commands.push(Command::CreateLink {
            output_node: on.id,
            output_port: out_port,
            input_node: inn.id,
            input_port: in_port,
        });
        out.report.created = out.report.created.saturating_add(1);
    }

    if exclusive {
        let wanted: HashSet<&PresetLink> = preset.links.iter().collect();
        // Deterministic order so a test can assert on the command list.
        let mut extras: Vec<u32> = store
            .links
            .values()
            .filter_map(|l| {
                let named = PresetLink {
                    output_node: store.nodes.get(&l.output_node)?.name.clone(),
                    output_port: store.ports.get(&l.output_port)?.name.clone(),
                    input_node: store.nodes.get(&l.input_node)?.name.clone(),
                    input_port: store.ports.get(&l.input_port)?.name.clone(),
                };
                (!wanted.contains(&named)).then_some(l.id)
            })
            .collect();
        extras.sort_unstable();
        out.report.destroyed = u32::try_from(extras.len()).unwrap_or(u32::MAX);
        out.commands
            .extend(extras.into_iter().map(|id| Command::DestroyLink { id }));
    }

    out
}
