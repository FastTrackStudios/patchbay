//! [`DanteNetworkAdapter`]: the Dante network as one
//! [`DeviceAdapter`] (see the crate docs for the mapping).
//!
//! Reads always go to the devices (ARC); the adapter only caches the
//! endpoint list from mDNS, refreshed in the background. Every write is
//! confirmed by polling a read-back, because ARC acknowledges a command
//! before the device has applied it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use patchbay_device::{
    ChannelRef, Crosspoint, DeviceAdapter, DeviceError, DeviceEvent, DeviceId, DeviceInfo,
    DeviceSnapshot, ParamValue, Transport, WriteGuard,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::control::{DISCOVER_TIMEOUT, DanteControl, DanteDeviceState, Endpoint, read_all};
use crate::mapping::{self, ParamTarget, parse_param_path};
use crate::{ChannelSide, DanteError};

/// Pause before retrying unreachable seeds once.
const SEED_RETRY: Duration = Duration::from_millis(300);

/// Adapter tuning.
#[derive(Debug, Clone)]
pub struct DanteOptions {
    /// Stable identity (`dante:network:<config name>`).
    pub device_id: String,
    /// mDNS browse length (connect and every refresh).
    pub discover_timeout: Duration,
    /// Pause between background re-discoveries.
    pub refresh_interval: Duration,
    /// Consecutive browses a device may be missing before it's dropped
    /// (mDNS answers from hardware are lazy).
    pub max_misses: u8,
    /// Read-back polls after a write.
    pub confirm_attempts: u32,
    /// Pause between read-back polls.
    pub confirm_delay: Duration,
}

impl Default for DanteOptions {
    fn default() -> Self {
        Self {
            device_id: DeviceId::from_parts("dante", "network", "dante").to_string(),
            discover_timeout: DISCOVER_TIMEOUT,
            refresh_interval: Duration::from_secs(30),
            max_misses: 2,
            confirm_attempts: 10,
            confirm_delay: Duration::from_millis(200),
        }
    }
}

struct Core {
    control: Arc<dyn DanteControl>,
    opts: DanteOptions,
    /// Device name → (endpoint, consecutive browses it was missing).
    endpoints: Mutex<BTreeMap<String, (Endpoint, u8)>>,
    events: broadcast::Sender<DeviceEvent>,
    online: AtomicBool,
    /// One write (and its read-back) at a time.
    write: tokio::sync::Mutex<()>,
}

impl Core {
    fn endpoints(&self) -> Vec<Endpoint> {
        self.endpoints
            .lock()
            .values()
            .map(|(e, _)| e.clone())
            .collect()
    }

    fn endpoint(&self, device: &str) -> Option<Endpoint> {
        self.endpoints.lock().get(device).map(|(e, _)| e.clone())
    }

    fn info(&self) -> DeviceInfo {
        let n = self.endpoints.lock().len();
        DeviceInfo {
            id: DeviceId::new(self.opts.device_id.clone()),
            vendor: "Audinate".to_owned(),
            model: "Dante network".to_owned(),
            serial: None,
            firmware: None,
            transport: Transport::Other {
                description: format!("dante mDNS + ARC (inferno-net) · {n} device(s)"),
            },
            online: self.online.load(Ordering::SeqCst),
        }
    }

    fn emit(&self, ev: DeviceEvent) {
        let _ = self.events.send(ev);
    }

    /// Fold one browse result in. Returns whether the device set changed.
    fn merge(&self, found: Vec<Endpoint>) -> bool {
        let mut map = self.endpoints.lock();
        let before: Vec<Endpoint> = map.values().map(|(e, _)| e.clone()).collect();
        for (_, misses) in map.values_mut() {
            *misses = misses.saturating_add(1);
        }
        for e in found {
            map.insert(e.name.clone(), (e, 0));
        }
        let max = self.opts.max_misses;
        map.retain(|_, (_, misses)| *misses < max);
        let after: Vec<Endpoint> = map.values().map(|(e, _)| e.clone()).collect();
        before != after
    }

