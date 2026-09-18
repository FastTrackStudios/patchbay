//! [`TfAdapter`]: a Yamaha TF console as a generic [`DeviceAdapter`].
//!
//! - **Params only.** Over RCP the TF exposes faders, on keys, sends,
//!   pans, labels, DCA and mute-group masters and scenes — but no input
//!   patch, no output patch and no Dante patch ("Dante Patch from Console:
//!   No"). The adapter therefore reports **no port groups and no
//!   crosspoints**, and [`DeviceAdapter::set_route`] returns
//!   [`DeviceError::Unsupported`]. Dante subscriptions feeding the TF's
//!   NY64-D card belong to a Dante adapter.
//! - **Source of truth is the console.** Every write is `set` (the
//!   console answers `OK set …` with the applied, possibly clamped value)
//!   followed by a `get` read-back; the event carries the read-back.
//!   `NOTIFY set` lines (surface, TF Editor, `StageMix`, other RCP clients)
//!   become [`DeviceEvent::ParamChanged`].
//! - **Scenes.** A recall changes everything but does not reliably emit
//!   per-parameter NOTIFYs, so after any recall — ours or a NOTIFY'd
//!   `sscurrent_ex`/`ssrecall_ex` — the adapter waits a debounce, re-reads
//!   everything, emits `ParamChanged` for what changed and then
//!   [`DeviceEvent::SnapshotReplaced`]. Recall is disruptive and needs
//!   [`WriteGuard::AllowDisruptive`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::join_all;
use parking_lot::Mutex;
use patchbay_device::{
    ChannelRef, DeviceAdapter, DeviceError, DeviceEvent, DeviceId, DeviceInfo, DeviceSnapshot,
    Param, ParamValue, Transport, WriteGuard,
};
use tokio::sync::{Notify, broadcast};
use tokio::task::JoinHandle;

use crate::client::{Client, ClientEvent, ClientOptions, Notification};
use crate::error::{Result, TfError};
use crate::rcp::command::{Command, MAX_SCENE, SceneBank};
use crate::rcp::reply::{ParamReply, PrmInfo, SceneCurrent, SceneInfo};
use crate::tf::table::{
    ModelLimits, ParamDef, ParamTable, SceneField, Target, ValueKind, embedded_prminfo,
};
use crate::tf::values::{decode, encode, param_kind};
use crate::tf::vocab::Vocab;

/// Why `set_route` is unsupported.
pub const NO_ROUTING: &str = "Yamaha TF exposes no input, output or Dante patch over RCP; \
     Dante subscriptions belong to a Dante adapter";

const PRMINFO_BATCH: u32 = 16;
const PRMINFO_MAX: u32 = 1024;

/// Adapter tuning.
#[derive(Debug, Clone)]
pub struct TfOptions {
    /// Connection options (keepalive, rate limit, reconnect…).
    pub client: ClientOptions,
    /// Explicit device identity (`yamaha:tf1:<this>`). TF1 V4.55 reports
    /// an empty `serialno` and `devicename`, so without this the id falls
    /// back to the console's IP address.
    pub device_id: Option<String>,
    /// Enumerate `prminfo` at connect (else use the embedded TF1 table).
    pub discover_params: bool,
    /// Wait after a scene recall before re-reading (fades, NOTIFY bursts).
    pub rescan_debounce: Duration,
    /// Concurrent `get`s during a full read (the client window also caps).
    pub read_concurrency: usize,
}

impl Default for TfOptions {
    fn default() -> Self {
        Self {
            client: ClientOptions::default(),
            device_id: None,
            discover_params: true,
            rescan_debounce: Duration::from_millis(500),
            // The console answers a burst of 320 pipelined `get`s in
            // ~0.1 s; the full read is round-trip bound, not console bound.
            read_concurrency: 256,
        }
    }
}

/// Current scene as read from the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneState {
    /// Bank/number/modified.
    pub current: SceneCurrent,
    /// Title from `ssinfo_ex`, if it answered.
    pub title: Option<String>,
}

struct State {
    values: HashMap<String, ParamValue>,
    vocab: Vocab,
}

struct Core {
    client: Client,
    table: ParamTable,
    info: DeviceInfo,
    product: String,
    state: Mutex<State>,
    events: broadcast::Sender<DeviceEvent>,
    online: AtomicBool,
    rescan: Notify,
    opts: TfOptions,
}

