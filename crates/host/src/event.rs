//! Change notifications.

use serde::{Deserialize, Serialize};

use crate::{HostLink, HostNode, PortRef};

/// Something changed in the host graph. Delivered through
/// [`crate::HostBackend::subscribe`].
///
/// Events reflect what the **OS** reports (device hot-plug, apps starting
/// to play), not only what patchbay asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    /// A node appeared.
    NodeAdded(HostNode),
    /// A node's properties changed (ports, rate, running state).
    NodeChanged(HostNode),
    /// A node disappeared.
    NodeRemoved {
        /// [`HostNode::id`].
        id: String,
    },
    /// A link was created.
    LinkAdded(HostLink),
    /// A link's gain / enabled state changed.
    LinkChanged(HostLink),
    /// A link was removed.
    LinkRemoved {
        /// Source port.
        from: PortRef,
        /// Destination port.
        to: PortRef,
    },
    /// The system default device changed.
    DefaultDeviceChanged {
        /// `true` for the default output (playback) device, `false` for
        /// the default input.
        output: bool,
        /// New default's [`HostNode::id`], if known.
        id: Option<String>,
    },
    /// State changed wholesale or events were lost; re-read with
    /// [`crate::HostBackend::snapshot`].
    SnapshotReplaced,
}
