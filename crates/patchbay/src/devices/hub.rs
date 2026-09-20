//! [`DeviceHub`]: owns every configured hardware adapter.
//!
//! One supervisor task per config entry connects (discovering if
//! needed), forwards the adapter's events into the RPC stream, and on
//! a drop reconnects with exponential backoff. Nothing here touches
//! `PipeWire`: a device being offline only ever fails device calls, and
//! the hub runs the same on macOS (no `PipeWire` engine) as on Linux.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use patchbay_device::{DeviceEvent, DeviceInfo, WriteGuard};
use patchbay_proto::{
    DeviceChannel, DeviceConfig, DeviceCrosspoint, DeviceEventKind, DeviceEventWire,
    DeviceLinkState, DeviceParamValue, DeviceRestoreReport, DeviceRestoreStatus,
    DeviceSettingSnapshot, DeviceSnapshotInfo, DeviceSummary, DeviceView, ParamView, PatchbayError,
    path_in_prefix,
};
use tokio::sync::broadcast::error::RecvError;

use super::cache::DiscoveryCache;
use super::registry::{self, Adapter, ConnectCtx, ConnectError};
use super::wire;
use crate::plan::devices::{self as planner, DeviceOp};
use crate::presets::PresetStore;
use crate::store::GraphStore;

const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long a session has to last before the backoff is considered
/// earned back.
///
/// Resetting on `connect` alone makes a flapping device reconnect at
/// [`BACKOFF_MIN`] forever. Against the Antelope Manager Server that
/// churn is not harmless: connecting every two seconds broke its
/// heartbeat to the Galaxy 32 and it tore the Thunderbolt device down
/// (`Heart beat exception` → `_stop_device` → `Stopping server for
/// device`). A connection that dies immediately has not proved
/// anything, so it does not earn a fresh backoff.
const SESSION_EARNS_RESET: Duration = Duration::from_secs(30);

/// Live state of one configured device.
struct SlotState {
    adapter: Option<Adapter>,
    /// Last known identity (kept while offline).
    info: Option<DeviceInfo>,
    link: DeviceLinkState,
    error: String,
    /// Reached at least once (a later failure is *offline*, not *not
    /// found*).
    ever_online: bool,
}

struct Slot {
    config: DeviceConfig,
    state: Mutex<SlotState>,
}

impl Slot {
    fn summary(&self) -> DeviceSummary {
        let st = self.state.lock();
        let mut s = blank_summary(&self.config.name, &self.config.kind);
        if let Some(info) = st
            .adapter
            .as_ref()
            .map(|a| a.info())
            .or_else(|| st.info.clone())
        {
            wire::apply_info(&mut s, &info);
        }
        s.state = st.link;
        s.error.clone_from(&st.error);
        s
    }

    fn adapter(&self) -> Result<Adapter, PatchbayError> {
        let st = self.state.lock();
        st.adapter.clone().ok_or_else(|| {
            let why = if st.error.is_empty() {
                format!("{} is {:?}", self.config.name, st.link)
            } else {
                format!("{} is {:?}: {}", self.config.name, st.link, st.error)
            };
            PatchbayError::device("offline", why)
        })
    }

    fn id(&self) -> String {
        let st = self.state.lock();
        st.info
            .as_ref()
            .map(|i| i.id.to_string())
            .unwrap_or_default()
    }
}

/// A summary with only the config half filled in.
pub(crate) fn blank_summary(name: &str, kind: &str) -> DeviceSummary {
    DeviceSummary {
        id: String::new(),
        name: name.to_owned(),
        kind: kind.to_owned(),
        vendor: String::new(),
        model: String::new(),
        serial: String::new(),
        firmware: String::new(),
        transport: String::new(),
        state: DeviceLinkState::Connecting,
        error: String::new(),
    }
}

/// Every configured device + the event hub the RPC stream serves.
pub(crate) struct DeviceHub {
    slots: Vec<Arc<Slot>>,
    events: architect::PubSub<DeviceEventWire>,
    presets: Arc<PresetStore>,
}

/// `PATCHBAY_DEVICES=off` (or `0`/`false`) disables the device layer.
fn devices_disabled_by_env() -> bool {
    std::env::var("PATCHBAY_DEVICES").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false"
        )
    })
}

