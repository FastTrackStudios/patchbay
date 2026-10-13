//! `patchbay-device` — the adapter-agnostic hardware model.
//!
//! patchbay routes more than `PipeWire`: interfaces, mixers and network
//! audio all carry their own routers and parameters. This crate is the
//! shared vocabulary every hardware adapter maps onto:
//!
//! - **Identity** — [`DeviceId`] / [`DeviceInfo`].
//! - **Routing** — a device exposes input and output [`PortGroup`]s; each
//!   output channel takes at most one source ([`Crosspoint`], router
//!   semantics — Antelope routing pages, Dante subscriptions and Yamaha
//!   input patching are all single-source-per-destination).
//! - **Parameters** — [`Param`]s addressed by a slash path
//!   (`mixer/1/strip/16/level`), typed by [`ParamKind`] and valued by
//!   [`ParamValue`]. Channel metadata (names, colours) is params too.
//! - **Events** — [`DeviceEvent`] via a `tokio::sync::broadcast` channel.
//! - **Behaviour** — the [`DeviceAdapter`] trait (native `async fn`
//!   shape, `Send` futures) and its object-safe twin [`DynDeviceAdapter`]
//!   for heterogeneous device lists.
//!
//! No vendor code lives here; adapters are separate crates
//! (`crates/adapters/*`). See `docs/devices.md`.

mod adapter;
mod error;
mod event;
mod id;
mod mirror;
mod param;
mod routing;
mod snapshot;

pub use adapter::{BoxFuture, DeviceAdapter, DynDeviceAdapter};
pub use error::DeviceError;
pub use event::DeviceEvent;
pub use id::{DeviceId, DeviceInfo, Transport};
pub use mirror::{MirrorUpdate, apply_event, diff_events};
pub use param::{Param, ParamKind, ParamValue, WriteGuard};
pub use routing::{ChannelRef, Crosspoint, PortGroup};
pub use snapshot::DeviceSnapshot;