fn invalid(path: &str, reason: impl Into<String>) -> DeviceError {
    DeviceError::InvalidValue {
        target: path.to_owned(),
        reason: reason.into(),
    }
}

/// Parse `A05` / `b22` / `B7` → bank + number.
///
/// # Errors
/// [`TfError::Invalid`] on anything else.
pub fn parse_scene(s: &str) -> Result<(SceneBank, u8)> {
    let bad = || TfError::Invalid(format!("scene {s:?}: expected A00..A99 or B00..B99"));
    let mut chars = s.trim().chars();
    let bank = chars
        .next()
        .and_then(SceneBank::from_letter)
        .ok_or_else(bad)?;
    let n: u8 = chars.as_str().parse().map_err(|_| bad())?;
    if n > MAX_SCENE {
        return Err(bad());
    }
    Ok((bank, n))
}

impl Core {
    fn emit(&self, ev: DeviceEvent) {
        // No subscribers is fine.
        let _ = self.events.send(ev);
    }

    /// Store device-reported values; emit `ParamChanged` for changes.
    fn apply(&self, values: Vec<(String, ParamValue)>, emit: bool) {
        let mut changed = Vec::new();
        {
            let mut st = self.state.lock();
            for (path, v) in values {
                if st.values.get(&path) != Some(&v) {
                    st.values.insert(path.clone(), v.clone());
                    changed.push((path, v));
                }
            }
        }
        if emit {
            for (path, value) in changed {
                self.emit(DeviceEvent::ParamChanged { path, value });
            }
        }
    }

