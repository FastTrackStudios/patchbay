//! [`Galaxy32Adapter`]: the Galaxy32 as a generic [`DeviceAdapter`].
//!
//! Source of truth is the device: every write is read-modify-write under
//! one lock, then re-read (or awaited in the cyclic state) until the
//! device reports the new value; the confirmed value is what gets
//! emitted as a [`DeviceEvent`]. Other clients' writes arrive as
//! notifications and are emitted too. The official panel will NOT show
//! our writes (it caches) and may later overwrite them with stale state.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use patchbay_device::{
    ChannelRef, Crosspoint, DeviceAdapter, DeviceError, DeviceEvent, DeviceId, DeviceInfo,
    DeviceSnapshot, MirrorUpdate, Param, ParamValue, PortGroup, Transport, WriteGuard, apply_event,
    diff_events,
};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::afx::AfxCatalog;
use super::ops::{AfxSlot, DeviceState, Galaxy32, RouteSlot};
use super::params::{
    self, ParamPath, StripField, afx_effect_path, afx_options, describe, notification_events,
    page_crosspoints, reverb_path, state_events, state_params, strip_path, strip_value, trim_path,
};
use super::tables::{
    self, AFX_SLOTS, AFX_STRIPS, MIXER_STRIPS, MIXERS, MONITOR_OUTPUT, OUTPUT_PAGES, ROUTING_SLOTS,
    SAMPLE_RATES, SOURCE_TYPES, TRIM_LINE_IN,
};
use crate::client::{CYCLIC_STATE_CMD, Client, ClientEvent};
use crate::discovery::{Announce, Listener};
use crate::error::{AntelopeError, Result};
use crate::protocol::envelope::ServerFrame;

const CONFIRM_ATTEMPTS: u32 = 6;
const CONFIRM_DELAY: Duration = Duration::from_millis(80);
const STATE_CONFIRM_TIMEOUT: Duration = Duration::from_millis(1500);
const CLOCK_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

type AfxCache = Arc<Mutex<HashMap<(u8, u8), Vec<Value>>>>;

/// How often the mirror is re-read from the device. Routing writes by
/// other clients (the Antelope panel) are never notified, so this is how
/// they are noticed — and emitted as events.
const MIRROR_REFRESH: Duration = Duration::from_secs(5);

struct Inner {
    dev: Galaxy32,
    info: DeviceInfo,
    online: Arc<AtomicBool>,
    events: broadcast::Sender<DeviceEvent>,
    op_lock: tokio::sync::Mutex<()>,
    afx: AfxCatalog,
    afx_options: Vec<String>,
    afx_cache: AfxCache,
    pump: JoinHandle<()>,
    /// Last known full state: fresh reads, patched by our own events.
    /// `None` = stale (next `current` reads the device).
    mirror: Mutex<Option<DeviceSnapshot>>,
    refresher: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.pump.abort();
        let refresher = self.refresher.lock().take();
        if let Some(r) = refresher {
            r.abort();
        }
    }
}

/// Antelope Galaxy32 behind Antelope's Manager Server.
#[derive(Clone)]
pub struct Galaxy32Adapter {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Galaxy32Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Galaxy32Adapter")
            .field("id", &self.inner.info.id)
            .field("addr", &self.inner.dev.client().addr())
            .finish_non_exhaustive()
    }
}

/// Retry `read` until `ok` holds; the device applies writes
/// asynchronously relative to our reads.
async fn confirm<T, F, Fut>(what: &str, read: F, ok: impl Fn(&T) -> bool) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    for _ in 0..CONFIRM_ATTEMPTS {
        let v = read().await?;
        if ok(&v) {
            return Ok(v);
        }
        tokio::time::sleep(CONFIRM_DELAY).await;
    }
    Err(AntelopeError::NotConfirmed(what.to_owned()))
}

fn invalid(path: &str, reason: impl Into<String>) -> DeviceError {
    DeviceError::InvalidValue {
        target: path.to_owned(),
        reason: reason.into(),
    }
}

fn want_toggle(path: &str, v: &ParamValue) -> Result<bool, DeviceError> {
    match v {
        ParamValue::Toggle(b) => Ok(*b),
        _ => Err(invalid(path, "expected a toggle")),
    }
}

