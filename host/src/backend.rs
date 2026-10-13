//! The trait every host-audio backend implements.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{
    HostCapabilities, HostError, HostEvent, HostLink, HostSnapshot, PortRef, VirtualDeviceSpec,
};

/// A boxed, `Send` future — the return type of [`DynHostBackend`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What [`HostBackend::set_volume`] adjusts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum VolumeTarget {
    /// A node's own volume (device master / app stream volume).
    Node {
        /// [`crate::HostNode::id`].
        id: String,
    },
    /// One link's gain.
    Link {
        /// Source port.
        from: PortRef,
        /// Destination port.
        to: PortRef,
    },
    /// One source of a virtual device.
    VirtualSource {
        /// Id returned by [`HostBackend::create_virtual_device`].
        device: String,
        /// Index into [`VirtualDeviceSpec::sources`].
        index: u32,
    },
    /// One monitor of a virtual device.
    VirtualMonitor {
        /// Id returned by [`HostBackend::create_virtual_device`].
        device: String,
        /// Index into [`VirtualDeviceSpec::monitors`].
        index: u32,
    },
}

/// The machine's own audio system, as a graph.
///
/// Contract:
///
/// - The **OS is the source of truth**. [`HostBackend::snapshot`] reads
///   it; [`HostEvent`]s report what the OS reports (hot-plug, apps
///   starting to play), not only what patchbay did.
/// - Links **mix** (many-to-many) and are validated with
///   [`crate::validate_link`] before touching the OS.
/// - Anything outside [`HostBackend::capabilities`] fails with
///   [`HostError::Unsupported`], never silently.
/// - Backends never change system defaults or mute other apps on their
///   own.
/// - Persistence follows the platform: `PipeWire` links are created with
///   `object.linger` and are **never** cleaned up at exit (patchbay edits
///   the system graph, it doesn't own it); Core Audio taps / private
///   aggregates are process-owned and torn down when their owner drops
///   (until the HAL plug-in exists). See `docs/host-backends.md`.
///
/// Shape: edition-2024 return-position `impl Future + Send` (so futures
/// can be spawned), **not** object safe; every backend is also a
/// [`DynHostBackend`] (blanket impl) for `Box<dyn DynHostBackend>`.
/// Implementations may do short blocking OS calls inside these futures —
/// Core Audio property calls are synchronous and fast.
pub trait HostBackend: Send + Sync {
    /// Short backend name (`coreaudio`, `pipewire`).
    fn name(&self) -> &'static str;

    /// What this backend can do (cheap, no I/O).
    fn capabilities(&self) -> HostCapabilities;

    /// Read the full graph.
    fn snapshot(&self) -> impl Future<Output = Result<HostSnapshot, HostError>> + Send;

    /// Create (or update the gain / enabled state of) a link.
    fn create_link(&self, link: HostLink) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Remove the link `from` → `to`.
    fn remove_link(
        &self,
        from: PortRef,
        to: PortRef,
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Create a virtual device; returns its node id.
    fn create_virtual_device(
        &self,
        spec: VirtualDeviceSpec,
    ) -> impl Future<Output = Result<String, HostError>> + Send;

    /// Remove a virtual device created by [`Self::create_virtual_device`].
    fn remove_virtual_device(&self, id: &str)
    -> impl Future<Output = Result<(), HostError>> + Send;

    /// Set a linear gain, from 0.0 up to [`crate::MAX_GAIN`].
    fn set_volume(
        &self,
        target: VolumeTarget,
        gain: f32,
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Subscribe to graph events. A lagged receiver should treat the gap as
    /// [`HostEvent::SnapshotReplaced`].
    fn subscribe(&self) -> broadcast::Receiver<HostEvent>;
}

/// Object-safe mirror of [`HostBackend`]. Blanket-implemented.
pub trait DynHostBackend: Send + Sync {
    /// See [`HostBackend::name`].
    fn name(&self) -> &'static str;
    /// See [`HostBackend::capabilities`].
    fn capabilities(&self) -> HostCapabilities;
    /// See [`HostBackend::snapshot`].
    fn snapshot(&self) -> BoxFuture<'_, Result<HostSnapshot, HostError>>;
    /// See [`HostBackend::create_link`].
    fn create_link(&self, link: HostLink) -> BoxFuture<'_, Result<(), HostError>>;
    /// See [`HostBackend::remove_link`].
    fn remove_link(&self, from: PortRef, to: PortRef) -> BoxFuture<'_, Result<(), HostError>>;
    /// See [`HostBackend::create_virtual_device`].
    fn create_virtual_device(
        &self,
        spec: VirtualDeviceSpec,
    ) -> BoxFuture<'_, Result<String, HostError>>;
    /// See [`HostBackend::remove_virtual_device`].
    fn remove_virtual_device<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), HostError>>;
    /// See [`HostBackend::set_volume`].
    fn set_volume(&self, target: VolumeTarget, gain: f32) -> BoxFuture<'_, Result<(), HostError>>;
    /// See [`HostBackend::subscribe`].
    fn subscribe(&self) -> broadcast::Receiver<HostEvent>;
}

impl<T: HostBackend> DynHostBackend for T {
    fn name(&self) -> &'static str {
        HostBackend::name(self)
    }

    fn capabilities(&self) -> HostCapabilities {
        HostBackend::capabilities(self)
    }

    fn snapshot(&self) -> BoxFuture<'_, Result<HostSnapshot, HostError>> {
        Box::pin(HostBackend::snapshot(self))
    }

    fn create_link(&self, link: HostLink) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(HostBackend::create_link(self, link))
    }

    fn remove_link(&self, from: PortRef, to: PortRef) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(HostBackend::remove_link(self, from, to))
    }

    fn create_virtual_device(
        &self,
        spec: VirtualDeviceSpec,
    ) -> BoxFuture<'_, Result<String, HostError>> {
        Box::pin(HostBackend::create_virtual_device(self, spec))
    }

    fn remove_virtual_device<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), HostError>> {
        Box::pin(HostBackend::remove_virtual_device(self, id))
    }

    fn set_volume(&self, target: VolumeTarget, gain: f32) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(HostBackend::set_volume(self, target, gain))
    }

    fn subscribe(&self) -> broadcast::Receiver<HostEvent> {
        HostBackend::subscribe(self)
    }
}
