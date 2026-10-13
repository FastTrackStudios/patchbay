//! The adapter trait every hardware family implements.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::broadcast;

use crate::{
    ChannelRef, DeviceError, DeviceEvent, DeviceInfo, DeviceSnapshot, ParamValue, WriteGuard,
};

/// A boxed, `Send` future — the return type of [`DynDeviceAdapter`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One controllable device.
///
/// Implementations own their connection and background tasks. Contract:
///
/// - The **device is the source of truth**. Reads come from the device,
///   writes are followed by a read-back, and [`DeviceEvent`]s reflect
///   what the device reports (other clients and hardware panels write too).
/// - `set_route` has router semantics: exactly one source (or none) per
///   output channel.
/// - Disruptive parameters (clock, sample rate) are refused unless the
///   write carries [`WriteGuard::AllowDisruptive`].
///
/// Uses edition-2024 return-position `impl Future + Send` so futures can
/// be spawned; for `dyn` use, every adapter is also a [`DynDeviceAdapter`].
pub trait DeviceAdapter: Send + Sync {
    /// Identity and online state (cheap, no I/O).
    fn info(&self) -> DeviceInfo;

    /// Read the full state from the device.
    fn snapshot(&self) -> impl Future<Output = Result<DeviceSnapshot, DeviceError>> + Send;

    /// The adapter's current view of the device, as cheaply as it can
    /// give it. Adapters that keep an exact mirror (device notifications
    /// plus write read-backs) answer from it without I/O; the default is
    /// a full [`snapshot`](Self::snapshot). Use `snapshot` where a fresh
    /// read matters (saving or restoring a snapshot).
    fn current(&self) -> impl Future<Output = Result<DeviceSnapshot, DeviceError>> + Send {
        self.snapshot()
    }

    /// Write one parameter with an explicit [`WriteGuard`].
    fn apply_param(
        &self,
        path: &str,
        value: ParamValue,
        guard: WriteGuard,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send;

    /// Write one non-disruptive parameter.
    fn set_param(
        &self,
        path: &str,
        value: ParamValue,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send {
        self.apply_param(path, value, WriteGuard::Normal)
    }

    /// Patch `source` (or silence, `None`) into the output channel.
    fn set_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send;

    /// Subscribe to device events.
    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent>;
}

/// Object-safe mirror of [`DeviceAdapter`] for heterogeneous device
/// lists (`Vec<Box<dyn DynDeviceAdapter>>`). Blanket-implemented.
pub trait DynDeviceAdapter: Send + Sync {
    /// See [`DeviceAdapter::info`].
    fn info(&self) -> DeviceInfo;
    /// See [`DeviceAdapter::snapshot`].
    fn snapshot(&self) -> BoxFuture<'_, Result<DeviceSnapshot, DeviceError>>;
    /// See [`DeviceAdapter::current`].
    fn current(&self) -> BoxFuture<'_, Result<DeviceSnapshot, DeviceError>>;
    /// See [`DeviceAdapter::apply_param`].
    fn apply_param<'a>(
        &'a self,
        path: &'a str,
        value: ParamValue,
        guard: WriteGuard,
    ) -> BoxFuture<'a, Result<(), DeviceError>>;
    /// See [`DeviceAdapter::set_route`].
    fn set_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> BoxFuture<'_, Result<(), DeviceError>>;
    /// See [`DeviceAdapter::subscribe`].
    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent>;
}

impl<T: DeviceAdapter> DynDeviceAdapter for T {
    fn info(&self) -> DeviceInfo {
        DeviceAdapter::info(self)
    }

    fn snapshot(&self) -> BoxFuture<'_, Result<DeviceSnapshot, DeviceError>> {
        Box::pin(DeviceAdapter::snapshot(self))
    }

    fn current(&self) -> BoxFuture<'_, Result<DeviceSnapshot, DeviceError>> {
        Box::pin(DeviceAdapter::current(self))
    }

    fn apply_param<'a>(
        &'a self,
        path: &'a str,
        value: ParamValue,
        guard: WriteGuard,
    ) -> BoxFuture<'a, Result<(), DeviceError>> {
        Box::pin(DeviceAdapter::apply_param(self, path, value, guard))
    }

    fn set_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> BoxFuture<'_, Result<(), DeviceError>> {
        Box::pin(DeviceAdapter::set_route(self, output, source))
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        DeviceAdapter::subscribe(self)
    }
}