fn want_u8(path: &str, v: &ParamValue) -> Result<u8, DeviceError> {
    match v {
        ParamValue::Int(n) => u8::try_from(*n).map_err(|_| invalid(path, "expected 0..=255")),
        ParamValue::Enum(n) => {
            u8::try_from(*n).map_err(|_| invalid(path, "enum index out of range"))
        }
        ParamValue::Toggle(b) => Ok(u8::from(*b)),
        _ => Err(invalid(path, "expected an integer")),
    }
}

/// How many announced endpoints one discovery pass will try.
///
/// Each attempt is a session the Manager Server has to set up and tear
/// down, and a dead endpoint costs the whole handshake timeout. Two is
/// enough to cover "the first one just died"; more is just churn.
const MAX_ENDPOINT_ATTEMPTS: usize = 2;

impl Galaxy32Adapter {
    /// Connect to a control endpoint described by `announce` (identity
    /// comes from the announce: serial, firmware, server version). Probes
    /// with a read; endpoints that answer `FAIL` are rejected.
    ///
    /// # Errors
    /// Connect failure, or the endpoint doesn't answer reads.
    pub async fn connect(addr: SocketAddr, announce: &Announce) -> Result<Self> {
        let client = Client::connect(addr).await?;
        let dev = Galaxy32::new(client);
        dev.get_routing(0).await?;

        let props = &announce.properties;
        let model = props
            .device_name
            .clone()
            .unwrap_or_else(|| "Galaxy32".to_owned());
        let serial = props.serial_number.clone();
        let info = DeviceInfo {
            id: DeviceId::from_parts(
                "antelope",
                &model.to_lowercase(),
                serial.as_deref().unwrap_or(&announce.uuid),
            ),
            vendor: "Antelope Audio".to_owned(),
            model,
            serial,
            firmware: props.firmware_version.clone(),
            transport: Transport::Tcp {
                addr: addr.to_string(),
                via: Some(format!(
                    "Antelope Manager Server {}",
                    props.server_version.as_deref().unwrap_or("?")
                )),
            },
            online: true,
        };

        let afx = AfxCatalog::load()?;
        let afx_options = afx_options(&afx);
        let (events, _) = broadcast::channel(1024);
        let online = Arc::new(AtomicBool::new(true));
        let afx_cache: AfxCache = Arc::default();
        let pump = tokio::spawn(pump(
            dev.client().subscribe(),
            events.clone(),
            Arc::clone(&online),
            Arc::clone(&afx_cache),
        ));
        Ok(Self {
            inner: Arc::new(Inner {
                dev,
                info,
                online,
                events,
                op_lock: tokio::sync::Mutex::new(()),
                afx,
                afx_options,
                afx_cache,
                pump,
                mirror: Mutex::new(None),
                refresher: Mutex::new(None),
            }),
        }
        .start_mirror())
    }

    /// Start the task that keeps the mirror current: it applies every
    /// event this adapter emits, and re-reads the device every
    /// [`MIRROR_REFRESH`] (emitting whatever changed behind our back).
    fn start_mirror(self) -> Self {
        let weak = Arc::downgrade(&self.inner);
        let rx = self.inner.events.subscribe();
        let task = tokio::spawn(mirror_task(weak, rx));
        *self.inner.refresher.lock() = Some(task);
        self
    }