    async fn read(&self, device: &str) -> Result<DanteDeviceState, DeviceError> {
        let ep = self
            .endpoint(device)
            .ok_or_else(|| DeviceError::UnknownPort(device.to_owned()))?;
        let st = self.control.read(&ep).await;
        if st.reachable {
            Ok(st)
        } else {
            Err(DeviceError::Timeout(format!("{device} did not answer ARC")))
        }
    }

    /// Poll `device` until `ok` holds for its state.
    async fn confirm(
        &self,
        endpoint: &Endpoint,
        what: &str,
        ok: impl Fn(&DanteDeviceState) -> bool,
    ) -> Result<DanteDeviceState, DeviceError> {
        for attempt in 0..self.opts.confirm_attempts {
            if attempt > 0 {
                tokio::time::sleep(self.opts.confirm_delay).await;
            }
            let st = self.control.read(endpoint).await;
            if st.reachable && ok(&st) {
                return Ok(st);
            }
        }
        Err(DeviceError::Protocol(format!(
            "{what}: not confirmed by read-back from {}",
            endpoint.name
        )))
    }
}

struct Inner {
    core: Arc<Core>,
    refresher: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.refresher.abort();
    }
}

/// The Dante network as one device. Cheap to clone.
#[derive(Clone)]
pub struct DanteNetworkAdapter {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DanteNetworkAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DanteNetworkAdapter")
            .field("id", &self.inner.core.opts.device_id)
            .field("devices", &self.inner.core.endpoints.lock().len())
            .finish_non_exhaustive()
    }
}

async fn refresh_loop(core: Arc<Core>, browse_now: bool) {
    let mut first = true;
    loop {
        if !(first && browse_now) {
            tokio::time::sleep(core.opts.refresh_interval).await;
        }
        first = false;
        let found = match core.control.discover(core.opts.discover_timeout).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "dante: re-discovery failed");
                continue;
            }
        };
        let changed = core.merge(found);
        if core.endpoints.lock().is_empty() {
            core.online.store(false, Ordering::SeqCst);
            core.emit(DeviceEvent::Offline);
            return;
        }
        if changed {
            tracing::info!(
                devices = core.endpoints.lock().len(),
                "dante: network changed"
            );
            core.emit(DeviceEvent::SnapshotReplaced);
        }
    }
}

impl DanteNetworkAdapter {
    /// Discover the network. Fails with [`DanteError::Empty`] when no
    /// device answers mDNS (nothing is written, ever, at connect).
    ///
    /// # Errors
    /// Discovery failure or an empty network.
    pub async fn connect(
        control: Arc<dyn DanteControl>,
        opts: DanteOptions,
    ) -> Result<Self, DanteError> {
        let found = control.discover(opts.discover_timeout).await?;
        Self::start(control, opts, found, false)
    }

    /// Connect from previously known endpoints (e.g. a cache) without
    /// waiting out an mDNS browse: every seed is read over ARC, and the
    /// ones that answer bring the adapter online at once. A full browse
    /// then runs immediately in the background and adds or drops
    /// devices. With no reachable seed this is [`Self::connect`].
    ///
    /// # Errors
    /// As [`Self::connect`] when no seed answers.
    pub async fn connect_seeded(
        control: Arc<dyn DanteControl>,
        opts: DanteOptions,
        seeds: Vec<Endpoint>,
    ) -> Result<Self, DanteError> {
        let mut reachable = Vec::new();
        // Twice: right after launch macOS fails a process's first LAN
        // sends (EPIPE) while it evaluates Local Network access.
        for attempt in 0..2_u8 {
            if seeds.is_empty() {
                break;
            }
            if attempt > 0 {
                tokio::time::sleep(SEED_RETRY).await;
            }
            reachable = read_all(control.as_ref(), &seeds)
                .await
                .into_iter()
                .filter(|st| st.reachable)
                .map(|st| st.endpoint)
                .collect();
            if !reachable.is_empty() {
                break;
            }
        }
        if reachable.is_empty() {
            return Self::connect(control, opts).await;
        }
        Self::start(control, opts, reachable, true)
    }

