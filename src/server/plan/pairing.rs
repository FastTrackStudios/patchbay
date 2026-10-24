//! Channel pairing: match two nodes' ports 1:1 by numeric suffix.
//!
//! `out7` → `playback_7`, `capture_23` → `in23`. This is the direct-path
//! bulk wiring tool — plain links add zero latency, unlike routing audio
//! through a loopback sink — and it backs both the `connect_one_to_one`
//! RPC and whole-node BANK routes.

use std::collections::BTreeMap;

use crate::proto::{MediaKind, PortDirection};

use crate::server::store::GraphStore;

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
        let Some(channel) = crate::proto::channel_of_port(&p.name) else {
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