    /// Full read of the device, without taking the op lock.
    async fn read_snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        Ok(DeviceSnapshot {
            info: DeviceAdapter::info(self),
            inputs: SOURCE_TYPES
                .iter()
                .map(|s| PortGroup {
                    id: s.id.to_owned(),
                    name: s.name.to_owned(),
                    channels: s.channels,
                })
                .collect(),
            outputs: OUTPUT_PAGES
                .iter()
                .map(|p| PortGroup {
                    id: p.id.to_owned(),
                    name: p.name.to_owned(),
                    channels: p.channels,
                })
                .collect(),
            routes: self.routes().await?,
            params: self.params().await?,
        })
    }

    /// Fresh read: replace the mirror and emit an event for everything
    /// that differs from it.
    async fn refresh_mirror(&self) -> Result<DeviceSnapshot, DeviceError> {
        let snap = {
            let _g = self.inner.op_lock.lock().await;
            self.read_snapshot().await?
        };
        let old = self.inner.mirror.lock().replace(snap.clone());
        if let Some(old) = old {
            for ev in diff_events(&old, &snap) {
                self.emit(ev);
            }
        }
        Ok(snap)
    }

    /// Discover for `timeout`, then try the control endpoints of the
    /// device with `serial` (any Galaxy if `None`) best-first, keeping the
    /// first that answers a read.
    ///
    /// Every attempt costs a real session on the Manager Server, so this
    /// listens first and then tries a small number of ranked endpoints —
    /// it does not connect to everything it hears. Connecting to each
    /// announced endpoint in turn is what tore the Thunderbolt device
    /// down: the server keeps announcing endpoints whose sessions have
    /// ended, so "try them all" means a connect every couple of seconds,
    /// forever, and its heartbeat to the hardware does not survive that.
    ///
    /// # Errors
    /// Discovery failure or no usable endpoint.
    pub async fn discover_and_connect(serial: Option<&str>, timeout: Duration) -> Result<Self> {
        let listener = Listener::bind()?;
        let now = tokio::time::Instant::now();
        let deadline = now.checked_add(timeout).unwrap_or(now);
        let mut tried: HashSet<SocketAddr> = HashSet::new();
        let mut heard: Vec<(SocketAddr, Announce)> = Vec::new();
        while let Ok(recv) = tokio::time::timeout_at(deadline, listener.recv()).await {
            let a = recv?;
            if !a.is_control() || serial.is_some_and(|s| a.serial() != Some(s)) {
                continue;
            }
            let Some(addr) = a.socket_addr() else {
                continue;
            };
            if tried.insert(addr) {
                heard.push((addr, a));
            }
        }
        if heard.is_empty() {
            return Err(AntelopeError::NotFound(format!(
                "no control endpoint announced (serial {serial:?})"
            )));
        }

        // Loopback before LAN (same server, shorter path), live before
        // stale. Stale endpoints are only worth a try when nothing at
        // all looked live — otherwise they are known-silent sessions.
        let any_live = heard.iter().any(|(_, a)| a.looks_live());
        let mut order: Vec<(SocketAddr, Announce)> = heard
            .into_iter()
            .filter(|(_, a)| a.looks_live() || !any_live)
            .collect();
        order.sort_by_key(|(addr, a)| (!a.looks_live(), !addr.ip().is_loopback()));
        let considered = order.len();
        order.truncate(MAX_ENDPOINT_ATTEMPTS);

        let mut last_err = None;
        for (addr, a) in order {
            match Self::connect(addr, &a).await {
                Ok(adapter) => return Ok(adapter),
                Err(e) => {
                    tracing::info!(%addr, error = %e, "antelope: endpoint rejected");
                    last_err = Some(e);
                }
            }
        }
        Err(AntelopeError::NotFound(last_err.map_or_else(
            || format!("no usable control endpoint of {considered} announced"),
            |e| {
                format!(
                    "no endpoint answered reads ({considered} announced, tried {}; last error: {e})",
                    considered.min(MAX_ENDPOINT_ATTEMPTS)
                )
            },
        )))
    }

    /// Typed operations / raw escape hatch (`raw_call`, `raw_request`).
    #[must_use]
    pub fn device(&self) -> &Galaxy32 {
        &self.inner.dev
    }

    /// The AFX catalog parsed from the server schema.
    #[must_use]
    pub fn afx_catalog(&self) -> &AfxCatalog {
        &self.inner.afx
    }

    fn emit(&self, ev: DeviceEvent) {
        // Patch the mirror before anyone hears about it, so a `current`
        // right after a confirmed write already has the new value (the
        // mirror task applies the same event again — idempotent).
        {
            let mut mirror = self.inner.mirror.lock();
            if let Some(snap) = mirror.as_mut()
                && apply_event(snap, &ev) == MirrorUpdate::Stale
            {
                *mirror = None;
            }
        }
        // No subscribers is fine.
        let _ = self.inner.events.send(ev);
    }

    /// Resolve an AFX strip/slot (0-based) to its loaded insert.
    async fn afx_slot(&self, strip: u8, slot: u8) -> Result<AfxSlot> {
        self.inner
            .dev
            .get_afx_strip(strip)
            .await?
            .get(usize::from(slot))
            .copied()
            .ok_or_else(|| {
                AntelopeError::Invalid(format!("AFX strip {strip} slot {slot} is empty"))
            })
    }

    /// Write a whole AFX instance config: `values` are all of the loaded
    /// effect's fields in schema order. `strip`/`slot` are 1-based.
    ///
    /// The server rebroadcasts it to other clients; there is no
    /// per-instance read-back yet (TODO afx-read), so this is NOT
    /// confirmed. Values are cached so single fields can then be written
    /// with [`DeviceAdapter::set_param`] (`afx/strip/S/slot/K/<field>`).
    ///
    /// # Errors
    /// Empty slot, unknown effect, bad arity/range, socket failure.
    pub async fn set_afx_conf(&self, strip: u8, slot: u8, values: &[Value]) -> Result<()> {
        let (s, k) = (strip.saturating_sub(1), slot.saturating_sub(1));
        let _g = self.inner.op_lock.lock().await;
        let ins = self.afx_slot(s, k).await?;
        self.send_afx_conf(ins, values).await
    }

    async fn send_afx_conf(&self, ins: AfxSlot, values: &[Value]) -> Result<()> {
        let effect = self.inner.afx.by_type(ins.effect).ok_or_else(|| {
            AntelopeError::Invalid(format!("AFX type {} not in schema", ins.effect))
        })?;
        let call = effect.encode(ins.inst, values)?;
        self.inner.dev.client().send(&call).await?;
        self.inner
            .afx_cache
            .lock()
            .insert((ins.effect, ins.inst), values.to_vec());
        Ok(())
    }

    async fn write(&self, path: &str, p: ParamPath, value: ParamValue) -> Result<(), DeviceError> {
        let dev = &self.inner.dev;
        match p {
            ParamPath::Strip { mixer, ch, field } => {
                return self.write_strip(path, mixer, ch, field, &value).await;
            }
            ParamPath::Reverb { mixer, field } => {
                let mut r = dev.get_reverb(mixer).await?;
                r.set(field, want_u8(path, &value)?);
                dev.set_reverb(&r).await?;
                confirm(path, || dev.get_reverb(mixer), |x| *x == r).await?;
                self.emit(DeviceEvent::ParamChanged {
                    path: reverb_path(mixer, field),
                    value,
                });
            }
            ParamPath::MonitorToggle(t) => {
                let on = want_toggle(path, &value)?;
                dev.set_monitor_toggle(t, MONITOR_OUTPUT, on).await?;
                dev.wait_state(STATE_CONFIRM_TIMEOUT, |s| s.monitor_toggle(t) == on)
                    .await
                    .ok_or_else(|| AntelopeError::NotConfirmed(path.to_owned()))?;
                self.emit(DeviceEvent::ParamChanged {
                    path: path.to_owned(),
                    value,
                });
            }
            ParamPath::MonitorVolume => {
                let ParamValue::Int(n) = value else {
                    return Err(invalid(path, "expected an integer"));
                };
                let vol = u16::try_from(n).map_err(|_| invalid(path, "expected 0..=65535"))?;
                dev.set_monitor_volume(MONITOR_OUTPUT, vol).await?;
                dev.wait_state(STATE_CONFIRM_TIMEOUT, |s| s.monitor.volume == vol)
                    .await
                    .ok_or_else(|| AntelopeError::NotConfirmed(path.to_owned()))?;
                self.emit(DeviceEvent::ParamChanged {
                    path: path.to_owned(),
                    value,
                });
            }
            ParamPath::TrimControl | ParamPath::TrimLevel { .. } => {
                return self.write_trim(path, &p, value).await;
            }
            ParamPath::SyncSource => {
                let i = want_u8(path, &value)?;
                dev.set_sync_source_index(i).await?;
                dev.wait_state(CLOCK_CONFIRM_TIMEOUT, |s| s.sync_source == i)
                    .await
                    .ok_or_else(|| AntelopeError::NotConfirmed(path.to_owned()))?;
                self.emit(DeviceEvent::ParamChanged {
                    path: path.to_owned(),
                    value,
                });
            }
            ParamPath::SampleRate => {
                let i = want_u8(path, &value)?;
                let hz = *SAMPLE_RATES
                    .get(usize::from(i))
                    .ok_or_else(|| invalid(path, "sample rate index out of range"))?;
                dev.set_sample_rate_index(i).await?;
                dev.wait_state(CLOCK_CONFIRM_TIMEOUT, |s| s.sample_rate == hz)
                    .await
                    .ok_or_else(|| AntelopeError::NotConfirmed(path.to_owned()))?;
                self.emit(DeviceEvent::ParamChanged {
                    path: path.to_owned(),
                    value,
                });
            }
            ParamPath::MeasuredRate | ParamPath::Locked => {
                return Err(DeviceError::ReadOnly(path.to_owned()));
            }
            ParamPath::AfxEffect { strip, slot } => {
                return self.write_afx_effect(path, strip, slot, &value).await;
            }
            ParamPath::AfxField { strip, slot, field } => {
                return self.write_afx_field(path, strip, slot, &field, value).await;
            }
        }
        Ok(())
    }

    async fn write_strip(
        &self,
        path: &str,
        mixer: u8,
        ch: u8,
        field: StripField,
        value: &ParamValue,
    ) -> Result<(), DeviceError> {
        let dev = &self.inner.dev;
        let mut s = dev.get_mixer_strip(mixer, ch).await?;
        match (field, value) {
            (StripField::Level, ParamValue::Level(db)) => {
                s.level = params::db_attenuation(*db).ok_or_else(|| invalid(path, "bad dB"))?;
            }
            (StripField::Send, ParamValue::Level(db)) => {
                s.send = params::db_attenuation(*db).ok_or_else(|| invalid(path, "bad dB"))?;
            }
            (StripField::Pan, ParamValue::Pan(p)) => {
                s.pan = params::f64_pan(*p).ok_or_else(|| invalid(path, "bad pan"))?;
            }
            (StripField::Mute, v) => s.mute = u8::from(want_toggle(path, v)?),
            (StripField::Solo, v) => s.solo = u8::from(want_toggle(path, v)?),
            _ => return Err(invalid(path, "value type doesn't fit the field")),
        }
        dev.set_mixer_strip(mixer, ch, &s).await?;
        let rb = confirm(path, || dev.get_mixer_strip(mixer, ch), |r| *r == s).await?;
        for f in StripField::ALL {
            self.emit(DeviceEvent::ParamChanged {
                path: strip_path(mixer, ch, f),
                value: strip_value(f, rb),
            });
        }
        Ok(())
    }

    async fn write_trim(
        &self,
        path: &str,
        p: &ParamPath,
        value: ParamValue,
    ) -> Result<(), DeviceError> {
        let dev = &self.inner.dev;
        let mut cfg = dev.get_trim_config(TRIM_LINE_IN).await?;
        cfg.levels.truncate(ROUTING_SLOTS);
        match (p, &value) {
            (ParamPath::TrimControl, v) => {
                let c = want_u8(path, v)?;
                if c > 1 {
                    return Err(invalid(path, "0 = ALL, 1 = MANUAL"));
                }
                cfg.control = c;
            }
            (ParamPath::TrimLevel { ch }, ParamValue::Level(dbu)) => {
                let l = params::dbu_trim(*dbu).ok_or_else(|| invalid(path, "dBu out of range"))?;
                *cfg.levels
                    .get_mut(usize::from(*ch))
                    .ok_or_else(|| invalid(path, "no such channel"))? = l;
            }
            _ => return Err(invalid(path, "expected a level in dBu")),
        }
        dev.set_trim_config(&cfg).await?;
        confirm(
            path,
            || dev.get_trim_config(TRIM_LINE_IN),
            |r| {
                r.control == cfg.control
                    && r.levels.get(..ROUTING_SLOTS) == Some(cfg.levels.as_slice())
            },
        )
        .await?;
        self.emit(DeviceEvent::ParamChanged {
            path: path.to_owned(),
            value,
        });
        Ok(())
    }

    async fn write_afx_effect(
        &self,
        path: &str,
        strip: u8,
        slot: u8,
        value: &ParamValue,
    ) -> Result<(), DeviceError> {
        let dev = &self.inner.dev;
        let ty = want_u8(path, value)?;
        if ty != 0 && self.inner.afx.by_type(ty).is_none() {
            return Err(invalid(path, format!("unknown AFX type {ty}")));
        }
        let mut chain = dev.get_afx_strip(strip).await?;
        let idx = usize::from(slot);
        if ty == 0 {
            if idx < chain.len() {
                chain.remove(idx);
            }
        } else {
            let inst = dev.allocate_afx_instance(ty).await?;
            let ins = AfxSlot { effect: ty, inst };
            match chain.get_mut(idx) {
                Some(s) => *s = ins,
                None => chain.push(ins),
            }
        }
        dev.set_afx_strip(strip, &chain).await?;
        confirm(path, || dev.get_afx_strip(strip), |r| *r == chain).await?;
        for (k, s) in (0..).zip(0..AFX_SLOTS) {
            let t = chain.get(s).map_or(0, |x| x.effect);
            self.emit(DeviceEvent::ParamChanged {
                path: afx_effect_path(strip, k),
                value: ParamValue::Enum(u32::from(t)),
            });
        }
        Ok(())
    }

    async fn write_afx_field(
        &self,
        path: &str,
        strip: u8,
        slot: u8,
        field: &str,
        value: ParamValue,
    ) -> Result<(), DeviceError> {
        let ins = self.afx_slot(strip, slot).await?;
        let effect = self
            .inner
            .afx
            .by_type(ins.effect)
            .ok_or_else(|| invalid(path, format!("AFX type {} not in schema", ins.effect)))?;
        let fi = effect
            .field_index(field)
            .ok_or_else(|| DeviceError::UnknownParam(path.to_owned()))?;
        let mut values = self
            .inner
            .afx_cache
            .lock()
            .get(&(ins.effect, ins.inst))
            .cloned()
            .ok_or_else(|| {
                DeviceError::Unsupported(format!(
                    "{path}: current values of {} #{} are unknown (no AFX read-back yet); \
                     write the whole config with Galaxy32Adapter::set_afx_conf first",
                    effect.name, ins.inst
                ))
            })?;
        let v = match &value {
            ParamValue::Int(n) => Value::from(*n),
            ParamValue::Toggle(b) => Value::from(u8::from(*b)),
            ParamValue::Level(x) | ParamValue::Pan(x) => Value::from(*x),
            ParamValue::Enum(n) => Value::from(*n),
            ParamValue::Text(_) => return Err(invalid(path, "expected a number")),
        };
        *values
            .get_mut(fi)
            .ok_or_else(|| invalid(path, "field index"))? = v;
        self.send_afx_conf(ins, &values).await?;
        self.emit(DeviceEvent::ParamChanged {
            path: path.to_owned(),
            value,
        });
        Ok(())
    }

    async fn routes(&self) -> Result<Vec<Crosspoint>> {
        let mut out = Vec::new();
        for p in &OUTPUT_PAGES {
            let page = self.inner.dev.get_routing(p.page).await?;
            out.extend(page_crosspoints(p.page, &page.slots));
        }
        Ok(out)
    }

    fn param(&self, path: String, value: ParamValue) -> Option<Param> {
        let p = ParamPath::parse(&path)?;
        let d = describe(&p, &self.inner.afx_options);
        Some(Param {
            path,
            label: d.label,
            kind: d.kind,
            value,
            writable: d.writable,
            disruptive: d.disruptive,
        })
    }

    async fn params(&self) -> Result<Vec<Param>> {
        let dev = &self.inner.dev;
        let mut raw: Vec<(String, ParamValue)> = Vec::new();
        for m in 0..MIXERS {
            let strips = dev.get_mixer(m).await?;
            for (ch, s) in (0..=MIXER_STRIPS).zip(&strips) {
                for f in StripField::ALL {
                    raw.push((strip_path(m, ch, f), strip_value(f, *s)));
                }
            }
            let r = dev.get_reverb(m).await?;
            for f in super::ops::ReverbConfig::FIELDS {
                let v = r.get(f).unwrap_or_default();
                raw.push((
                    reverb_path(m, f),
                    if f == "on" {
                        ParamValue::Toggle(v != 0)
                    } else {
                        ParamValue::Int(i64::from(v))
                    },
                ));
            }
        }
        if let Some(s) = dev.state() {
            raw.extend(state_params(&s));
        }
        let trims = dev.get_trim_config(TRIM_LINE_IN).await?;
        raw.push((
            "trim/line_in/control".to_owned(),
            ParamValue::Enum(u32::from(trims.control)),
        ));
        for (ch, l) in (0..).zip(trims.levels.iter().take(ROUTING_SLOTS)) {
            raw.push((trim_path(ch), ParamValue::Level(l.dbu())));
        }
        let cache = self.inner.afx_cache.lock().clone();
        for strip in 0..AFX_STRIPS {
            let chain = dev.get_afx_strip(strip).await?;
            for (k, s) in (0..).zip(0..AFX_SLOTS) {
                let ins = chain.get(s);
                raw.push((
                    afx_effect_path(strip, k),
                    ParamValue::Enum(u32::from(ins.map_or(0, |x| x.effect))),
                ));
                // Field values only where we know them (our own writes).
                let Some(ins) = ins else { continue };
                let (Some(effect), Some(values)) = (
                    self.inner.afx.by_type(ins.effect),
                    cache.get(&(ins.effect, ins.inst)),
                ) else {
                    continue;
                };
                for (f, v) in effect.fields.iter().zip(values) {
                    if let Some(n) = v.as_i64() {
                        raw.push((
                            format!(
                                "afx/strip/{}/slot/{}/{}",
                                u16::from(strip).saturating_add(1),
                                u16::from(k).saturating_add(1),
                                f.name
                            ),
                            ParamValue::Int(n),
                        ));
                    }
                }
            }
        }
        Ok(raw
            .into_iter()
            .filter_map(|(p, v)| self.param(p, v))
            .collect())
    }
}