    async fn read_scene(&self) -> Result<Option<SceneState>> {
        let mut current = None;
        for bank in SceneBank::ALL {
            // Only the active bank answers OK; the other answers ERROR.
            match self.client.request(&Command::sscurrent(bank)).await {
                Ok(r) => {
                    current = Some(SceneCurrent::from_args(&r.args)?);
                    break;
                }
                Err(TfError::Rejected { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        let Some(current) = current else {
            return Ok(None);
        };
        let title = match self
            .client
            .request(&Command::ssinfo(current.bank, current.number))
            .await
        {
            Ok(r) => SceneInfo::from_args(&r.args).ok().map(|i| i.title),
            Err(TfError::Rejected { .. }) => None,
            Err(e) => return Err(e),
        };
        Ok(Some(SceneState { current, title }))
    }

    fn scene_values(scene: &SceneState) -> Vec<(String, ParamValue)> {
        let label = scene.current.label();
        let mut v = vec![
            ("scene/current".to_owned(), ParamValue::Text(label.clone())),
            ("scene/recall".to_owned(), ParamValue::Text(label)),
            (
                "scene/modified".to_owned(),
                ParamValue::Toggle(scene.current.modified),
            ),
        ];
        if let Some(t) = &scene.title {
            v.push(("scene/title".to_owned(), ParamValue::Text(t.clone())));
        }
        v
    }

    async fn get_def(&self, i: usize) -> Result<ParamReply> {
        match self.table.defs().get(i).map(|d| &d.target) {
            Some(Target::Rcp { address, x, y }) => self.client.get(address, *x, *y).await,
            _ => Err(TfError::Invalid(format!("param #{i} is not an RCP param"))),
        }
    }

    /// Read every mapped param (pipelined, bounded). Rejected reads are
    /// skipped; connection failures abort.
    async fn read_all(&self) -> Result<Vec<(String, ParamValue)>> {
        let defs = self.table.defs();
        // Indices, not references: stream closures over borrowed items trip
        // the higher-ranked `Send` inference of `async fn` in traits.
        let rcp: Vec<usize> = defs
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d.target, Target::Rcp { .. }))
            .map(|(i, _)| i)
            .collect();
        let results: Vec<(usize, Result<ParamReply>)> = futures_util::stream::iter(rcp)
            .map(|i| async move { (i, self.get_def(i).await) })
            .buffer_unordered(self.opts.read_concurrency.max(1))
            .collect()
            .await;
        let mut out = Vec::with_capacity(results.len());
        let mut rejected = 0usize;
        {
            let mut st = self.state.lock();
            for (i, r) in results {
                let Some(def) = defs.get(i) else { continue };
                match r {
                    Ok(reply) => {
                        if let Some(v) = decode(def.kind, &reply.value, &mut st.vocab) {
                            out.push((def.path.clone(), v));
                        } else {
                            tracing::debug!(path = %def.path, value = %reply.value,
                                "yamaha: undecodable value");
                        }
                    }
                    Err(TfError::Rejected { command, reason }) => {
                        rejected = rejected.saturating_add(1);
                        tracing::debug!(%command, %reason, "yamaha: read rejected");
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if rejected > 0 {
            tracing::warn!(rejected, "yamaha: some reads were rejected");
        }
        if let Some(scene) = self.read_scene().await? {
            out.extend(Self::scene_values(&scene));
        }
        Ok(out)
    }

    async fn refresh(&self, emit: bool) -> Result<()> {
        let values = self.read_all().await?;
        self.apply(values, emit);
        Ok(())
    }

    fn on_notify(&self, n: &Notification) {
        if n.is_scene_change() {
            tracing::debug!(line = %n.line, "yamaha: scene change; scheduling re-read");
            self.rescan.notify_one();
            return;
        }
        let Some(p) = n.param() else {
            return;
        };
        let Some(def) = self.table.by_rcp(&p.address, p.x, p.y) else {
            tracing::trace!(line = %n.line, "yamaha: NOTIFY for an unmapped param");
            return;
        };
        let v = decode(def.kind, &p.value, &mut self.state.lock().vocab);
        let Some(v) = v else {
            tracing::debug!(line = %n.line, "yamaha: undecodable NOTIFY value");
            return;
        };
        self.state.lock().values.insert(def.path.clone(), v.clone());
        self.emit(DeviceEvent::ParamChanged {
            path: def.path.clone(),
            value: v,
        });
    }

    fn params(&self) -> Vec<Param> {
        let st = self.state.lock();
        self.table
            .defs()
            .iter()
            .filter_map(|d| {
                st.values.get(&d.path).map(|v| Param {
                    path: d.path.clone(),
                    label: d.label.clone(),
                    kind: param_kind(d.kind, &st.vocab),
                    value: v.clone(),
                    writable: d.writable,
                    disruptive: d.disruptive,
                })
            })
            .collect()
    }

    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            online: self.online.load(Ordering::SeqCst),
            ..self.info.clone()
        }
    }

    fn snapshot(&self) -> DeviceSnapshot {
        DeviceSnapshot {
            info: self.info(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            params: self.params(),
        }
    }

    async fn recall(&self, bank: SceneBank, n: u8) -> Result<()> {
        self.client.request(&Command::ssrecall(bank, n)).await?;
        tokio::time::sleep(self.opts.rescan_debounce).await;
        self.refresh(true).await?;
        self.emit(DeviceEvent::SnapshotReplaced);
        Ok(())
    }

    async fn write_rcp(
        &self,
        def: &ParamDef,
        address: &str,
        x: u16,
        y: u16,
        value: &ParamValue,
    ) -> Result<(), DeviceError> {
        let path = def.path.as_str();
        let wire =
            encode(def.kind, value, &self.state.lock().vocab).map_err(|r| invalid(path, r))?;
        let echo = self.client.set(address, x, y, wire).await?;
        if echo.address != address || echo.x != x || echo.y != y {
            return Err(
                TfError::Protocol(format!("`OK set` echoed another param: {echo:?}")).into(),
            );
        }
        let rb = self.client.get(address, x, y).await?;
        let confirmed = decode(def.kind, &rb.value, &mut self.state.lock().vocab)
            .ok_or_else(|| TfError::NotConfirmed(format!("{path}: unreadable {}", rb.value)))?;
        if rb.value != echo.value {
            tracing::warn!(path, set = %echo.value, read = %rb.value,
                "yamaha: read-back differs from the `OK set` echo");
        }
        self.state
            .lock()
            .values
            .insert(path.to_owned(), confirmed.clone());
        self.emit(DeviceEvent::ParamChanged {
            path: path.to_owned(),
            value: confirmed,
        });
        Ok(())
    }
}

/// Client events → device events; scene changes → debounced re-read.
async fn pump(core: Arc<Core>, mut rx: broadcast::Receiver<ClientEvent>) {
    loop {
        match rx.recv().await {
            Ok(ClientEvent::Notify(n)) => core.on_notify(&n),
            Ok(ClientEvent::Connected { reconnect }) => {
                core.online.store(true, Ordering::SeqCst);
                if reconnect {
                    core.emit(DeviceEvent::Online);
                    core.rescan.notify_one();
                }
            }
            Ok(ClientEvent::Disconnected) => {
                core.online.store(false, Ordering::SeqCst);
                core.emit(DeviceEvent::Offline);
            }
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "yamaha: event pump lagged; re-reading");
                core.rescan.notify_one();
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn rescanner(core: Arc<Core>) {
    loop {
        core.rescan.notified().await;
        tokio::time::sleep(core.opts.rescan_debounce).await;
        match core.refresh(true).await {
            Ok(()) => core.emit(DeviceEvent::SnapshotReplaced),
            Err(e) => tracing::warn!(error = %e, "yamaha: re-read failed"),
        }
    }
}

/// Enumerate `prminfo 0..` until the console answers ERROR.
async fn discover_prminfo(client: &Client) -> Result<Vec<PrmInfo>> {
    let mut rows = Vec::new();
    let mut start = 0u32;
    while start < PRMINFO_MAX {
        let end = start.saturating_add(PRMINFO_BATCH);
        let batch = (start..end).map(|i| async move { client.request(&Command::prminfo(i)).await });
        for r in join_all(batch).await {
            match r {
                Ok(reply) => rows.push(PrmInfo::from_args(&reply.args)?),
                Err(TfError::Rejected { .. }) => return Ok(rows),
                Err(e) => return Err(e),
            }
        }
        start = end;
    }
    Ok(rows)
}

struct Inner {
    core: Arc<Core>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// A Yamaha TF console (TF1 first) over RCP.
#[derive(Clone)]
pub struct TfAdapter {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TfAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TfAdapter")
            .field("id", &self.inner.core.info.id)
            .field("addr", &self.inner.core.client.addr())
            .finish_non_exhaustive()
    }
}

impl TfAdapter {
    /// Connect with default options (sends `scpmode keepalive 10000`).
    ///
    /// # Errors
    /// Connect failure, or the console doesn't answer `devinfo` / reads.
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        Self::connect_with(addr, TfOptions::default()).await
    }

    /// Connect: `devinfo`, `prminfo` discovery (fallback: embedded TF1
    /// table), full read, then start the NOTIFY pump.
    ///
    /// Sends only reads plus the optional `scpmode keepalive` session
    /// setting; never changes mixer state.
    ///
    /// # Errors
    /// Connect failure, or the console doesn't answer `devinfo` / reads.
    pub async fn connect_with(addr: SocketAddr, opts: TfOptions) -> Result<Self> {
        let client = Client::connect_with(addr, opts.client.clone()).await?;
        let rx = client.subscribe();
        let product = client.devinfo("productname").await?;
        let firmware = client.devinfo("version").await.ok();
        let nonempty = |s: Result<String>| s.ok().filter(|s| !s.is_empty());
        let serial = nonempty(client.devinfo("serialno").await);
        let devicename = nonempty(client.devinfo("devicename").await);

        let limits = ModelLimits::for_product(&product).unwrap_or_else(|| {
            tracing::warn!(%product, "yamaha: unknown model; using prminfo counts unclamped");
            ModelLimits::UNLIMITED
        });
        let rows = if opts.discover_params {
            match discover_prminfo(&client).await {
                Ok(rows) if !rows.is_empty() => rows,
                Ok(_) => embedded_prminfo()?,
                Err(e) => {
                    tracing::warn!(error = %e, "yamaha: prminfo discovery failed; using the embedded TF1 table");
                    embedded_prminfo()?
                }
            }
        } else {
            embedded_prminfo()?
        };
        let table = ParamTable::build(&rows, &limits);

        let ident = opts
            .device_id
            .clone()
            .or_else(|| serial.clone())
            .or(devicename)
            .unwrap_or_else(|| addr.ip().to_string());
        let info = DeviceInfo {
            id: DeviceId::from_parts("yamaha", &product.to_lowercase(), &ident),
            vendor: "Yamaha".to_owned(),
            model: product.clone(),
            serial,
            firmware,
            transport: Transport::Tcp {
                addr: addr.to_string(),
                via: None,
            },
            online: true,
        };
        let (events, _) = broadcast::channel(8192);
        let core = Arc::new(Core {
            client,
            table,
            info,
            product,
            state: Mutex::new(State {
                values: HashMap::new(),
                vocab: Vocab::default(),
            }),
            events,
            online: AtomicBool::new(true),
            rescan: Notify::new(),
            opts,
        });
        core.refresh(false).await?;
        let tasks = vec![
            tokio::spawn(pump(Arc::clone(&core), rx)),
            tokio::spawn(rescanner(Arc::clone(&core))),
        ];
        Ok(Self {
            inner: Arc::new(Inner { core, tasks }),
        })
    }

    /// The RCP client (escape hatch: raw `get`, `devinfo`, …).
    #[must_use]
    pub fn client(&self) -> &Client {
        &self.inner.core.client
    }

    /// `devinfo productname` as read at connect (`TF1`).
    #[must_use]
    pub fn product(&self) -> &str {
        &self.inner.core.product
    }

    /// The parameter table in use.
    #[must_use]
    pub fn table(&self) -> &ParamTable {
        &self.inner.core.table
    }

    /// Last device-reported state without I/O (kept current by NOTIFYs
    /// and re-reads; [`DeviceAdapter::snapshot`] re-reads everything).
    #[must_use]
    pub fn cached_snapshot(&self) -> DeviceSnapshot {
        self.inner.core.snapshot()
    }

    /// Read the current scene (`sscurrent_ex` + `ssinfo_ex`). Read-only.
    ///
    /// # Errors
    /// Connection failure or a malformed reply.
    pub async fn current_scene(&self) -> Result<Option<SceneState>> {
        self.inner.core.read_scene().await
    }

    /// Recall scene `bank`/`n` (0–99), then re-read everything and emit
    /// [`DeviceEvent::SnapshotReplaced`]. **Changes the whole mix.**
    ///
    /// # Errors
    /// [`DeviceError::DisruptiveWrite`] without
    /// [`WriteGuard::AllowDisruptive`]; console rejection; connection errors.
    pub async fn recall_scene(
        &self,
        bank: SceneBank,
        n: u8,
        guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        if guard != WriteGuard::AllowDisruptive {
            return Err(DeviceError::DisruptiveWrite("scene/recall".to_owned()));
        }
        if n > MAX_SCENE {
            return Err(invalid("scene/recall", "scene number must be 0..=99"));
        }
        Ok(self.inner.core.recall(bank, n).await?)
    }
}

impl DeviceAdapter for TfAdapter {
    fn info(&self) -> DeviceInfo {
        self.inner.core.info()
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        let core = &self.inner.core;
        core.refresh(true).await?;
        Ok(core.snapshot())
    }

    /// The mirror: every value came from the console (initial read,
    /// `NOTIFY` lines, write read-backs, post-recall re-reads).
    async fn current(&self) -> Result<DeviceSnapshot, DeviceError> {
        Ok(self.inner.core.snapshot())
    }

    async fn apply_param(
        &self,
        path: &str,
        value: ParamValue,
        guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        let core = &self.inner.core;
        let def = core
            .table
            .by_path(path)
            .ok_or_else(|| DeviceError::UnknownParam(path.to_owned()))?;
        if !def.writable {
            return Err(DeviceError::ReadOnly(path.to_owned()));
        }
        if def.disruptive && guard != WriteGuard::AllowDisruptive {
            return Err(DeviceError::DisruptiveWrite(path.to_owned()));
        }
        match (&def.target, def.kind) {
            (Target::Scene(SceneField::Recall), _) => {
                let ParamValue::Text(s) = &value else {
                    return Err(invalid(path, "expected text like `A05` or `B22`"));
                };
                let (bank, n) = parse_scene(s).map_err(|e| invalid(path, e.to_string()))?;
                self.recall_scene(bank, n, guard).await
            }
            (Target::Scene(_), _) | (_, ValueKind::Opaque) => {
                Err(DeviceError::ReadOnly(path.to_owned()))
            }
            (Target::Rcp { address, x, y }, _) => {
                core.write_rcp(def, address, *x, *y, &value).await
            }
        }
    }

    async fn set_route(
        &self,
        _output: ChannelRef,
        _source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported(NO_ROUTING.to_owned()))
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.inner.core.events.subscribe()
    }
}
