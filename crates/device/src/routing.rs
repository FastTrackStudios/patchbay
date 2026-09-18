//! Router model: port groups and single-source crosspoints.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A named bank of channels on one side of a device's router, e.g.
/// `LINE IN 1-32` (input) or `DANTE OUT 33-64` (output).
///
/// `id` is stable and adapter-chosen (prefer the vendor's own group id
/// where one exists); `name` is for display.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortGroup {
    /// Stable group id, unique per device and side.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Number of channels (channel indices are `0..channels`).
    pub channels: u16,
}

/// One channel of a [`PortGroup`]. `channel` is **0-based**.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChannelRef {
    /// [`PortGroup::id`] the channel belongs to.
    pub group: String,
    /// 0-based channel index within the group.
    pub channel: u16,
}

impl ChannelRef {
    /// Convenience constructor.
    #[must_use]
    pub fn new(group: impl Into<String>, channel: u16) -> Self {
        Self {
            group: group.into(),
            channel,
        }
    }
}

impl fmt::Display for ChannelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display is 1-based, matching every hardware panel.
        write!(
            f,
            "{}:{}",
            self.group,
            u32::from(self.channel).saturating_add(1)
        )
    }
}

/// One router cell: the output channel and the single source feeding it
/// (`None` = silence / unpatched).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Crosspoint {
    /// Destination (a channel of an output group).
    pub output: ChannelRef,
    /// Source (a channel of an input group), or `None`.
    pub source: Option<ChannelRef>,
}