impl DeviceAdapter for Galaxy32Adapter {
    fn info(&self) -> DeviceInfo {
        let mut i = self.inner.info.clone();
        i.online = self.inner.online.load(Ordering::SeqCst);
        i
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        self.refresh_mirror().await
    }

    /// The mirror (at most [`MIRROR_REFRESH`] behind for changes the
    /// server never notifies — routing by other clients; everything we
    /// write or are notified of is applied immediately).
    async fn current(&self) -> Result<DeviceSnapshot, DeviceError> {
        let cached = self.inner.mirror.lock().clone();
        match cached {
            Some(mut snap) => {
                snap.info = DeviceAdapter::info(self);
                Ok(snap)
            }
            None => self.refresh_mirror().await,
        }
    }

    async fn apply_param(
        &self,
        path: &str,
        value: ParamValue,
        guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        let p = ParamPath::parse(path).ok_or_else(|| DeviceError::UnknownParam(path.to_owned()))?;
        let d = describe(&p, &self.inner.afx_options);
        if !d.writable {
            return Err(DeviceError::ReadOnly(path.to_owned()));
        }
        if d.disruptive && guard != WriteGuard::AllowDisruptive {
            return Err(DeviceError::DisruptiveWrite(path.to_owned()));
        }
        if !value.matches(&d.kind) && !matches!(p, ParamPath::AfxField { .. }) {
            return Err(invalid(path, format!("{value:?} doesn't fit {:?}", d.kind)));
        }
        let _g = self.inner.op_lock.lock().await;
        self.write(path, p, value).await
    }