    fn start(
        control: Arc<dyn DanteControl>,
        opts: DanteOptions,
        found: Vec<Endpoint>,
        browse_now: bool,
    ) -> Result<Self, DanteError> {
        if found.is_empty() {
            return Err(DanteError::Empty);
        }
        let (events, _) = broadcast::channel(1024);
        let core = Arc::new(Core {
            control,
            opts,
            endpoints: Mutex::new(BTreeMap::new()),
            events,
            online: AtomicBool::new(true),
            write: tokio::sync::Mutex::new(()),
        });
        core.merge(found);
        let refresher = tokio::spawn(refresh_loop(Arc::clone(&core), browse_now));
        Ok(Self {
            inner: Arc::new(Inner { core, refresher }),
        })
    }

    /// Currently known devices.
    #[must_use]
    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.inner.core.endpoints()
    }

    /// Fresh ARC read of every known device.
    pub async fn read_network(&self) -> Vec<DanteDeviceState> {
        let core = &self.inner.core;
        read_all(core.control.as_ref(), &core.endpoints()).await
    }

    async fn write_param(&self, path: &str, value: ParamValue) -> Result<(), DeviceError> {
        let core = &self.inner.core;
        let unknown = || DeviceError::UnknownParam(path.to_owned());
        let (device, target) = parse_param_path(path).ok_or_else(unknown)?;
        let ep = core.endpoint(device).ok_or_else(unknown)?;
        let invalid = |reason: String| DeviceError::InvalidValue {
            target: path.to_owned(),
            reason,
        };
        let ParamValue::Text(name) = value else {
            return match target {
                ParamTarget::ReadOnly => Err(DeviceError::ReadOnly(path.to_owned())),
                _ => Err(invalid("expected text".to_owned())),
            };
        };
        let _guard = core.write.lock().await;
        match target {
            ParamTarget::ReadOnly => Err(DeviceError::ReadOnly(path.to_owned())),
            ParamTarget::DeviceName => {
                mapping::validate_device_name(&name).map_err(invalid)?;
                core.control
                    .set_device_name(&ep, &name)
                    .await
                    .map_err(DeviceError::from)?;
                core.confirm(&ep, path, |st| {
                    st.arc_name.as_deref() == Some(name.as_str())
                })
                .await?;
                {
                    let mut map = core.endpoints.lock();
                    map.remove(device);
                    let renamed = Endpoint {
                        name: name.clone(),
                        addr: ep.addr,
                    };
                    map.insert(name, (renamed, 0));
                }
                // Group ids and every path of this device changed.
                core.emit(DeviceEvent::SnapshotReplaced);
                Ok(())
            }
            ParamTarget::ChannelName(side, n) => {
                mapping::validate_channel_name(&name).map_err(invalid)?;
                let st = core.read(device).await?;
                let exists = match side {
                    ChannelSide::Tx => st.tx.iter().any(|c| c.number == n),
                    ChannelSide::Rx => st.rx.iter().any(|c| c.number == n),
                };
                if !exists {
                    return Err(unknown());
                }
                if n > 255 {
                    return Err(DeviceError::ReadOnly(path.to_owned()));
                }
                core.control
                    .set_channel_name(&ep, side, n, &name)
                    .await
                    .map_err(DeviceError::from)?;
                core.confirm(&ep, path, |st| match side {
                    ChannelSide::Tx => st.tx.iter().any(|c| c.number == n && c.name == name),
                    ChannelSide::Rx => st.rx.iter().any(|c| c.number == n && c.name == name),
                })
                .await?;
                core.emit(DeviceEvent::ParamChanged {
                    path: path.to_owned(),
                    value: ParamValue::Text(name),
                });
                Ok(())
            }
        }
    }

    async fn write_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        let core = &self.inner.core;
        let bad_port = |c: &ChannelRef| DeviceError::UnknownPort(c.to_string());
        let rx_ep = core
            .endpoint(&output.group)
            .ok_or_else(|| bad_port(&output))?;
        let rx_n = output
            .channel
            .checked_add(1)
            .ok_or_else(|| bad_port(&output))?;
        let _guard = core.write.lock().await;
        let rx_state = core.read(&output.group).await?;
        if !rx_state.rx.iter().any(|c| c.number == rx_n) {
            return Err(bad_port(&output));
        }
        if let Some(src) = &source {
            let tx_n = src.channel.checked_add(1).ok_or_else(|| bad_port(src))?;
            let tx_state = core.read(&src.group).await?;
            let tx_name = tx_state
                .tx
                .iter()
                .find(|c| c.number == tx_n)
                .map(|c| c.name.clone())
                .ok_or_else(|| bad_port(src))?;
            core.control
                .subscribe(&rx_ep, rx_n, &tx_name, &src.group)
                .await
                .map_err(DeviceError::from)?;
            core.confirm(&rx_ep, &format!("route {output}"), |st| {
                st.rx.iter().any(|c| {
                    c.number == rx_n
                        && c.tx_channel.as_deref() == Some(tx_name.as_str())
                        && c.tx_device.as_deref() == Some(src.group.as_str())
                })
            })
            .await?;
        } else {
            core.control
                .unsubscribe(&rx_ep, rx_n)
                .await
                .map_err(DeviceError::from)?;
            core.confirm(&rx_ep, &format!("route {output}"), |st| {
                st.rx.iter().any(|c| c.number == rx_n && !c.is_subscribed())
            })
            .await?;
        }
        core.emit(DeviceEvent::RouteChanged(Crosspoint { output, source }));
        Ok(())
    }
}

