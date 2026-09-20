//! `patchbay-host` — the platform-independent host-audio model.
//!
//! Hardware adapters (`patchbay-device`) control *external* boxes with
//! router semantics: one source per destination. A **host backend**
//! controls the machine's *own* audio system, which is a **graph**:
//!
//! - **Nodes** ([`HostNode`]) — hardware devices, virtual devices, per-app
//!   streams and aggregates. Every node has stable string identity.
//! - **Ports** ([`PortRef`]) — `(node, channel, direction)`, graph
//!   perspective: an [`PortDirection::Output`] port *produces* audio.
//! - **Links** ([`HostLink`]) — many-to-many and **mixing**: several
//!   links into one input port sum; each link carries its own gain.
//! - **Virtual devices** ([`VirtualDeviceSpec`]) — Loopback-style: a named
//!   device fed by sources (apps, input devices, system audio, pass-thru)
//!   and optionally monitored to outputs.
//!
//! Behaviour is the [`HostBackend`] trait (native `async fn` shape, `Send`
//! futures) with its object-safe twin [`DynHostBackend`]. Backends:
//! `patchbay-host-coreaudio` (macOS) and — after migration — the
//! `PipeWire` engine as `patchbay-host-pipewire`. See
//! `docs/host-backends.md`.
//!
//! Pure planning helpers ([`ChannelMap::validate`],
//! [`VirtualDeviceSpec::validate`], [`validate_link`], [`diff_nodes`])
//! live here so every backend applies the same rules.

mod backend;
mod caps;
mod error;
mod event;
mod gain;
mod link;
mod node;
mod plan;
mod snapshot;
mod virtual_device;

pub use backend::{BoxFuture, DynHostBackend, HostBackend, VolumeTarget};
pub use caps::HostCapabilities;
pub use error::HostError;
pub use event::HostEvent;
pub use gain::{Gain, MAX_GAIN};
pub use link::HostLink;
pub use node::{AppInfo, HostNode, NodeDirection, NodeKind, PortCounts, PortDirection, PortRef};
pub use plan::{diff_nodes, validate_link};
pub use snapshot::HostSnapshot;
pub use virtual_device::{
    AppSelector, ChannelMap, ChannelPair, MonitorSpec, SourceKind, SourceSpec, TapMute,
    VirtualDeviceSpec, bundle_matches, parent_bundle,
};
