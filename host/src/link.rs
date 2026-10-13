//! Mixing links between ports.

use serde::{Deserialize, Serialize};

use crate::PortRef;

/// A link from an output port to an input port.
///
/// Host graphs **mix**: any number of links may target one input port and
/// they sum (unlike `patchbay-device` crosspoints, which replace). A link
/// is identified by its `(from, to)` pair; there is at most one link per
/// pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostLink {
    /// Source port ([`crate::PortDirection::Output`]).
    pub from: PortRef,
    /// Destination port ([`crate::PortDirection::Input`]).
    pub to: PortRef,
    /// Linear gain, from 0.0 up to [`crate::MAX_GAIN`].
    pub gain: f32,
    /// A disabled link is kept (and remembered) but passes no audio.
    pub enabled: bool,
}

impl HostLink {
    /// An enabled unity-gain link.
    #[must_use]
    pub const fn new(from: PortRef, to: PortRef) -> Self {
        Self {
            from,
            to,
            gain: 1.0,
            enabled: true,
        }
    }

    /// Whether this link connects `from` → `to`.
    #[must_use]
    pub fn connects(&self, from: &PortRef, to: &PortRef) -> bool {
        &self.from == from && &self.to == to
    }
}