impl DeviceAdapter for DanteNetworkAdapter {
    fn info(&self) -> DeviceInfo {
        self.inner.core.info()
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        if !self.inner.core.online.load(Ordering::SeqCst) {
            return Err(DeviceError::Offline);
        }
        let states = self.read_network().await;
        Ok(mapping::snapshot(self.info(), &states))
    }

    async fn apply_param(
        &self,
        path: &str,
        value: ParamValue,
        _guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        // No Dante param is disruptive yet: sample rate / latency are
        // read-only in this adapter.
        self.write_param(path, value).await
    }

    async fn set_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        self.write_route(output, source).await
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.inner.core.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use patchbay_device::BoxFuture;

    use super::*;
    use crate::mapping::tests::device;

    /// In-memory network. Records every write; applies them so the
    /// read-back confirms.
    #[derive(Default)]
    struct Fake {
        devices: Mutex<Vec<DanteDeviceState>>,
        writes: Mutex<Vec<String>>,
    }

    impl Fake {
        fn with(devices: Vec<DanteDeviceState>) -> Arc<Self> {
            Arc::new(Self {
                devices: Mutex::new(devices),
                writes: Mutex::default(),
            })
        }

        fn edit(&self, ep: &Endpoint, f: impl FnOnce(&mut DanteDeviceState)) {
            if let Some(d) = self
                .devices
                .lock()
                .iter_mut()
                .find(|d| d.endpoint.addr == ep.addr)
            {
                f(d);
            }
        }
    }

    impl DanteControl for Fake {
        fn discover(&self, _: Duration) -> BoxFuture<'_, Result<Vec<Endpoint>, DanteError>> {
            let eps = self
                .devices
                .lock()
                .iter()
                .map(|d| d.endpoint.clone())
                .collect();
            Box::pin(async move { Ok(eps) })
        }

        fn read<'a>(&'a self, endpoint: &'a Endpoint) -> BoxFuture<'a, DanteDeviceState> {
            let st = self
                .devices
                .lock()
                .iter()
                .find(|d| d.endpoint.addr == endpoint.addr)
                .cloned()
                .unwrap_or_else(|| DanteDeviceState::unreachable(endpoint.clone()));
            Box::pin(async move { st })
        }

