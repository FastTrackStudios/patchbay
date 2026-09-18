//! Device identity.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable, human-readable device identity: `vendor:model:serial`, e.g.
/// `antelope:galaxy32:4202524000109`.
///
/// Like `PipeWire` node names, this is what presets persist — it must
/// survive reconnects, port changes and reboots, so adapters build it from
/// the hardware serial, never from a transport address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// Wrap an already-formatted id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Build the conventional `vendor:model:serial` form.
    #[must_use]
    pub fn from_parts(vendor: &str, model: &str, serial: &str) -> Self {
        Self(format!("{vendor}:{model}:{serial}"))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for DeviceId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// How patchbay reaches the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    /// A TCP control connection (`host:port`), optionally through a
    /// vendor service (e.g. Antelope's Manager Server).
    Tcp {
        /// `host:port` of the control endpoint.
        addr: String,
        /// Vendor service in between, if any.
        via: Option<String>,
    },
    /// Anything else (UDP control, USB/HID, …), described freely.
    Other {
        /// Free-form description.
        description: String,
    },
}

/// Static-ish facts about one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Stable identity.
    pub id: DeviceId,
    /// Manufacturer, e.g. `Antelope Audio`.
    pub vendor: String,
    /// Model, e.g. `Galaxy32`.
    pub model: String,
    /// Hardware serial number, when the device reports one.
    pub serial: Option<String>,
    /// Firmware version string, when known.
    pub firmware: Option<String>,
    /// How the device is reached.
    pub transport: Transport,
    /// Whether the control connection is currently up.
    pub online: bool,
}
