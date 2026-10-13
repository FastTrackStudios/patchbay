//! Channel pairing: match two nodes' ports 1:1 by numeric suffix.
//!
//! `out7` → `playback_7`, `capture_23` → `in23`. This is the direct-path
//! bulk wiring tool — plain links add zero latency, unlike routing audio
//! through a loopback sink — and it backs both the `connect_one_to_one`
//! RPC and whole-node BANK routes.

use std::collections::BTreeMap;

use patchbay_proto::{MediaKind, PortDirection};

use crate::store::GraphStore;

/// `channel number → port id` for one node and direction.
///
/// MIDI ports are excluded: REAPER exposes both `in18` (audio) and
/// `MIDI Input 18` on the same node, which collide on channel 18, and
/// bulk wiring must land on the AUDIO port. Lowest port id wins a tie so
/// the mapping is deterministic regardless of `HashMap` iteration order.
pub(crate) fn by_channel(
    store: &GraphStore,
    node_id: u32,
    direction: PortDirection,
) -> BTreeMap<u32, u32> {
    let mut m: BTreeMap<u32, u32> = BTreeMap::new();
    for p in store.ports.values() {
        if p.node_id != node_id || p.direction != direction || p.media_kind == MediaKind::Midi {
            continue;
        }
        let Some(channel) = patchbay_proto::channel_of_port(&p.name) else {
            continue;
        };
        m.entry(channel)
            .and_modify(|existing| *existing = (*existing).min(p.id))
            .or_insert(p.id);
    }
    m
}

/// `(output port, input port)` pairs shared by both nodes, in channel
/// order. A channel present on only one side is skipped.
pub(crate) fn pair_nodes(store: &GraphStore, out_node: u32, in_node: u32) -> Vec<(u32, u32)> {
    let outs = by_channel(store, out_node, PortDirection::Output);
    let ins = by_channel(store, in_node, PortDirection::Input);
    outs.into_iter()
        .filter_map(|(channel, out)| ins.get(&channel).map(|&inp| (out, inp)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::fixtures::{node, port, store_with};

    #[test]
    fn pairs_by_numeric_suffix_across_different_prefixes() {
        let store = store_with(
            &[node(1, "REAPER"), node(2, "Inferno sink")],
            &[
                port(10, 1, "out1", PortDirection::Output),
                port(11, 1, "out2", PortDirection::Output),
                port(20, 2, "playback_1", PortDirection::Input),
                port(21, 2, "playback_2", PortDirection::Input),
            ],
            &[],
        );
        assert_eq!(pair_nodes(&store, 1, 2), vec![(10, 20), (11, 21)]);
    }

    /// The bug this guards: REAPER carries `in18` AND `MIDI Input 18`.
    /// Both end in 18, so a naive pairing could wire audio into the MIDI
    /// port depending on hash order.
    #[test]
    fn midi_ports_never_win_a_channel() {
        let store = store_with(
            &[node(1, "Card"), node(2, "REAPER")],
            &[
                port(10, 1, "capture_18", PortDirection::Output),
                // Deliberately a LOWER id than the audio port, so "first
                // wins" would pick it.
                {
                    let mut p = port(19, 2, "MIDI Input 18", PortDirection::Input);
                    p.media_kind = MediaKind::Midi;
                    p
                },
                port(20, 2, "in18", PortDirection::Input),
            ],
            &[],
        );
        assert_eq!(
            pair_nodes(&store, 1, 2),
            vec![(10, 20)],
            "channel 18 must land on the audio port, not the MIDI one"
        );
    }

    #[test]
    fn channels_present_on_only_one_side_are_skipped() {
        let store = store_with(
            &[node(1, "A"), node(2, "B")],
            &[
                port(10, 1, "out1", PortDirection::Output),
                port(11, 1, "out9", PortDirection::Output),
                port(20, 2, "in1", PortDirection::Input),
            ],
            &[],
        );
        assert_eq!(pair_nodes(&store, 1, 2), vec![(10, 20)]);
    }

    #[test]
    fn non_numeric_ports_do_not_participate() {
        let store = store_with(
            &[node(1, "A"), node(2, "B")],
            &[
                port(10, 1, "capture_FL", PortDirection::Output),
                port(20, 2, "playback_FL", PortDirection::Input),
            ],
            &[],
        );
        assert!(pair_nodes(&store, 1, 2).is_empty());
    }

    #[test]
    fn duplicate_channel_resolves_to_the_lowest_port_id() {
        let store = store_with(
            &[node(1, "A"), node(2, "B")],
            &[
                port(30, 1, "out5", PortDirection::Output),
                port(12, 1, "monitor_5", PortDirection::Output),
                port(20, 2, "in5", PortDirection::Input),
            ],
            &[],
        );
        assert_eq!(
            pair_nodes(&store, 1, 2),
            vec![(12, 20)],
            "deterministic regardless of HashMap order"
        );
    }
}