    async fn set_route(
        &self,
        output: ChannelRef,
        source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        let page = tables::output_page_by_id(&output.group)
            .ok_or_else(|| DeviceError::UnknownPort(output.group.clone()))?;
        if output.channel >= page.channels {
            return Err(DeviceError::UnknownPort(output.to_string()));
        }
        let slot = match &source {
            None => RouteSlot::NONE,
            Some(src) => {
                let st = tables::source_type_by_id(&src.group)
                    .ok_or_else(|| DeviceError::UnknownPort(src.group.clone()))?;
                if src.channel >= st.channels {
                    return Err(DeviceError::UnknownPort(src.to_string()));
                }
                RouteSlot {
                    ty: st.ty,
                    ch: u8::try_from(src.channel)
                        .map_err(|_| DeviceError::UnknownPort(src.to_string()))?,
                }
            }
        };
        let idx = usize::from(output.channel);
        let dev = &self.inner.dev;
        let _g = self.inner.op_lock.lock().await;
        let mut cur = dev.get_routing(page.page).await?;
        *cur.slots
            .get_mut(idx)
            .ok_or_else(|| DeviceError::UnknownPort(output.to_string()))? = slot;
        dev.set_routing(page.page, &cur.slots).await?;
        let what = format!("route {output}");
        let rb = confirm(
            &what,
            || dev.get_routing(page.page),
            |r| r.slots == cur.slots,
        )
        .await?;
        let confirmed = rb.slots.get(idx).copied().and_then(params::slot_source);
        self.emit(DeviceEvent::RouteChanged(Crosspoint {
            output,
            source: confirmed,
        }));
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.inner.events.subscribe()
    }
}

