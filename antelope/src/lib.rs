//! `patchbay-antelope` — Antelope Audio devices for Patchbay: a device
//! family of its own, and its own Architect extension
//! (FastTrackStudios/studio#410, #418).
//!
//! - [`register`]: add the family and its console view to this process,
//!   before the engine and the window start. [`ext::Antelope`] (feature
//!   `extension`) does it from its `build`; an app that is not an
//!   Architect host (`fts-patchbay`, its CLI, the browser remote) calls it
//!   itself.
//! - [`family`] (feature `server`): the `DeviceFamily`.
//! - [`ui`] (feature `ui`): the Galaxy32 console.
//!
//! The protocol (feature `server`):
//!
//! Talks to Antelope's **Manager Server** (the vendor daemon the official
//! panel uses) over its JSON-over-TCP control protocol:
//!
//! - [`discover`] / [`Listener`]: UDP multicast announces
//!   (`239.192.5.8:5008`) → dynamic control ports.
//! - [`Client`]: framing, the captured `initialize_format` handshake,
//!   fire-and-forget writes, `(ext2, ext3)`-correlated reads, a broadcast
//!   of cyclic state + notifications.
//! - [`Galaxy32`]: typed Galaxy32 operations and pure call builders.
//! - [`Galaxy32Adapter`]: the Galaxy32 as a [`patchbay::device::DeviceAdapter`].
//! - [`AfxCatalog`]: schema-driven AFX effect encoder.
//!
//! **Source of truth is the device.** The official Antelope panel does
//! not refresh from other clients' writes and later re-sends its stale
//! whole-page/whole-strip state; this crate always reads back after
//! writing and never trusts its own cache over the device.

#[cfg(feature = "server")]
mod client;
#[cfg(feature = "server")]
mod discovery;
#[cfg(feature = "server")]
mod error;
#[cfg(feature = "extension")]
pub mod ext;
#[cfg(feature = "server")]
pub mod family;
#[cfg(feature = "server")]
mod galaxy32;
#[cfg(feature = "server")]
mod protocol;
#[cfg(feature = "ui")]
pub mod ui;

/// The config kind.
pub const KIND: &str = "antelope-galaxy32";

#[cfg(feature = "server")]
pub use client::{CYCLIC_STATE_CMD, Client, ClientEvent, DEFAULT_REQUEST_TIMEOUT};
#[cfg(feature = "server")]
pub use discovery::{
    ADMIN_SERVICE, Announce, AnnounceProperties, CONTROL_SERVICE, DISCOVERY_PORT, Listener,
    MULTICAST_GROUP, control_endpoints, discover,
};
#[cfg(feature = "server")]
pub use error::{AntelopeError, Result};
#[cfg(feature = "server")]
pub use galaxy32::adapter::Galaxy32Adapter;
#[cfg(feature = "server")]
pub use galaxy32::afx::{AfxCatalog, AfxEffect, AfxField, AfxFieldType};
#[cfg(feature = "server")]
pub use galaxy32::ops::{
    AfxAvailability, AfxSlot, DeviceState, Galaxy32, MixerStrip, MonitorState, MonitorToggle,
    ReverbConfig, RouteSlot, RoutingPage, TrimConfig, TrimLevel, afx_order_call, mixer_call,
    monitor_toggle_call, monitor_volume_call, parse_afx_strip_reply, parse_routing_reply,
    reverb_call, routing_call, sample_rate_call, sync_source_call, trim_call,
};
#[cfg(feature = "server")]
pub use protocol::call::Call;
#[cfg(feature = "server")]
pub use protocol::envelope::{Header, ServerFrame};
#[cfg(feature = "server")]
pub use protocol::framing::{FrameDecoder, encode_frame};

#[cfg(feature = "server")]
/// Galaxy32 constant tables (pages, source types, clock options, ids).
pub mod tables {
    pub use crate::galaxy32::tables::*;
}

/// Add the Antelope Galaxy32 to this process, before the engine and the
/// window start.
///
/// Its device family goes to every engine started from now on (feature
/// `server`), its console view to every window (feature `ui`). Once is
/// enough; later calls do nothing.
pub fn register() {
    patchbay::register_family!(family: family::AntelopeGalaxy32Family, views: ui::views());
}
