//! Full device state at one moment.

use serde::{Deserialize, Serialize};

use crate::{ChannelRef, Crosspoint, DeviceInfo, Param, PortGroup};

/// Everything patchbay knows about a device, read from the device itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    /// Identity and connection state.
    pub info: DeviceInfo,
    /// Router source groups.
    pub inputs: Vec<PortGroup>,
    /// Router destination groups.
    pub outputs: Vec<PortGroup>,
    /// One entry per output channel (router semantics).
    pub routes: Vec<Crosspoint>,
    /// All parameters, including channel metadata.
    pub params: Vec<Param>,
}

impl DeviceSnapshot {
    /// The source currently feeding `output`, if the output exists.
    /// `Some(None)` means "exists, unpatched".
    #[must_use]
    pub fn source_of(&self, output: &ChannelRef) -> Option<Option<&ChannelRef>> {
        self.routes
            .iter()
            .find(|c| &c.output == output)
            .map(|c| c.source.as_ref())
    }

    /// Look up a parameter by path.
    #[must_use]
    pub fn param(&self, path: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.path == path)
    }

    /// Look up an input group by id.
    #[must_use]
    pub fn input(&self, id: &str) -> Option<&PortGroup> {
        self.inputs.iter().find(|g| g.id == id)
    }

    /// Look up an output group by id.
    #[must_use]
    pub fn output(&self, id: &str) -> Option<&PortGroup> {
        self.outputs.iter().find(|g| g.id == id)
    }
}
