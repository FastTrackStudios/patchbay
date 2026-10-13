//! Shared graph mirror — written by the engine thread, read by the
//! service impl (snapshots, preset resolution).

use std::collections::HashMap;

use patchbay_proto::{GraphSnapshot, PwLink, PwNode, PwPort};

#[derive(Default)]
pub(crate) struct GraphStore {
    pub nodes: HashMap<u32, PwNode>,
    pub ports: HashMap<u32, PwPort>,
    pub links: HashMap<u32, PwLink>,
}

impl GraphStore {
    pub fn snapshot(&self) -> GraphSnapshot {
        let mut snap = GraphSnapshot {
            nodes: self.nodes.values().cloned().collect(),
            ports: self.ports.values().cloned().collect(),
            links: self.links.values().cloned().collect(),
        };
        // Deterministic order keeps UI layout + preset diffs stable.
        snap.nodes.sort_by_key(|n| n.id);
        snap.ports.sort_by_key(|p| p.id);
        snap.links.sort_by_key(|l| l.id);
        snap
    }

    /// The node owning `port`, if both are known.
    pub fn node_of_port(&self, port_id: u32) -> Option<&PwNode> {
        self.ports
            .get(&port_id)
            .and_then(|p| self.nodes.get(&p.node_id))
    }

    /// Does any link already connect these two ports?
    pub fn link_between(&self, output_port: u32, input_port: u32) -> Option<u32> {
        self.links
            .values()
            .find(|l| l.output_port == output_port && l.input_port == input_port)
            .map(|l| l.id)
    }

    /// Resolve a (node.name, port.name) pair to a live port id.
    pub fn port_by_names(&self, node_name: &str, port_name: &str) -> Option<u32> {
        let node = self.nodes.values().find(|n| n.name == node_name)?;
        self.ports
            .values()
            .find(|p| p.node_id == node.id && p.name == port_name)
            .map(|p| p.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::fixtures::{link, node, port, store_with};
    use patchbay_proto::PortDirection;

    fn rig() -> GraphStore {
        store_with(
            &[node(1, "REAPER"), node(2, "Inferno sink")],
            &[
                port(10, 1, "out1", PortDirection::Output),
                port(20, 2, "playback_1", PortDirection::Input),
            ],
            &[link(50, 10, 20)],
        )
    }

    /// The UI applies incremental events on top of a snapshot, and both
    /// sides binary-search by id — so the snapshot MUST come out sorted.
    #[test]
    fn snapshot_is_sorted_by_id() {
        let mut store = GraphStore::default();
        for id in [9_u32, 3, 7, 1] {
            store.nodes.insert(id, node(id, &format!("n{id}")));
            store
                .ports
                .insert(id, port(id, id, "out1", PortDirection::Output));
        }
        let snap = store.snapshot();
        let node_ids: Vec<u32> = snap.nodes.iter().map(|n| n.id).collect();
        let port_ids: Vec<u32> = snap.ports.iter().map(|p| p.id).collect();
        assert_eq!(node_ids, vec![1, 3, 7, 9]);
        assert_eq!(port_ids, vec![1, 3, 7, 9]);
    }

    #[test]
    fn node_of_port_resolves_through_the_port() {
        let store = rig();
        assert_eq!(store.node_of_port(10).map(|n| n.id), Some(1));
        assert_eq!(store.node_of_port(20).map(|n| n.id), Some(2));
        assert!(store.node_of_port(999).is_none());
    }

    /// A port whose node has already been removed must not resolve —
    /// planners rely on this to skip half-torn-down endpoints.
    #[test]
    fn node_of_port_is_none_when_the_node_is_gone() {
        let mut store = rig();
        store.nodes.remove(&1);
        assert!(store.node_of_port(10).is_none());
    }

    #[test]
    fn link_between_is_direction_sensitive() {
        let store = rig();
        assert_eq!(store.link_between(10, 20), Some(50));
        assert_eq!(
            store.link_between(20, 10),
            None,
            "output and input are not interchangeable"
        );
    }

    #[test]
    fn port_by_names_round_trips() {
        let store = rig();
        assert_eq!(store.port_by_names("REAPER", "out1"), Some(10));
        assert_eq!(store.port_by_names("REAPER", "nope"), None);
        assert_eq!(store.port_by_names("Nobody", "out1"), None);
    }
}