/// Client frames → device events.
async fn pump(
    mut rx: broadcast::Receiver<ClientEvent>,
    events: broadcast::Sender<DeviceEvent>,
    online: Arc<AtomicBool>,
    afx_cache: AfxCache,
) {
    let mut last: Option<DeviceState> = None;
    loop {
        match rx.recv().await {
            Ok(ClientEvent::Frame(f)) => match &*f {
                ServerFrame::Cyclic {
                    header, contents, ..
                } if header.cmd == CYCLIC_STATE_CMD => {
                    if let Some(s) = DeviceState::from_cyclic(contents) {
                        if last.is_some() {
                            for ev in state_events(last.as_ref(), &s) {
                                let _ = events.send(ev);
                            }
                        }
                        last = Some(s);
                    }
                }
                ServerFrame::Notification { .. } => {
                    let Some(call) = f.notified_call() else {
                        continue;
                    };
                    if call.method.starts_with("set_") && call.method.ends_with("_conf") {
                        // [type_id, inst_id, values…] — remember for
                        // single-field writes.
                        let ty = call
                            .args
                            .first()
                            .and_then(Value::as_u64)
                            .and_then(|n| u8::try_from(n).ok());
                        let inst = call
                            .args
                            .get(1)
                            .and_then(Value::as_u64)
                            .and_then(|n| u8::try_from(n).ok());
                        if let (Some(ty), Some(inst), Some(rest)) = (ty, inst, call.args.get(2..)) {
                            afx_cache.lock().insert((ty, inst), rest.to_vec());
                        }
                    }
                    for ev in notification_events(&call) {
                        let _ = events.send(ev);
                    }
                }
                _ => {}
            },
            Ok(ClientEvent::Closed) => {
                online.store(false, Ordering::SeqCst);
                let _ = events.send(DeviceEvent::Offline);
                break;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    missed = n,
                    "antelope: event pump lagged; asking for a re-read"
                );
                let _ = events.send(DeviceEvent::SnapshotReplaced);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Keep the mirror current (see [`Galaxy32Adapter::start_mirror`]).
async fn mirror_task(weak: Weak<Inner>, mut rx: broadcast::Receiver<DeviceEvent>) {
    let mut tick = tokio::time::interval(MIRROR_REFRESH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(inner) = weak.upgrade() else { break };
                let mut mirror = inner.mirror.lock();
                match ev {
                    Ok(ev) => {
                        if let Some(snap) = mirror.as_mut()
                            && apply_event(snap, &ev) == MirrorUpdate::Stale
                        {
                            *mirror = None;
                        }
                    }
                    // Missed events: the mirror can't be trusted.
                    Err(broadcast::error::RecvError::Lagged(_)) => *mirror = None,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                let Some(inner) = weak.upgrade() else { break };
                let adapter = Galaxy32Adapter { inner };
                if !adapter.inner.online.load(Ordering::SeqCst) {
                    continue;
                }
                if let Err(e) = adapter.refresh_mirror().await {
                    tracing::debug!(error = %e, "antelope: mirror refresh failed");
                }
            }
        }
    }
}
