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

use patchbay_proto::{ApplyReport, PresetLink, RoutingPreset};

use crate::engine::Command;
use crate::store::GraphStore;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::fixtures::{insert_link, node, port, store_with};
    use patchbay_proto::PortDirection;

    fn preset(links: &[PresetLink]) -> RoutingPreset {
        RoutingPreset {
            name: "FOH".to_owned(),
            description: String::new(),
            links: links.to_vec(),
        }
    }

    fn plink(on: &str, op: &str, inn: &str, ip: &str) -> PresetLink {
        PresetLink {
            output_node: on.to_owned(),
            output_port: op.to_owned(),
            input_node: inn.to_owned(),
            input_port: ip.to_owned(),
        }
    }

    fn rig() -> GraphStore {
        store_with(
            &[node(1, "REAPER"), node(2, "Inferno sink")],
            &[
                port(10, 1, "out1", PortDirection::Output),
                port(11, 1, "out2", PortDirection::Output),
                port(20, 2, "playback_1", PortDirection::Input),
                port(21, 2, "playback_2", PortDirection::Input),
            ],
            &[],
        )
    }

    #[test]
    fn creates_links_whose_endpoints_are_live() {
        let p = preset(&[plink("REAPER", "out1", "Inferno sink", "playback_1")]);
        let out = plan(&rig(), &p, false);
        assert_eq!(out.report.created, 1);
        assert_eq!(out.report.existing, 0);
        assert!(out.report.missing.is_empty());
        assert_eq!(
            out.commands,
            vec![Command::CreateLink {
                output_node: 1,
                output_port: 10,
                input_node: 2,
                input_port: 20,
            }]
        );
    }

    #[test]
    fn an_absent_endpoint_is_reported_not_fatal() {
        let p = preset(&[
            plink("REAPER", "out1", "Inferno sink", "playback_1"),
            plink("REAPER", "out99", "Inferno sink", "playback_99"),
        ]);
        let out = plan(&rig(), &p, false);
        assert_eq!(out.report.created, 1, "the live half still applies");
        assert_eq!(out.report.missing.len(), 1);
        assert_eq!(out.report.missing[0].output_port, "out99");
    }

    #[test]
    fn an_existing_link_counts_as_existing_and_sends_nothing() {
        let mut store = rig();
        insert_link(&mut store, 50, 10, 20);

        let p = preset(&[plink("REAPER", "out1", "Inferno sink", "playback_1")]);
        let out = plan(&store, &p, false);
        assert_eq!(out.report.existing, 1);
        assert_eq!(out.report.created, 0);
        assert!(out.commands.is_empty());
    }

    /// Exclusive mode is the only destructive path in the crate. It must
    /// destroy exactly the links the preset does NOT name — never the
    /// ones it does.
    #[test]
    fn exclusive_destroys_only_links_outside_the_preset() {
        let mut store = rig();
        insert_link(&mut store, 50, 10, 20);
        insert_link(&mut store, 51, 11, 21);
        // Preset names only the first link.
        let p = preset(&[plink("REAPER", "out1", "Inferno sink", "playback_1")]);
        let out = plan(&store, &p, true);

        assert_eq!(out.report.existing, 1, "the named link is kept");
        assert_eq!(out.report.destroyed, 1);
        assert_eq!(
            out.commands,
            vec![Command::DestroyLink { id: 51 }],
            "only the unnamed link is torn down"
        );
    }

    #[test]
    fn non_exclusive_never_destroys_anything() {
        let mut store = rig();
        insert_link(&mut store, 51, 11, 21);

        let p = preset(&[plink("REAPER", "out1", "Inferno sink", "playback_1")]);
        let out = plan(&store, &p, false);
        assert_eq!(out.report.destroyed, 0);
        assert!(
            out.commands
                .iter()
                .all(|c| !matches!(c, Command::DestroyLink { .. }))
        );
    }

    /// Applying an empty preset exclusively clears the graph — that is
    /// the intended "wipe to nothing", and it should be unambiguous.
    #[test]
    fn empty_exclusive_preset_tears_everything_down() {
        let mut store = rig();
        insert_link(&mut store, 50, 10, 20);

        let out = plan(&store, &preset(&[]), true);
        assert_eq!(out.report.destroyed, 1);
        assert_eq!(out.commands, vec![Command::DestroyLink { id: 50 }]);
    }

    #[test]
    fn is_idempotent_across_a_second_apply() {
        let store = rig();
        let p = preset(&[plink("REAPER", "out1", "Inferno sink", "playback_1")]);
        let first = plan(&store, &p, false);
        assert_eq!(first.report.created, 1);

        // Apply the plan's effect, then re-plan.
        let mut after = store;
        insert_link(&mut after, 50, 10, 20);

        let second = plan(&after, &p, false);
        assert!(second.commands.is_empty());
        assert_eq!(second.report.existing, 1);
        assert_eq!(second.report.created, 0);
    }
}
