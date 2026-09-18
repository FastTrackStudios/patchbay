//! `patchbay-dante` — the Dante network for patchbay, over
//! [`inferno_net`] (pure-Rust ARC control + mDNS discovery; runs on macOS
//! and Linux alike).
//!
//! - [`DanteControl`]: the control surface the adapter needs (discover,
//!   read one device, subscribe/unsubscribe, rename). [`InfernoControl`]
//!   is the real implementation; tests plug in a fake, so **writes are
//!   only ever exercised against fakes**.
//! - [`scan`]: discover + read every device (channels, subscriptions,
//!   sample rate, latency). Also backs the `dante_network` RPC.
//! - [`DanteNetworkAdapter`]: the whole network as ONE
//!   [`patchbay_device::DeviceAdapter`] — Dante Controller's matrix. RX
//!   channels of every device are router outputs, TX channels of every
//!   device are router inputs, a subscription is a crosspoint
//!   (`rx device:channel ← tx device:channel`). Group ids are Dante
//!   device names (what subscriptions reference on the wire).
//!
//! Why one adapter for the network rather than one per box: a
//! subscription is inherently cross-device (the source is another box's
//! TX channel), and the generic model's crosspoints are per adapter. One
//! network adapter keeps every subscription a plain crosspoint and fits
//! the hub's one-entry-per-config supervision without a dynamic device
//! provider.

mod adapter;
mod control;
mod error;
mod mapping;

pub use adapter::{DanteNetworkAdapter, DanteOptions};
pub use control::{
    ARC_TIMEOUT, ChannelSide, DISCOVER_TIMEOUT, DanteControl, DanteDeviceState, Endpoint,
    InfernoControl, RxChannel, TxChannel, scan,
};
pub use error::DanteError;
pub use mapping::{
    ParamTarget, parse_param_path, snapshot, validate_channel_name, validate_device_name,
};
