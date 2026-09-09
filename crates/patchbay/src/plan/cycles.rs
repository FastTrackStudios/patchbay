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

use crate::store::GraphStore;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::fixtures::{insert_link, node, port, store_with};
    use patchbay_proto::PortDirection;

    /// Two nodes, each with an output and an input port, so any pair can
    /// be linked in either direction.
    fn duplex_pair() -> GraphStore {
        store_with(
            &[node(1, "A"), node(2, "B"), node(3, "C")],
            &[
                port(10, 1, "out1", PortDirection::Output),
                port(11, 1, "in1", PortDirection::Input),
                port(20, 2, "out1", PortDirection::Output),
                port(21, 2, "in1", PortDirection::Input),
                port(30, 3, "out1", PortDirection::Output),
                port(31, 3, "in1", PortDirection::Input),
            ],
            &[],
        )
    }

    #[test]
    fn an_empty_graph_has_no_cycles() {
        let guard = CycleGuard::new(&duplex_pair());
        assert!(!guard.would_cycle(1, 2));
    }

    #[test]
    fn the_reverse_of_an_existing_link_is_a_cycle() {
        let mut store = duplex_pair();
        // A → B already exists.
        insert_link(&mut store, 50, 10, 21);

        let guard = CycleGuard::new(&store);
        assert!(guard.would_cycle(2, 1), "B → A closes the loop");
        assert!(!guard.would_cycle(1, 3), "A → C is still fine");
    }

    #[test]
    fn detects_a_loop_through_an_intermediate_node() {
        let mut store = duplex_pair();
        // A → B → C exists; C → A would close a three-node loop.
        insert_link(&mut store, 50, 10, 21);
        insert_link(&mut store, 51, 20, 31);
        let guard = CycleGuard::new(&store);
        assert!(guard.would_cycle(3, 1), "C → A closes A→B→C→A");
        assert!(!guard.would_cycle(1, 3), "A → C is a shortcut, not a loop");
    }

    /// A node feeding itself is the DAW monitoring itself — pre-existing
    /// and none of our business. Rejecting it would block legitimate
    /// bank routes onto a duplex device.
    #[test]
    fn a_self_link_is_not_treated_as_a_cycle() {
        let guard = CycleGuard::new(&duplex_pair());
        assert!(!guard.would_cycle(1, 1));
    }

    /// The batch case: two routes that are individually safe close a
    /// loop together, so the guard must see the plan as it is built.
    #[test]
    fn accumulates_within_one_planning_pass() {
        let mut guard = CycleGuard::new(&duplex_pair());
        assert!(!guard.would_cycle(1, 2));
        guard.accept(1, 2);
        assert!(
            guard.would_cycle(2, 1),
            "the second half of the pass must see the first"
        );
    }

    #[test]
    fn a_diamond_is_not_a_cycle() {
        let mut store = duplex_pair();
        // A → B and A → C: both fed from A, no path back.
        insert_link(&mut store, 50, 10, 21);
        insert_link(&mut store, 51, 10, 31);
        let guard = CycleGuard::new(&store);
        assert!(!guard.would_cycle(2, 3), "B → C joins two siblings");
    }
}
