//! External devices: hardware adapters (`patchbay-device` +
//! `crates/adapters/*`) behind the same `PatchbayService` surface as the
//! `PipeWire` graph.
//!
//! - [`hub`] — `DeviceHub`: config → supervised adapters, events, calls,
//!   snapshot save/diff/restore.
//! - [`registry`] — config `kind` → connect function (one line per
//!   adapter family) and the default device set.
//! - [`cache`] — last-found addresses of auto-discovered devices.
//! - [`system_audio`] — the host's own audio system as a read-only
//!   device (Core Audio / the `PipeWire` graph).
//! - [`wire`] — device model ↔ proto conversions.

pub(crate) mod cache;
pub(crate) mod hub;
pub(crate) mod registry;
pub(crate) mod system_audio;
pub(crate) mod wire;

pub(crate) use hub::DeviceHub;