impl DeviceHub {
    /// Build from config and start a supervisor per enabled entry. With
    /// no `devices` section the default set is used
    /// ([`registry::default_devices`]).
    ///
    /// Needs a tokio runtime to supervise; without one (a sync caller)
    /// the devices are listed but stay offline.
    pub(crate) fn start(
        presets: Arc<PresetStore>,
        graph: Arc<parking_lot::RwLock<GraphStore>>,
    ) -> Self {
        let events = architect::PubSub::sliding(4096);
        let configs = if devices_disabled_by_env() {
            Vec::new()
        } else {
            let c = presets.device_configs();
            if c.is_empty() {
                registry::default_devices()
            } else {
                c
            }
        };
        let runtime = tokio::runtime::Handle::try_current().ok();
        let ctx = ConnectCtx {
            cache: Arc::new(DiscoveryCache::open()),
            graph,
        };
        let mut slots = Vec::new();
        for config in configs {
            let kind = registry::find(&config.kind);
            let (link, error) = match (!config.is_enabled(), kind, &runtime) {
                (true, _, _) => (DeviceLinkState::Disabled, String::new()),
                (false, None, _) => (
                    DeviceLinkState::Offline,
                    format!(
                        "unknown device kind '{}' (known: {})",
                        config.kind,
                        registry::KINDS
                            .iter()
                            .map(|k| k.kind)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                (false, Some(_), None) => (
                    DeviceLinkState::Offline,
                    "no async runtime to supervise the device".to_owned(),
                ),
                (false, Some(k), Some(_)) if k.discovers && config.addr.is_empty() => {
                    (DeviceLinkState::Searching, String::new())
                }
                (false, Some(_), Some(_)) => (DeviceLinkState::Connecting, String::new()),
            };
            let slot = Arc::new(Slot {
                config,
                state: Mutex::new(SlotState {
                    adapter: None,
                    info: None,
                    link,
                    error,
                    ever_online: false,
                }),
            });
            if let (
                DeviceLinkState::Connecting | DeviceLinkState::Searching,
                Some(rt),
                Some(kind),
            ) = (link, &runtime, kind)
            {
                rt.spawn(supervise(
                    Arc::clone(&slot),
                    kind.connect,
                    ctx.clone(),
                    events.clone(),
                ));
            }
            slots.push(slot);
        }
        Self {
            slots,
            events,
            presets,
        }
    }

    pub(crate) const fn events(&self) -> &architect::PubSub<DeviceEventWire> {
        &self.events
    }

    pub(crate) fn list(&self) -> Vec<DeviceSummary> {
        self.slots.iter().map(|s| s.summary()).collect()
    }

    /// Resolve a device reference: exact id or config name first, then
    /// a unique case-insensitive substring of id / name / model / serial.
    fn resolve(&self, query: &str) -> Result<&Arc<Slot>, PatchbayError> {
        let q = query.trim();
        if q.is_empty() {
            return Err(PatchbayError::not_found("device", &q));
        }
        if let Some(s) = self
            .slots
            .iter()
            .find(|s| s.id() == q || s.config.name == q)
        {
            return Ok(s);
        }
        let ql = q.to_lowercase();
        let hits: Vec<&Arc<Slot>> = self
            .slots
            .iter()
            .filter(|s| {
                let sum = s.summary();
                [&sum.id, &sum.name, &sum.model, &sum.serial]
                    .iter()
                    .any(|f| f.to_lowercase().contains(&ql))
            })
            .collect();
        match hits.as_slice() {
            [one] => Ok(one),
            [] => Err(PatchbayError::not_found("device", &q)),
            many => Err(PatchbayError::device(
                "ambiguous_device",
                format!(
                    "'{q}' matches {} devices ({}); use the full id",
                    many.len(),
                    many.iter()
                        .map(|s| s.summary().id)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    /// The adapter's current view of the whole device (its live mirror
    /// where it keeps one — see `DeviceAdapter::current`).
    pub(crate) async fn view(&self, id: &str) -> Result<DeviceView, PatchbayError> {
        let slot = self.resolve(id)?;
        let adapter = slot.adapter()?;
        let snap = adapter.current().await.map_err(|e| wire::error(&e))?;
        Ok(wire::view(slot.summary(), &snap))
    }

    /// Fresh read of the whole device from the hardware.
    async fn fresh_view(&self, id: &str) -> Result<DeviceView, PatchbayError> {
        let slot = self.resolve(id)?;
        let adapter = slot.adapter()?;
        let snap = adapter.snapshot().await.map_err(|e| wire::error(&e))?;
        Ok(wire::view(slot.summary(), &snap))
    }

    pub(crate) async fn params(
        &self,
        id: &str,
        prefix: &str,
    ) -> Result<Vec<ParamView>, PatchbayError> {
        let v = self.view(id).await?;
        Ok(v.params
            .into_iter()
            .filter(|p| path_in_prefix(&p.path, prefix))
            .collect())
    }

    async fn apply(
        adapter: &Adapter,
        path: &str,
        value: &DeviceParamValue,
        allow_disruptive: bool,
    ) -> Result<(), PatchbayError> {
        let guard = if allow_disruptive {
            WriteGuard::AllowDisruptive
        } else {
            WriteGuard::Normal
        };
        adapter
            .apply_param(path, wire::value_from_wire(value), guard)
            .await
            .map_err(|e| wire::error(&e))
    }

    pub(crate) async fn set_param(
        &self,
        id: &str,
        path: &str,
        value: &DeviceParamValue,
        allow_disruptive: bool,
    ) -> Result<ParamView, PatchbayError> {
        let slot = self.resolve(id)?;
        let adapter = slot.adapter()?;
        Self::apply(&adapter, path, value, allow_disruptive).await?;
        // Source of truth is the device: `apply_param` confirmed the write
        // by reading it back, and the adapter's mirror holds that value.
        let snap = adapter.current().await.map_err(|e| wire::error(&e))?;
        snap.param(path)
            .map(wire::param)
            .ok_or_else(|| PatchbayError::not_found("device param", &path))
    }

    pub(crate) async fn set_route(
        &self,
        id: &str,
        output: &DeviceChannel,
        source: Option<&DeviceChannel>,
    ) -> Result<DeviceCrosspoint, PatchbayError> {
        let slot = self.resolve(id)?;
        let adapter = slot.adapter()?;
        let out = wire::channel_from_wire(output);
        adapter
            .set_route(out.clone(), source.map(wire::channel_from_wire))
            .await
            .map_err(|e| wire::error(&e))?;
        let snap = adapter.current().await.map_err(|e| wire::error(&e))?;
        Ok(DeviceCrosspoint {
            output: output.clone(),
            source: snap
                .source_of(&out)
                .ok_or_else(|| PatchbayError::not_found("device output", &output.label()))?
                .map(wire::channel),
        })
    }

    // ── Snapshots ────────────────────────────────────────────────────

    pub(crate) async fn save_snapshot(
        &self,
        id: &str,
        name: &str,
        include: Vec<String>,
        exclude: Vec<String>,
    ) -> Result<DeviceSnapshotInfo, PatchbayError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PatchbayError::Internal("snapshot name is empty".into()));
        }
        // A saved snapshot must be exactly what the hardware holds now.
        let live = self.fresh_view(id).await?;
        let (params, routes) = planner::capture(&live, &include, &exclude);
        let snap = DeviceSettingSnapshot {
            name: name.to_owned(),
            device: live.summary.id.clone(),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            include,
            exclude,
            params,
            routes,
        };
        let info = snap.info();
        let presets = Arc::clone(&self.presets);
        tokio::task::spawn_blocking(move || presets.save_device_snapshot(snap))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?;
        Ok(info)
    }

    pub(crate) fn snapshots(&self) -> Vec<DeviceSnapshotInfo> {
        self.presets
            .device_snapshots()
            .iter()
            .map(DeviceSettingSnapshot::info)
            .collect()
    }

    pub(crate) async fn delete_snapshot(&self, name: &str) -> Result<(), PatchbayError> {
        let presets = Arc::clone(&self.presets);
        let n = name.to_owned();
        let removed = tokio::task::spawn_blocking(move || presets.delete_device_snapshot(&n))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?;
        if removed {
            Ok(())
        } else {
            Err(PatchbayError::not_found("device snapshot", &name))
        }
    }

    /// Diff (`dry_run`) or restore a saved snapshot onto its device.
    pub(crate) async fn restore(
        &self,
        name: &str,
        only: &[String],
        dry_run: bool,
        allow_disruptive: bool,
    ) -> Result<DeviceRestoreReport, PatchbayError> {
        let snap = self
            .presets
            .device_snapshot(name)
            .ok_or_else(|| PatchbayError::not_found("device snapshot", &name))?;
        let slot = self.resolve(&snap.device)?;
        let adapter = slot.adapter()?;
        let live = wire::view(
            slot.summary(),
            &adapter.snapshot().await.map_err(|e| wire::error(&e))?,
        );
        let plan = planner::plan(&live, &snap, only, allow_disruptive);
        let mut items = Vec::with_capacity(plan.items.len());
        for p in plan.items {
            let mut item = p.item;
            if let (false, Some(op)) = (dry_run, p.op) {
                let res = match &op {
                    DeviceOp::Param { path, value } => {
                        Self::apply(&adapter, path, value, allow_disruptive).await
                    }
                    DeviceOp::Route { output, source } => adapter
                        .set_route(
                            wire::channel_from_wire(output),
                            source.as_ref().map(wire::channel_from_wire),
                        )
                        .await
                        .map_err(|e| wire::error(&e)),
                };
                match res {
                    Ok(()) => item.status = DeviceRestoreStatus::Applied,
                    Err(e) => {
                        item.status = DeviceRestoreStatus::Failed;
                        item.error = e.to_string();
                    }
                }
            }
            items.push(item);
        }
        Ok(DeviceRestoreReport {
            snapshot: snap.name,
            device: snap.device,
            dry_run,
            unchanged: plan.unchanged,
            items,
        })
    }
}

fn publish(events: &architect::PubSub<DeviceEventWire>, device: &str, event: DeviceEventKind) {
    events.publish(DeviceEventWire {
        device: device.to_owned(),
        event,
    });
}

/// State after a failed connect attempt: discovery finding nothing is
/// *not found* — unless the device was reached before, then it's
/// *offline* (it went away).
const fn link_after_failure(e: &ConnectError, ever_online: bool) -> DeviceLinkState {
    match (e, ever_online) {
        (ConnectError::NotFound(_), false) => DeviceLinkState::NotFound,
        _ => DeviceLinkState::Offline,
    }
}

/// Connect → forward events → on drop, back off and reconnect. Runs for
/// the life of the process.
async fn supervise(
    slot: Arc<Slot>,
    connect: fn(DeviceConfig, ConnectCtx) -> registry::ConnectFuture,
    ctx: ConnectCtx,
    events: architect::PubSub<DeviceEventWire>,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        match connect(slot.config.clone(), ctx.clone()).await {
            Ok(adapter) => {
                let up_since = std::time::Instant::now();
                // Subscribe before announcing so nothing slips between.
                let mut rx = adapter.subscribe();
                let info = adapter.info();
                let id = info.id.to_string();
                {
                    let mut st = slot.state.lock();
                    st.adapter = Some(Arc::clone(&adapter));
                    st.info = Some(info);
                    st.link = DeviceLinkState::Online;
                    st.error.clear();
                    st.ever_online = true;
                }
                tracing::info!(device = %id, name = %slot.config.name, "device online");
                publish(&events, &id, DeviceEventKind::Online);
                let reason = loop {
                    match rx.recv().await {
                        Ok(DeviceEvent::Offline) => break "connection closed".to_owned(),
                        Ok(ev) => publish(&events, &id, wire::event(&ev)),
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!(device = %id, missed = n, "device events lagged");
                            publish(&events, &id, DeviceEventKind::SnapshotReplaced);
                        }
                        Err(RecvError::Closed) => break "event channel closed".to_owned(),
                    }
                };
                {
                    let mut st = slot.state.lock();
                    st.adapter = None;
                    st.link = DeviceLinkState::Offline;
                    st.error.clone_from(&reason);
                }
                tracing::warn!(device = %id, %reason, "device offline; reconnecting");
                publish(&events, &id, DeviceEventKind::Offline);
                if up_since.elapsed() >= SESSION_EARNS_RESET {
                    backoff = BACKOFF_MIN;
                }
            }
            Err(e) => {
                let mut st = slot.state.lock();
                st.link = link_after_failure(&e, st.ever_online);
                let msg = e.message().to_owned();
                if st.error != msg {
                    tracing::info!(name = %slot.config.name, error = %msg, "device connect failed");
                }
                st.error = msg;
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_states() {
        let nf = ConnectError::NotFound("x".into());
        let failed = ConnectError::Failed("x".into());
        assert_eq!(link_after_failure(&nf, false), DeviceLinkState::NotFound);
        assert_eq!(link_after_failure(&nf, true), DeviceLinkState::Offline);
        assert_eq!(link_after_failure(&failed, false), DeviceLinkState::Offline);
    }
}
