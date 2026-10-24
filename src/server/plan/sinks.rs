//! Virtual-sink reconciliation.
//!
//! A virtual sink is a named bus (a `support.null-audio-sink`). They are
//! persisted in config and re-created whenever the engine reconnects,
//! because `object.linger` keeps a sink alive across THIS process
//! exiting but not across a `PipeWire` restart.

use crate::proto::{VirtualSink, sink_node_name};

use crate::server::engine::Command;
use crate::server::store::GraphStore;

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
