//! `patchbay-antelope` — Antelope Audio devices for patchbay.
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
//! - [`Galaxy32Adapter`]: the Galaxy32 as a [`patchbay_device::DeviceAdapter`].
//! - [`AfxCatalog`]: schema-driven AFX effect encoder.
//!
//! **Source of truth is the device.** The official Antelope panel does
//! not refresh from other clients' writes and later re-sends its stale
//! whole-page/whole-strip state; this crate always reads back after
//! writing and never trusts its own cache over the device.

mod client;
mod discovery;
mod error;
mod galaxy32;
mod protocol;

pub use client::{CYCLIC_STATE_CMD, Client, ClientEvent, DEFAULT_REQUEST_TIMEOUT};
pub use discovery::{
    ADMIN_SERVICE, Announce, AnnounceProperties, CONTROL_SERVICE, DISCOVERY_PORT, Listener,
    MULTICAST_GROUP, control_endpoints, discover,
};
pub use error::{AntelopeError, Result};
pub use galaxy32::adapter::Galaxy32Adapter;
pub use galaxy32::afx::{AfxCatalog, AfxEffect, AfxField, AfxFieldType};
pub use galaxy32::ops::{
    AfxAvailability, AfxSlot, DeviceState, Galaxy32, MixerStrip, MonitorState, MonitorToggle,
    ReverbConfig, RouteSlot, RoutingPage, TrimConfig, TrimLevel, afx_order_call, mixer_call,
    monitor_toggle_call, monitor_volume_call, parse_afx_strip_reply, parse_routing_reply,
    reverb_call, routing_call, sample_rate_call, sync_source_call, trim_call,
};
pub use protocol::call::Call;
pub use protocol::envelope::{Header, ServerFrame};
pub use protocol::framing::{FrameDecoder, encode_frame};

/// Galaxy32 constant tables (pages, source types, clock options, ids).
pub mod tables {
    pub use crate::galaxy32::tables::*;
}
