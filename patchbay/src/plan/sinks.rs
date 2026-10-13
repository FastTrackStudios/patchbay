//! Virtual-sink reconciliation.
//!
//! A virtual sink is a named bus (a `support.null-audio-sink`). They are
//! persisted in config and re-created whenever the engine reconnects,
//! because `object.linger` keeps a sink alive across THIS process
//! exiting but not across a `PipeWire` restart.

use patchbay_proto::{VirtualSink, sink_node_name};

use crate::engine::Command;
use crate::store::GraphStore;

/// Channel counts the adapter will accept. Mirrors the RPC-level guard
/// so a config hand-edited past the UI still can't ask for something
/// absurd.
pub(crate) const MAX_CHANNELS: u32 = 64;

/// Commands to create every configured sink that isn't already live.
///
/// Pure and idempotent: re-planning against a graph containing these
/// sinks returns an empty list.
pub(crate) fn plan(store: &GraphStore, configured: &[VirtualSink]) -> Vec<Command> {
    configured
        .iter()
        .filter_map(|sink| {
            let node_name = sink_node_name(&sink.name);
            let live = store.nodes.values().any(|n| n.name == node_name);
            if live {
                return None;
            }
            Some(Command::CreateVirtualSink {
                node_name,
                description: sink.name.clone(),
                channels: sink.channels.clamp(1, MAX_CHANNELS),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::fixtures::{node, store_with};

    fn sink(name: &str, channels: u32) -> VirtualSink {
        VirtualSink {
            name: name.to_owned(),
            channels,
            capturable: false,
        }
    }

    #[test]
    fn creates_a_configured_sink_that_is_not_live() {
        let store = store_with(&[], &[], &[]);
        let plan = plan(&store, &[sink("Stems Bus", 8)]);
        assert_eq!(
            plan,
            vec![Command::CreateVirtualSink {
                node_name: "patchbay.stems_bus".to_owned(),
                description: "Stems Bus".to_owned(),
                channels: 8,
            }]
        );
    }

    /// Runs on every settle, so it must not keep re-creating what exists.
    #[test]
    fn is_idempotent_when_the_sink_is_already_live() {
        let store = store_with(&[node(1, "patchbay.stems_bus")], &[], &[]);
        assert!(plan(&store, &[sink("Stems Bus", 8)]).is_empty());
    }

    #[test]
    fn matches_on_the_derived_node_name_not_the_display_name() {
        // A node literally called "Stems Bus" is NOT our sink.
        let store = store_with(&[node(1, "Stems Bus")], &[], &[]);
        assert_eq!(plan(&store, &[sink("Stems Bus", 8)]).len(), 1);
    }

    #[test]
    fn channel_count_is_clamped_into_range() {
        let store = store_with(&[], &[], &[]);
        let out = plan(&store, &[sink("Zero", 0), sink("Huge", 9999)]);
        let channels: Vec<u32> = out
            .iter()
            .map(|c| match c {
                Command::CreateVirtualSink { channels, .. } => *channels,
                _ => panic!("expected CreateVirtualSink"),
            })
            .collect();
        assert_eq!(channels, vec![1, MAX_CHANNELS]);
    }

    #[test]
    fn plans_only_the_missing_half_of_a_mixed_set() {
        let store = store_with(&[node(1, "patchbay.stems_bus")], &[], &[]);
        let out = plan(&store, &[sink("Stems Bus", 8), sink("Cue Bus", 2)]);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            Command::CreateVirtualSink { description, .. } if description == "Cue Bus"
        ));
    }
}
