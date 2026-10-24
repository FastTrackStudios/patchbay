//! Feedback-loop detection for planned links.
//!
//! A virtual sink exposes BOTH `playback_N` (input) and `monitor_N`
//! (output), so a route can wire a bus's monitor back into its own
//! playback, and a pair of bank routes between two nodes can close a
//! loop through them. `PipeWire` refuses some direct self-links but will
//! not save you from a two-node cycle — and a feedback loop in a live
//! rig is not a cosmetic bug, it is a howl through the monitors.
//!
//! So the planners check before they emit: a link is rejected when the
//! graph already contains a path from its DESTINATION node back to its
//! SOURCE node. Node-level rather than port-level, because that is the
//! granularity at which audio actually flows through a device.

use std::collections::{HashMap, HashSet};

use crate::server::store::GraphStore;

/// Node-level adjacency of the live graph: `node → nodes it feeds`.
///
/// Self-edges are dropped: a node whose own output feeds its own input
/// (REAPER monitoring itself) is the DAW's business, not a routing loop
/// we introduced.
pub(crate) fn adjacency(store: &GraphStore) -> HashMap<u32, HashSet<u32>> {
    let mut adj: HashMap<u32, HashSet<u32>> = HashMap::new();
    for l in store.links.values() {
        if l.output_node != l.input_node {
            adj.entry(l.output_node).or_default().insert(l.input_node);
        }
    }
    adj
}

/// Is `target` reachable from `start` by following links downstream?
fn reaches(adj: &HashMap<u32, HashSet<u32>>, start: u32, target: u32) -> bool {
    let mut pending = vec![start];
    let mut seen = HashSet::new();
    while let Some(current) = pending.pop() {
        if current == target {
            return true;
        }
        if !seen.insert(current) {
            continue;
        }
        if let Some(next) = adj.get(&current) {
            pending.extend(next.iter().copied());
        }
    }
    false
}

/// Tracks which links are safe to add, accounting for the ones already
/// planned in this same pass.
///
/// Planning is a batch: two routes that are each fine on their own can
/// close a loop together, so the guard has to see the plan as it grows.
pub(crate) struct CycleGuard {
    adj: HashMap<u32, HashSet<u32>>,
}

impl CycleGuard {
    pub(crate) fn new(store: &GraphStore) -> Self {
        Self {
            adj: adjacency(store),
        }
    }

    /// Would linking `out_node → in_node` close a feedback loop?
    ///
    /// A link from a node back to itself is allowed (that is the DAW's
    /// own monitoring, and it already exists in the graph); a path back
    /// through any other node is not.
    pub(crate) fn would_cycle(&self, out_node: u32, in_node: u32) -> bool {
        out_node != in_node && reaches(&self.adj, in_node, out_node)
    }

    /// Record a link the plan is about to create, so later links in the
    /// same batch see it.
    pub(crate) fn accept(&mut self, out_node: u32, in_node: u32) {
        if out_node != in_node {
            self.adj.entry(out_node).or_default().insert(in_node);
        }
    }
}