        fn subscribe<'a>(
            &'a self,
            ep: &'a Endpoint,
            rx: u16,
            ch: &'a str,
            dev: &'a str,
        ) -> BoxFuture<'a, Result<(), DanteError>> {
            self.writes
                .lock()
                .push(format!("sub {}:{rx} <- {ch}@{dev}", ep.name));
            self.edit(ep, |d| {
                if let Some(c) = d.rx.iter_mut().find(|c| c.number == rx) {
                    c.tx_channel = Some(ch.to_owned());
                    c.tx_device = Some(dev.to_owned());
                }
            });
            Box::pin(async { Ok(()) })
        }

        fn unsubscribe<'a>(
            &'a self,
            ep: &'a Endpoint,
            rx: u16,
        ) -> BoxFuture<'a, Result<(), DanteError>> {
            self.writes.lock().push(format!("unsub {}:{rx}", ep.name));
            self.edit(ep, |d| {
                if let Some(c) = d.rx.iter_mut().find(|c| c.number == rx) {
                    c.tx_channel = None;
                    c.tx_device = None;
                }
            });
            Box::pin(async { Ok(()) })
        }

        fn set_device_name<'a>(
            &'a self,
            ep: &'a Endpoint,
            name: &'a str,
        ) -> BoxFuture<'a, Result<(), DanteError>> {
            self.writes
                .lock()
                .push(format!("rename {} -> {name}", ep.name));
            self.edit(ep, |d| {
                d.arc_name = Some(name.to_owned());
                d.endpoint.name = name.to_owned();
            });
            Box::pin(async { Ok(()) })
        }

        fn set_channel_name<'a>(
            &'a self,
            ep: &'a Endpoint,
            side: ChannelSide,
            n: u16,
            name: &'a str,
        ) -> BoxFuture<'a, Result<(), DanteError>> {
            self.writes
                .lock()
                .push(format!("name {}/{}/{n} = {name}", ep.name, side.as_str()));
            self.edit(ep, |d| match side {
                ChannelSide::Tx => {
                    if let Some(c) = d.tx.iter_mut().find(|c| c.number == n) {
                        c.name = name.to_owned();
                    }
                }
                ChannelSide::Rx => {
                    if let Some(c) = d.rx.iter_mut().find(|c| c.number == n) {
                        c.name = name.to_owned();
                    }
                }
            });
            Box::pin(async { Ok(()) })
        }
    }

    fn opts() -> DanteOptions {
        DanteOptions {
            refresh_interval: Duration::from_secs(3600),
            confirm_delay: Duration::from_millis(1),
            ..DanteOptions::default()
        }
    }

    fn rig() -> Arc<Fake> {
        Fake::with(vec![
            device("Console", 1, &["Main L", "Main R"], &[]),
            device("Stage", 2, &["Mic 1"], &[("", ""), ("", "")]),
        ])
    }

    #[tokio::test]
    async fn empty_network_is_an_error() {
        let fake = Fake::with(Vec::new());
        let r = DanteNetworkAdapter::connect(fake, opts()).await;
        assert!(matches!(r, Err(DanteError::Empty)));
    }

    #[tokio::test]
    async fn seeded_connect_skips_the_browse_then_catches_up() {
        let fake = rig();
        let console = fake.devices.lock()[0].endpoint.clone();
        let gone = Endpoint {
            name: "Gone".into(),
            addr: "10.9.9.9:4440".parse().expect("addr"),
        };
        let a = DanteNetworkAdapter::connect_seeded(fake.clone(), opts(), vec![console, gone])
            .await
            .expect("connect");
        // Online from the one reachable seed; the dead seed is dropped.
        let mut rx = DeviceAdapter::subscribe(&a);
        let names = |a: &DanteNetworkAdapter| {
            a.endpoints()
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        };
        assert!(names(&a) == ["Console"] || names(&a) == ["Console", "Stage"]);
        // The immediate background browse adds the rest.
        if names(&a).len() == 1 {
            let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("browse event")
                .expect("event");
            assert!(matches!(ev, DeviceEvent::SnapshotReplaced));
        }
        assert_eq!(names(&a), ["Console", "Stage"]);
        assert!(fake.writes.lock().is_empty());
    }

    #[tokio::test]
    async fn seeded_connect_without_reachable_seeds_browses() {
        let fake = rig();
        let gone = Endpoint {
            name: "Gone".into(),
            addr: "10.9.9.9:4440".parse().expect("addr"),
        };
        let a = DanteNetworkAdapter::connect_seeded(fake, opts(), vec![gone])
            .await
            .expect("connect");
        assert_eq!(a.endpoints().len(), 2);
    }

    #[tokio::test]
    async fn connect_is_read_only_and_snapshots() {
        let fake = rig();
        let a = DanteNetworkAdapter::connect(fake.clone(), opts())
            .await
            .expect("connect");
        let s = a.snapshot().await.expect("snapshot");
        assert_eq!(s.info.id.as_str(), "dante:network:dante");
        assert_eq!(s.outputs.len(), 1);
        assert_eq!(s.inputs.len(), 2);
        assert!(fake.writes.lock().is_empty());
    }

    #[tokio::test]
    async fn route_subscribes_confirms_and_clears() {
        let fake = rig();
        let a = DanteNetworkAdapter::connect(fake.clone(), opts())
            .await
            .expect("connect");
        let mut rx = DeviceAdapter::subscribe(&a);
        let out = ChannelRef::new("Stage", 1);
        a.set_route(out.clone(), Some(ChannelRef::new("Console", 1)))
            .await
            .expect("route");
        let s = a.snapshot().await.expect("snapshot");
        assert_eq!(
            s.source_of(&out),
            Some(Some(&ChannelRef::new("Console", 1)))
        );
        assert!(matches!(
            rx.recv().await,
            Ok(DeviceEvent::RouteChanged(c)) if c.output == out
        ));
        a.set_route(out.clone(), None).await.expect("clear");
        assert_eq!(a.snapshot().await.expect("s").source_of(&out), Some(None));
        assert_eq!(
            *fake.writes.lock(),
            vec!["sub Stage:2 <- Main R@Console", "unsub Stage:2"]
        );
    }

    #[tokio::test]
    async fn route_errors_write_nothing() {
        let fake = rig();
        let a = DanteNetworkAdapter::connect(fake.clone(), opts())
            .await
            .expect("connect");
        for (out, src) in [
            (
                ChannelRef::new("Nope", 0),
                Some(ChannelRef::new("Console", 0)),
            ),
            (
                ChannelRef::new("Stage", 9),
                Some(ChannelRef::new("Console", 0)),
            ),
            (
                ChannelRef::new("Stage", 0),
                Some(ChannelRef::new("Console", 7)),
            ),
            (
                ChannelRef::new("Stage", 0),
                Some(ChannelRef::new("Ghost", 0)),
            ),
        ] {
            let e = a.set_route(out, src).await.expect_err("must fail");
            assert!(matches!(e, DeviceError::UnknownPort(_)), "{e:?}");
        }
        assert!(fake.writes.lock().is_empty());
    }

    #[tokio::test]
    async fn params_rename_and_refuse() {
        let fake = rig();
        let a = DanteNetworkAdapter::connect(fake.clone(), opts())
            .await
            .expect("connect");
        a.set_param("Console/tx/1/name", ParamValue::Text("Kick".into()))
            .await
            .expect("rename channel");
        let e = a
            .set_param("Console/sample_rate", ParamValue::Int(96_000))
            .await
            .expect_err("ro");
        assert!(matches!(e, DeviceError::ReadOnly(_)), "{e:?}");
        let e = a
            .set_param("Console/tx/1/name", ParamValue::Text("a@b".into()))
            .await
            .expect_err("invalid");
        assert!(matches!(e, DeviceError::InvalidValue { .. }), "{e:?}");
        let e = a
            .set_param("Console/tx/9/name", ParamValue::Text("x".into()))
            .await
            .expect_err("unknown");
        assert!(matches!(e, DeviceError::UnknownParam(_)), "{e:?}");
        a.set_param("Stage/name", ParamValue::Text("Stage-2".into()))
            .await
            .expect("rename device");
        assert!(a.endpoints().iter().any(|e| e.name == "Stage-2"));
        let s = a.snapshot().await.expect("snapshot");
        assert!(s.output("Stage-2").is_some());
        assert_eq!(
            s.param("Console/tx/1/name").map(|p| p.value.clone()),
            Some(ParamValue::Text("Kick".into()))
        );
        assert_eq!(
            *fake.writes.lock(),
            vec!["name Console/tx/1 = Kick", "rename Stage -> Stage-2"]
        );
    }

    #[tokio::test]
    async fn missing_devices_drop_after_max_misses() {
        let fake = rig();
        let a = DanteNetworkAdapter::connect(fake.clone(), opts())
            .await
            .expect("connect");
        let core = &a.inner.core;
        let console = core.endpoint("Console").expect("ep");
        assert!(!core.merge(vec![console.clone()]));
        assert_eq!(core.endpoints().len(), 2);
        assert!(core.merge(vec![console]));
        assert_eq!(core.endpoints().len(), 1);
    }
}
