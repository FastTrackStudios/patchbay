//! Change notifications.

use serde::{Deserialize, Serialize};

use crate::{Crosspoint, ParamValue};

/// Something changed on a device. Delivered through
/// [`crate::DeviceAdapter::subscribe`].
///
/// Events reflect what the **device** reports (reads, notifications,
/// periodic state), not what patchbay asked for — hardware panels and
/// other clients write too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DeviceEvent {
    /// A parameter took a new value.
    ParamChanged {
        /// [`crate::Param::path`].
        path: String,
        /// New value.
        value: ParamValue,
    },
    /// A router cell changed (or was re-reported).
    RouteChanged(Crosspoint),
    /// The control connection came up.
    Online,
    /// The control connection dropped.
    Offline,
    /// State changed wholesale (preset recall, reconnect); re-read with
    /// [`crate::DeviceAdapter::snapshot`].
    SnapshotReplaced,
}
