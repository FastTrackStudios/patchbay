//! What a host backend can do.

use serde::{Deserialize, Serialize};

use crate::HostError;

/// Capability flags of a [`crate::HostBackend`]. UIs hide what a backend
/// can't do; backends refuse it with [`HostError::Unsupported`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // a flag set is exactly what this is
pub struct HostCapabilities {
    /// `create_link` / `remove_link` work.
    pub graph_links: bool,
    /// `create_virtual_device` works.
    pub virtual_devices: bool,
    /// Individual applications' audio can be captured (per-app sources).
    pub app_capture: bool,
    /// Virtual devices other apps can *select* as their output (pass-thru).
    /// On macOS this needs an installed HAL `AudioServerPlugIn`.
    pub pass_thru_device: bool,
    /// Per-port peak meters are available.
    pub meters: bool,
}

impl HostCapabilities {
    /// Nothing supported (the non-macOS stub of a macOS backend, say).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            graph_links: false,
            virtual_devices: false,
            app_capture: false,
            pass_thru_device: false,
            meters: false,
        }
    }

    /// `Ok` if `flag` is set, else [`HostError::Unsupported`] naming `what`.
    ///
    /// # Errors
    /// [`HostError::Unsupported`].
    pub fn require(flag: bool, what: &str) -> Result<(), HostError> {
        if flag {
            Ok(())
        } else {
            Err(HostError::Unsupported(what.to_owned()))
        }
    }
}
