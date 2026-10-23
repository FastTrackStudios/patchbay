//! Nodes and ports of the host graph.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A physical (or driver-backed) audio device: built-in speakers, a USB
    /// interface, a Dante Virtual Soundcard.
    HardwareDevice,
    /// A software device other apps can select (HAL plug-in, `PipeWire`
    /// null sink, one of ours).
    VirtualDevice,
    /// One application's audio stream (a process on macOS, a stream node
    /// on `PipeWire`).
    AppStream,
    /// A device composed of other devices / taps (Core Audio aggregate).
    Aggregate,
}

/// Direction of a single port, **graph perspective**.
///
/// An `Output` port produces audio and can be a link's `from`; an `Input`
/// port consumes audio and can be a link's `to`. A microphone's capture
/// channels are therefore graph *outputs*, a speaker's playback channels
/// graph *inputs*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortDirection {
    /// Consumes audio (playback channels, a virtual device's sources side).
    Input,
    /// Produces audio (capture channels, app streams).
    Output,
}

/// Which port directions a node has, derived from its [`PortCounts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeDirection {
    /// Only output ports (produces audio).
    Source,
    /// Only input ports (consumes audio).
    Sink,
    /// Both.
    Duplex,
    /// No ports (e.g. an idle process, a device with no active streams).
    None,
}

impl NodeDirection {
    /// Derive the direction from port counts.
    #[must_use]
    pub const fn from_counts(counts: PortCounts) -> Self {
        match (counts.inputs > 0, counts.outputs > 0) {
            (true, true) => Self::Duplex,
            (true, false) => Self::Sink,
            (false, true) => Self::Source,
            (false, false) => Self::None,
        }
    }
}

/// Port counts of a node, graph perspective (see [`PortDirection`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortCounts {
    /// Ports that consume audio.
    pub inputs: u32,
    /// Ports that produce audio.
    pub outputs: u32,
}

impl PortCounts {
    /// Count for one direction.
    #[must_use]
    pub const fn get(self, direction: PortDirection) -> u32 {
        match direction {
            PortDirection::Input => self.inputs,
            PortDirection::Output => self.outputs,
        }
    }
}

/// The application behind an [`NodeKind::AppStream`] node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppInfo {
    /// OS process id. Changes every launch — never persist it.
    pub pid: i32,
    /// Bundle id (macOS) / `application.id` (`PipeWire`), the identity to
    /// persist. `None` for unbundled binaries.
    pub bundle_id: Option<String>,
    /// Human-readable name.
    pub name: String,
}

/// One node of the host graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostNode {
    /// Stable, backend-namespaced identity (e.g. `coreaudio:device:<uid>`).
    /// Stable for as long as the underlying object exists; for devices it
    /// survives reboots.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What the node is.
    pub kind: NodeKind,
    /// Derived from [`Self::ports`].
    pub direction: NodeDirection,
    /// Port counts per direction.
    pub ports: PortCounts,
    /// Nominal sample rate, when the node has one.
    pub sample_rate: Option<f64>,
    /// Set for [`NodeKind::AppStream`] nodes.
    pub app: Option<AppInfo>,
    /// Backend-specific extras (transport type, manufacturer, running
    /// state). Informational; never part of identity.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub props: BTreeMap<String, String>,
}

impl HostNode {
    /// Whether this node has `port` (node id, direction and channel range).
    #[must_use]
    pub fn has_port(&self, port: &PortRef) -> bool {
        port.node == self.id && port.channel < self.ports.get(port.direction)
    }
}

/// One port: `(node, channel, direction)`. Channels are 0-based.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortRef {
    /// [`HostNode::id`].
    pub node: String,
    /// 0-based channel index within `direction`.
    pub channel: u32,
    /// Graph direction.
    pub direction: PortDirection,
}

impl PortRef {
    /// An output (audio-producing) port.
    #[must_use]
    pub fn output(node: impl Into<String>, channel: u32) -> Self {
        Self {
            node: node.into(),
            channel,
            direction: PortDirection::Output,
        }
    }

    /// An input (audio-consuming) port.
    #[must_use]
    pub fn input(node: impl Into<String>, channel: u32) -> Self {
        Self {
            node: node.into(),
            channel,
            direction: PortDirection::Input,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_from_counts() {
        let d = |inputs, outputs| NodeDirection::from_counts(PortCounts { inputs, outputs });
        assert_eq!(d(2, 2), NodeDirection::Duplex);
        assert_eq!(d(2, 0), NodeDirection::Sink);
        assert_eq!(d(0, 1), NodeDirection::Source);
        assert_eq!(d(0, 0), NodeDirection::None);
    }
}
