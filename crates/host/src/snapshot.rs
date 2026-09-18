//! Full graph state.

use serde::{Deserialize, Serialize};

use crate::{HostCapabilities, HostLink, HostNode, PortRef};

/// Everything a backend knows, at one instant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostSnapshot {
    /// Backend name (`coreaudio`, `pipewire`).
    pub backend: String,
    /// What the backend can do.
    pub capabilities: HostCapabilities,
    /// All nodes, sorted by id.
    pub nodes: Vec<HostNode>,
    /// All links patchbay knows about.
    pub links: Vec<HostLink>,
}

impl HostSnapshot {
    /// Node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&HostNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Whether `port` exists.
    #[must_use]
    pub fn has_port(&self, port: &PortRef) -> bool {
        self.node(&port.node).is_some_and(|n| n.has_port(port))
    }

    /// The link `from` → `to`, if any.
    #[must_use]
    pub fn link(&self, from: &PortRef, to: &PortRef) -> Option<&HostLink> {
        self.links.iter().find(|l| l.connects(from, to))
    }
}
