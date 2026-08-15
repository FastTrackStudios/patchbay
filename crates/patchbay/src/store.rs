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
