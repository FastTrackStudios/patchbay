//! `PatchbayService` implementation over the engine + preset store.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc;

use parking_lot::{Mutex, RwLock};
use patchbay_proto::services::patchbay_service::{
    PatchbayServiceStreamSource, patchbay_service_stream_service_descriptor, stream_serve,
};
use patchbay_proto::{
    AliasEntry, AppStream, ApplyReport, CanvasView, ClockDefaults, ClockInfo, ColorEntry,
    DanteDevice, DanteDeviceConfig, DanteStatus, GraphEvent, GraphSnapshot, IconEntry, LatencyRule,
    MeterLevel, NamedRoute, PatchbayError, PatchbayService, PresetLink, RoutingPreset,
    ServiceAction, ServiceStatus, VirtualSink, patchbay_service_service_descriptor,
    serve_patchbay_service,
};

use crate::engine::{self, Command, EngineHandle};
use crate::meters::Taps;
use crate::plan;
use crate::presets::PresetStore;
use crate::settle::{Burst, Settle};
use crate::store::GraphStore;
use crate::telemetry as tel;

/// How often the `pw-dump` poller samples live node state.
const STATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// How often the event pump wakes to check whether the graph has gone
/// quiet. Bounds settle latency; it is not itself a settle delay.
const SETTLE_TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// The headless patchbay backend: `PipeWire` engine thread, graph
/// mirror, presets/aliases, and the RPC surface. Cheap to clone; all
/// state is shared behind the `Arc`.
#[derive(Clone)]
pub struct PatchbayBackend {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<RwLock<GraphStore>>,
    engine: EngineHandle,
    events_hub: architect::PubSub<GraphEvent>,
    presets: Arc<PresetStore>,
    dante: crate::dante_net::DanteEndpoints,
    icons: crate::icons::IconCache,
    /// Live meter taps. Empty until a client asks for metering — each
    /// tap is a `parec` child, so nothing runs speculatively.
    meters: Mutex<Taps>,
}

impl Default for PatchbayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl PatchbayBackend {
    /// Spawn the `PipeWire` thread and the event pump. The replay window
    /// covers the snapshot→subscribe gap: clients fetch `graph()` first,
    /// then apply (idempotent) events from the recent past.
    ///
    /// Long by nature: this is the backend's whole wiring diagram, and
    /// each block's `store`/`presets`/`engine` clones only make sense
    /// next to the thread they feed.
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Self {
        let store = Arc::new(RwLock::new(GraphStore::default()));
        let presets = Arc::new(PresetStore::open());
        let (events_tx, events_rx) = mpsc::channel::<GraphEvent>();
        let poll_tx = events_tx.clone();
        let enrich_tx = events_tx.clone();
        let engine = engine::spawn(store.clone(), events_tx);

        // Live node-state poller: pw-dump every couple of seconds and
        // emit NodeStateChanged deltas. This is the free "is anything
        // going through here" signal (running/idle/suspended) —
        // `PipeWire` has no per-port level API, so activity state is the
        // honest, no-tap answer. Read-only shell-out; quiet graphs emit
        // nothing.
        {
            let store = store.clone();
            let spawned = std::thread::Builder::new()
                .name("patchbay-states".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(STATE_POLL_INTERVAL);
                        crate::enrich::poll_node_states(&store, &poll_tx);
                    }
                });
            if let Err(e) = spawned {
                // Degraded, not fatal: without this poller nodes just
                // never report running/idle. The graph still works.
                tracing::error!("could not spawn patchbay-states thread: {e}");
            }
        }

        // Big window: a single app connecting is a burst of hundreds of
        // port events, a `PipeWire` reconnect is ~2000 — a small ring
        // drops the middle of the burst and clients silently lose nodes
        // (REAPER "not showing up" was exactly this).
        let events_hub = architect::PubSub::sliding(16_384);
        let pump = events_hub.clone();
        {
            let store = store.clone();
            let presets = presets.clone();
            let engine = engine.clone();
            let spawned = std::thread::Builder::new()
                .name("patchbay-events".into())
                .spawn(move || {
                    event_pump(&store, &presets, &engine, &events_rx, &pump, &enrich_tx);
                });
            if let Err(e) = spawned {
                // This one IS fatal in practice — with no pump the graph
                // mirror never reaches any client — but a panic here
                // would take out the host app, so surface it and let the
                // caller see an empty graph instead.
                tracing::error!("could not spawn patchbay-events thread: {e}");
            }
        }

        Self {
            inner: Arc::new(Inner {
                store,
                engine,
                events_hub,
                presets,
                dante: crate::dante_net::DanteEndpoints::default(),
                icons: crate::icons::IconCache::default(),
                meters: Mutex::new(Taps::default()),
            }),
        }
    }

    /// A fresh `LayerRouter` serving this backend — the RPC layer plus
    /// its `#[subscribe]` stream sibling over the same impl.
    #[must_use]
    pub fn router(&self) -> architect::LayerRouter {
        architect::LayerRouter::new()
            .with(
                patchbay_service_service_descriptor(),
                serve_patchbay_service(self.clone()),
            )
            .with(
                patchbay_service_stream_service_descriptor(),
                stream_serve(self.clone()),
            )
    }

    /// Resolve + send one create-link command. Returns whether a
    /// command was actually sent (false = link already exists).
    fn create_link_inner(&self, output_port: u32, input_port: u32) -> Result<bool, PatchbayError> {
        let (output_node, input_node) = {
            let store = self.inner.store.read();
            if store.link_between(output_port, input_port).is_some() {
                return Ok(false);
            }
            let out_node = store
                .node_of_port(output_port)
                .ok_or_else(|| PatchbayError::not_found("output port", &output_port))?
                .id;
            let in_node = store
                .node_of_port(input_port)
                .ok_or_else(|| PatchbayError::not_found("input port", &input_port))?
                .id;
            (out_node, in_node)
        };
        self.inner
            .engine
            .send(Command::CreateLink {
                output_node,
                output_port,
                input_node,
                input_port,
            })
            .map_err(PatchbayError::EngineUnavailable)?;
        Ok(true)
    }

    /// Current links as name-keyed [`PresetLink`]s (links with
    /// unresolvable endpoints are skipped).
    fn links_by_names(&self) -> Vec<PresetLink> {
        let store = self.inner.store.read();
        let mut out: Vec<PresetLink> = store
            .links
            .values()
            .filter_map(|l| {
                let on = store.nodes.get(&l.output_node)?;
                let op = store.ports.get(&l.output_port)?;
                let inn = store.nodes.get(&l.input_node)?;
                let ip = store.ports.get(&l.input_port)?;
                Some(PresetLink {
                    output_node: on.name.clone(),
                    output_port: op.name.clone(),
                    input_node: inn.name.clone(),
                    input_port: ip.name.clone(),
                })
            })
            .collect();
        out.sort_by(|a, b| {
            (&a.output_node, &a.output_port, &a.input_node, &a.input_port).cmp(&(
                &b.output_node,
                &b.output_port,
                &b.input_node,
                &b.input_port,
            ))
        });
        out.dedup();
        out
    }
}

impl PatchbayBackend {
    /// Run a blocking closure (preset-store mutations do synchronous
    /// file I/O) off the async executor.
    async fn blocking<T, F>(&self, f: F) -> Result<T, PatchbayError>
    where
        F: FnOnce(Self) -> T + Send + 'static,
        T: Send + 'static,
    {
        let this = self.clone();
        tokio::task::spawn_blocking(move || f(this))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    /// Rewrite the `WirePlumber` drop-in from `rules` (blocking I/O +
    /// a pw-metadata read for the graph rate → off the executor).
    async fn write_latency_dropin(&self, rules: Vec<LatencyRule>) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || {
            let rate = crate::clock::clock_info().rate;
            crate::latency::write_dropin(&rules, rate)
        })
        .await
        .map_err(|e| PatchbayError::Internal(e.to_string()))?
        .map_err(PatchbayError::Internal)
    }
}

// ── Engine command execution ────────────────────────────────────────

/// Hand a plan to the engine, counting what actually made it through.
///
/// The engine refuses commands while it is down or reconnecting; that is
/// normal (the next settle retries), so it rides the wide event rather
/// than a log line per command.
fn execute(engine: &EngineHandle, commands: Vec<Command>) -> (u32, u32) {
    let (mut sent, mut failed) = (0_u32, 0_u32);
    for cmd in commands {
        match engine.send(cmd) {
            Ok(()) => sent = sent.saturating_add(1),
            Err(_) => failed = failed.saturating_add(1),
        }
    }
    tel::set(tel::COMMANDS_SENT, i64::from(sent));
    if failed > 0 {
        tel::set(tel::COMMANDS_FAILED, i64::from(failed));
    }
    (sent, failed)
}

/// `target → alias` for planning.
fn alias_map(presets: &PresetStore) -> HashMap<String, String> {
    presets
        .aliases()
        .into_iter()
        .map(|a| (a.target, a.alias))
        .collect()
}

// ── Settle actions ──────────────────────────────────────────────────

/// Create any persisted virtual sink that isn't in the live graph.
/// Returns how many creates were issued.
fn ensure_virtual_sinks(
    store: &RwLock<GraphStore>,
    presets: &PresetStore,
    engine: &EngineHandle,
) -> u32 {
    let configured = presets.virtual_sinks();
    let commands = plan::sinks::plan(&store.read(), &configured);
    tel::set(
        tel::SINKS_CONFIGURED,
        i64::try_from(configured.len()).unwrap_or(i64::MAX),
    );
    let created = if commands.is_empty() {
        0
    } else {
        tel::set(
            tel::SINKS_CREATED,
            i64::try_from(commands.len()).unwrap_or(i64::MAX),
        );
        let (sent, _) = execute(engine, commands);
        sent
    };

    // Companion capture sources, so a bus shows up in OBS by name. Runs
    // after the sinks so the monitor it masters from exists; idempotent,
    // and a no-op when nothing is marked capturable.
    let exposed = crate::capture::ensure(&configured);
    if exposed > 0 {
        tel::set(tel::CAPTURE_SOURCES_CREATED, i64::from(exposed));
    }
    created
}

/// Apply every enabled named route against the live graph. Idempotent —
/// never destroys anything, never duplicates an existing link. Returns
/// the number of links created.
fn apply_named_routes(
    store: &RwLock<GraphStore>,
    presets: &PresetStore,
    engine: &EngineHandle,
) -> u32 {
    let routes = presets.routes();
    if routes.is_empty() {
        return 0;
    }
    let aliases = alias_map(presets);
    let planned = plan::routes::plan(&store.read(), &routes, &aliases);

    tel::set(
        tel::ROUTES_CONSIDERED,
        i64::try_from(routes.iter().filter(|r| r.enabled).count()).unwrap_or(i64::MAX),
    );
    tel::set(tel::ROUTES_RESOLVED, i64::from(planned.resolved));
    if !planned.unresolved.is_empty() {
        // The "why is my rig not wired" field. Bounded by the user's own
        // config, so safe to carry in full.
        tel::set_display(tel::ROUTES_UNRESOLVED, planned.unresolved.join(", "));
    }
    if planned.commands.is_empty() {
        return 0;
    }
    let (sent, _) = execute(engine, planned.commands);
    tel::set(tel::ROUTES_LINKS_CREATED, i64::from(sent));
    sent
}

/// Non-destructive chanmap import: name `node`'s channels from the
/// host's default `ReaperChanMap`, skipping channels the user already
/// aliased. A missing chanmap file is silently nothing.
fn auto_import_chanmap(store: &RwLock<GraphStore>, presets: &PresetStore, node: &str) -> u32 {
    let Ok(names) = crate::chanmap::read_names("") else {
        return 0;
    };
    let ports: Vec<String> = {
        let s = store.read();
        let Some(n) = s.nodes.values().find(|n| n.name == node) else {
            return 0;
        };
        s.ports
            .values()
            .filter(|p| p.node_id == n.id)
            .map(|p| p.name.clone())
            .collect()
    };
    let batch: Vec<(String, String)> = ports
        .into_iter()
        .filter_map(|port| {
            let channel = patchbay_proto::channel_of_port(&port)?;
            let target = format!("{node}:{port}");
            // Non-destructive: never overwrite a name the user set.
            if presets.has_alias(&target) {
                return None;
            }
            Some((target, names.get(&channel)?.clone()))
        })
        .collect();
    let written = u32::try_from(batch.len()).unwrap_or(u32::MAX);
    if written > 0 {
        presets.set_aliases(batch);
        tel::set(tel::NODE_NAME, node.to_owned());
        tel::set(tel::ALIASES_WRITTEN, i64::from(written));
    }
    written
}

// ── The event pump ──────────────────────────────────────────────────

/// Nodes whose chanmap we auto-import the first time they appear.
const CHANMAP_AUTO_IMPORT: &[&str] = &["REAPER"];

/// Drain the engine's event channel: mirror events out to subscribers,
/// and run the settle actions once the graph goes quiet.
///
/// One thread, one loop. Settling used to be six `thread::sleep` calls
/// on spawned threads, each guessing how long a burst would take; a
/// guess that is too short applies routes against a half-built graph and
/// never retries. Here the pump watches the stream it is already
/// draining and acts when it actually stops.
fn event_pump(
    store: &Arc<RwLock<GraphStore>>,
    presets: &Arc<PresetStore>,
    engine: &EngineHandle,
    events_rx: &mpsc::Receiver<GraphEvent>,
    pump: &architect::PubSub<GraphEvent>,
    enrich_tx: &mpsc::Sender<GraphEvent>,
) {
    let mut settle = Settle::default();
    // Nodes we've already auto-imported this connection; cleared on
    // Reset so a `PipeWire` restart re-imports.
    let mut imported: HashSet<String> = HashSet::new();
    // A Reset means the graph is being rebuilt from scratch, so the
    // next settle must re-seed sinks as well as routes.
    let mut reconnected = false;

    loop {
        match events_rx.recv_timeout(SETTLE_TICK) {
            Ok(ev) => {
                if matches!(ev, GraphEvent::Reset) {
                    imported.clear();
                    reconnected = true;
                }
                // Only structural changes can make a route resolvable;
                // a state flip on an existing node cannot.
                if matches!(
                    ev,
                    GraphEvent::Reset
                        | GraphEvent::NodeAdded(_)
                        | GraphEvent::PortAdded(_)
                        | GraphEvent::NodeRemoved { .. }
                        | GraphEvent::PortRemoved { .. }
                ) {
                    settle.observe(std::time::Instant::now());
                }
                pump.publish(ev);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if let Some(burst) = settle.take_settled(std::time::Instant::now()) {
            on_settled(
                store,
                presets,
                engine,
                enrich_tx,
                burst,
                std::mem::take(&mut reconnected),
                &mut imported,
            );
        }
    }
    tracing::warn!("patchbay event pump ended (engine gone)");
}

/// The graph went quiet: enrich, seed sinks, apply routes, import names.
///
/// One span per settle carrying the whole outcome — what the burst
/// looked like, what got created, and which routes did not resolve. That
/// is the wide event for background work; the individual steps do not
/// log.
fn on_settled(
    store: &Arc<RwLock<GraphStore>>,
    presets: &Arc<PresetStore>,
    engine: &EngineHandle,
    enrich_tx: &mpsc::Sender<GraphEvent>,
    burst: Burst,
    reconnected: bool,
    imported: &mut HashSet<String>,
) {
    let span = tracing::info_span!("patchbay.settle", otel.name = "patchbay/settle");
    let _guard = span.enter();

    tel::set(
        tel::TRIGGER,
        if reconnected { "reconnect" } else { "settle" },
    );
    tel::set(tel::SETTLE_EVENTS, i64::from(burst.events));
    tel::set(
        tel::SETTLE_BURST_MS,
        i64::try_from(burst.duration.as_millis()).unwrap_or(i64::MAX),
    );

    // Full node props the registry subset lacks (node.group,
    // application.*). Shells out to pw-dump, so it runs off the pump.
    {
        let store = store.clone();
        let tx = enrich_tx.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("patchbay-enrich".into())
            .spawn(move || crate::enrich::enrich_nodes(&store, &tx))
        {
            tracing::warn!("could not spawn enrichment: {e}");
        }
    }

    let sinks = ensure_virtual_sinks(store, presets, engine);
    let links = apply_named_routes(store, presets, engine);

    let mut aliases = 0_u32;
    for name in CHANMAP_AUTO_IMPORT {
        let present = store.read().nodes.values().any(|n| n.name == *name);
        if present && imported.insert((*name).to_owned()) {
            aliases = aliases.saturating_add(auto_import_chanmap(store, presets, name));
        }
    }

    {
        let g = store.read();
        tel::set(
            tel::GRAPH_NODES,
            i64::try_from(g.nodes.len()).unwrap_or(i64::MAX),
        );
        tel::set(
            tel::GRAPH_PORTS,
            i64::try_from(g.ports.len()).unwrap_or(i64::MAX),
        );
        tel::set(
            tel::GRAPH_LINKS,
            i64::try_from(g.links.len()).unwrap_or(i64::MAX),
        );
    }

    // One line, only when the settle actually changed something. A
    // quiet settle rides the span alone.
    if sinks > 0 || links > 0 || aliases > 0 {
        tracing::info!(
            sinks_created = sinks,
            links_created = links,
            aliases_written = aliases,
            "graph settled"
        );
    }
}

impl PatchbayServiceStreamSource for PatchbayBackend {
    fn graph_events_hub(&self) -> &architect::PubSub<GraphEvent> {
        &self.inner.events_hub
    }
}

impl PatchbayService for PatchbayBackend {
    async fn graph(&self) -> Result<GraphSnapshot, PatchbayError> {
        let snap = self.inner.store.read().snapshot();
        tel::set(
            tel::GRAPH_NODES,
            i64::try_from(snap.nodes.len()).unwrap_or(i64::MAX),
        );
        tel::set(
            tel::GRAPH_PORTS,
            i64::try_from(snap.ports.len()).unwrap_or(i64::MAX),
        );
        tel::set(
            tel::GRAPH_LINKS,
            i64::try_from(snap.links.len()).unwrap_or(i64::MAX),
        );
        Ok(snap)
    }

    async fn create_link(&self, output_port: u32, input_port: u32) -> Result<(), PatchbayError> {
        tel::set(tel::LINK_OUTPUT_PORT, i64::from(output_port));
        tel::set(tel::LINK_INPUT_PORT, i64::from(input_port));
        let sent = self.create_link_inner(output_port, input_port)?;
        tel::set(tel::LINK_ALREADY_PRESENT, !sent);
        Ok(())
    }

    async fn destroy_link(&self, link_id: u32) -> Result<(), PatchbayError> {
        if !self.inner.store.read().links.contains_key(&link_id) {
            return Err(PatchbayError::not_found("link", &link_id));
        }
        self.inner
            .engine
            .send(Command::DestroyLink { id: link_id })
            .map_err(PatchbayError::EngineUnavailable)
    }

    async fn connect_one_to_one(
        &self,
        output_node: String,
        input_node: String,
    ) -> Result<u32, PatchbayError> {
        // Pair by numeric suffix (out7 → playback_7). Non-numeric ports
        // don't participate; MIDI ports never win a channel.
        let pairs: Vec<(u32, u32)> = {
            let store = self.inner.store.read();
            let node_id = |name: &str| {
                store
                    .nodes
                    .values()
                    .find(|n| n.name == name)
                    .map(|n| n.id)
                    .ok_or_else(|| PatchbayError::not_found("node", &name))
            };
            let out_id = node_id(&output_node)?;
            let in_id = node_id(&input_node)?;
            plan::pairing::pair_nodes(&store, out_id, in_id)
        };
        if pairs.is_empty() {
            return Err(PatchbayError::Internal(format!(
                "no numeric-suffix port pairs between {output_node} and {input_node}"
            )));
        }
        let mut created = 0u32;
        for (out, inp) in pairs {
            if self.create_link_inner(out, inp)? {
                created = created.saturating_add(1);
            }
        }
        tel::set(tel::ROUTES_LINKS_CREATED, i64::from(created));
        Ok(created)
    }

    async fn disconnect_nodes(
        &self,
        output_node: String,
        input_node: String,
    ) -> Result<u32, PatchbayError> {
        let link_ids: Vec<u32> = {
            let store = self.inner.store.read();
            let node_id = |name: &str| store.nodes.values().find(|n| n.name == name).map(|n| n.id);
            let (Some(out_id), Some(in_id)) = (node_id(&output_node), node_id(&input_node)) else {
                return Err(PatchbayError::not_found(
                    "node",
                    &format!("{output_node} or {input_node}"),
                ));
            };
            store
                .links
                .values()
                .filter(|l| l.output_node == out_id && l.input_node == in_id)
                .map(|l| l.id)
                .collect()
        };
        for id in &link_ids {
            self.inner
                .engine
                .send(Command::DestroyLink { id: *id })
                .map_err(PatchbayError::EngineUnavailable)?;
        }
        Ok(u32::try_from(link_ids.len()).unwrap_or(u32::MAX))
    }

    async fn list_presets(&self) -> Result<Vec<RoutingPreset>, PatchbayError> {
        Ok(self.inner.presets.presets())
    }

    async fn save_preset(
        &self,
        name: String,
        description: String,
    ) -> Result<RoutingPreset, PatchbayError> {
        if name.trim().is_empty() {
            return Err(PatchbayError::Internal("preset name is empty".into()));
        }
        let links = self.links_by_names();
        self.blocking(move |this| this.inner.presets.upsert_preset(name, description, links))
            .await
    }

    async fn apply_preset(
        &self,
        name: String,
        exclusive: bool,
    ) -> Result<ApplyReport, PatchbayError> {
        let preset = self
            .inner
            .presets
            .preset(&name)
            .ok_or_else(|| PatchbayError::not_found("preset", &name))?;

        // Plan against one consistent snapshot of the graph, then
        // execute. Exclusive mode tears links DOWN, so deciding what to
        // destroy must not race a half-applied create.
        let planned = {
            let store = self.inner.store.read();
            plan::presets::plan(&store, &preset, exclusive)
        };

        tel::set(tel::PRESET_NAME, name);
        tel::set(tel::PRESET_EXCLUSIVE, exclusive);
        tel::set(tel::PRESET_CREATED, i64::from(planned.report.created));
        tel::set(tel::PRESET_EXISTING, i64::from(planned.report.existing));
        tel::set(tel::PRESET_DESTROYED, i64::from(planned.report.destroyed));
        tel::set(
            tel::PRESET_MISSING,
            i64::try_from(planned.report.missing.len()).unwrap_or(i64::MAX),
        );

        let (_, failed) = execute(&self.inner.engine, planned.commands);
        if failed > 0 {
            return Err(PatchbayError::EngineUnavailable(format!(
                "{failed} of the preset's commands were refused (engine reconnecting)"
            )));
        }
        Ok(planned.report)
    }

    async fn delete_preset(&self, name: String) -> Result<(), PatchbayError> {
        self.inner
            .presets
            .delete_preset(&name)
            .then_some(())
            .ok_or_else(|| PatchbayError::not_found("preset", &name))
    }

    async fn routes(&self) -> Result<Vec<NamedRoute>, PatchbayError> {
        Ok(self.inner.presets.routes())
    }

    async fn set_route(&self, route: NamedRoute) -> Result<(), PatchbayError> {
        if route.name.trim().is_empty() {
            return Err(PatchbayError::Internal("route name is empty".into()));
        }
        // Persisting writes the config file and the apply walks the
        // graph under a lock — both blocking, so off the executor.
        self.blocking(move |this| {
            this.inner.presets.set_route(route);
            // Apply immediately so a just-added route wires up now if
            // both ends are already present.
            apply_named_routes(&this.inner.store, &this.inner.presets, &this.inner.engine);
        })
        .await
    }

    async fn delete_route(&self, name: String) -> Result<(), PatchbayError> {
        self.inner
            .presets
            .delete_route(&name)
            .then_some(())
            .ok_or_else(|| PatchbayError::not_found("route", &name))
    }

    async fn apply_routes(&self) -> Result<u32, PatchbayError> {
        tel::set(tel::TRIGGER, "rpc");
        Ok(apply_named_routes(
            &self.inner.store,
            &self.inner.presets,
            &self.inner.engine,
        ))
    }

    async fn aliases(&self) -> Result<Vec<AliasEntry>, PatchbayError> {
        Ok(self.inner.presets.aliases())
    }

    async fn set_alias(&self, target: String, alias: String) -> Result<(), PatchbayError> {
        self.blocking(move |this| this.inner.presets.set_alias(target, alias))
            .await
    }

    async fn app_streams(&self) -> Result<Vec<AppStream>, PatchbayError> {
        // Shells out to pactl — off the executor.
        let streams = self.blocking(|_| crate::streams::list()).await??;
        tel::set(
            tel::STREAMS_LISTED,
            i64::try_from(streams.len()).unwrap_or(i64::MAX),
        );
        Ok(streams)
    }

    async fn move_app_stream(&self, index: u32, sink: String) -> Result<(), PatchbayError> {
        tel::set(tel::STREAM_INDEX, i64::from(index));
        tel::set(tel::STREAM_TARGET_SINK, sink.clone());
        self.blocking(move |_| crate::streams::move_to_sink(index, &sink))
            .await?
    }

    async fn set_metered(&self, nodes: Vec<String>) -> Result<u32, PatchbayError> {
        // Resolve each name to the source that actually carries its
        // audio (a sink is tapped through its monitor) while we hold the
        // graph, then reconcile off the executor — spawning children is
        // blocking work.
        let desired: HashMap<String, String> = {
            let store = self.inner.store.read();
            nodes
                .iter()
                .filter_map(|name| {
                    let node = store.nodes.values().find(|n| n.name == *name)?;
                    Some((
                        name.clone(),
                        crate::meters::source_for(&node.name, &node.media_class),
                    ))
                })
                .collect()
        };
        tel::set(
            tel::METERS_REQUESTED,
            i64::try_from(nodes.len()).unwrap_or(i64::MAX),
        );
        let live = self
            .blocking(move |this| {
                let mut taps = this.inner.meters.lock();
                taps.reconcile(&desired);
                taps.len()
            })
            .await?;
        tel::set(tel::METERS_ACTIVE, i64::try_from(live).unwrap_or(i64::MAX));
        Ok(u32::try_from(live).unwrap_or(u32::MAX))
    }

    async fn meters(&self) -> Result<Vec<MeterLevel>, PatchbayError> {
        Ok(self.inner.meters.lock().levels())
    }

    async fn set_aliases(&self, entries: Vec<AliasEntry>) -> Result<u32, PatchbayError> {
        let n = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        self.blocking(move |this| {
            this.inner
                .presets
                .set_aliases(entries.into_iter().map(|e| (e.target, e.alias)));
        })
        .await?;
        Ok(n)
    }

    async fn import_chanmap(&self, node: String, path: String) -> Result<u32, PatchbayError> {
        let names = crate::chanmap::read_names(&path).map_err(PatchbayError::Internal)?;
        // Every port of `node` whose numeric suffix is a named channel
        // gets the alias (playback + monitor + capture all match, so
        // the name shows on whichever side you're patching).
        let ports: Vec<String> = {
            let store = self.inner.store.read();
            let Some(n) = store.nodes.values().find(|n| n.name == node) else {
                return Err(PatchbayError::not_found("node", &node));
            };
            store
                .ports
                .values()
                .filter(|p| p.node_id == n.id)
                .map(|p| p.name.clone())
                .collect()
        };
        let batch: Vec<(String, String)> = ports
            .into_iter()
            .filter_map(|port| {
                let channel = crate::chanmap::channel_of_port(&port)?;
                Some((format!("{node}:{port}"), names.get(&channel)?.clone()))
            })
            .collect();
        let written = u32::try_from(batch.len()).unwrap_or(u32::MAX);
        self.inner.presets.set_aliases(batch);
        Ok(written)
    }

    async fn export_chanmap(&self, node: String, path: String) -> Result<u32, PatchbayError> {
        // channel → alias, from this node's aliased ports. Multiple
        // ports can share a channel (playback_5 + monitor_5) — first
        // alias wins, they're the same name after an import anyway.
        let prefix = format!("{node}:");
        let mut names = std::collections::BTreeMap::new();
        for entry in self.inner.presets.aliases() {
            let Some(port) = entry.target.strip_prefix(&prefix) else {
                continue;
            };
            let Some(channel) = crate::chanmap::channel_of_port(port) else {
                continue;
            };
            names.entry(channel).or_insert(entry.alias);
        }
        if names.is_empty() {
            return Err(PatchbayError::not_found("port aliases on node", &node));
        }
        crate::chanmap::write_names(&path, &names).map_err(PatchbayError::Internal)?;
        Ok(u32::try_from(names.len()).unwrap_or(u32::MAX))
    }

    async fn import_inferno_names(
        &self,
        node: String,
        device: String,
        direction: String,
    ) -> Result<u32, PatchbayError> {
        let want_rx = match direction.trim().to_lowercase().as_str() {
            "rx" | "in" | "input" | "capture" => true,
            "tx" | "out" | "output" | "playback" => false,
            other => {
                return Err(PatchbayError::Internal(format!(
                    "direction must be 'rx' or 'tx', got '{other}'"
                )));
            }
        };
        // Live ARC scan (mDNS + per-device channel query — seconds).
        let devices = self.inner.dante.network().await?;
        let dev = if device.trim().is_empty() {
            devices.first()
        } else {
            devices.iter().find(|d| d.name == device)
        }
        .ok_or_else(|| {
            PatchbayError::not_found(
                "dante device",
                &if device.trim().is_empty() {
                    "<any>"
                } else {
                    device.as_str()
                },
            )
        })?;
        // channel number → name, from the chosen direction's channel list.
        let names: std::collections::BTreeMap<u32, String> =
            if want_rx { &dev.rx } else { &dev.tx }
                .iter()
                .filter(|c| !c.name.trim().is_empty())
                .map(|c| (c.number, c.name.clone()))
                .collect();
        // Match ports on `node` by numeric suffix (same as import_chanmap).
        let ports: Vec<String> = {
            let store = self.inner.store.read();
            let Some(n) = store.nodes.values().find(|n| n.name == node) else {
                return Err(PatchbayError::not_found("node", &node));
            };
            store
                .ports
                .values()
                .filter(|p| p.node_id == n.id)
                .map(|p| p.name.clone())
                .collect()
        };
        let batch: Vec<(String, String)> = ports
            .into_iter()
            .filter_map(|port| {
                let channel = crate::chanmap::channel_of_port(&port)?;
                Some((format!("{node}:{port}"), names.get(&channel)?.clone()))
            })
            .collect();
        let written = u32::try_from(batch.len()).unwrap_or(u32::MAX);
        self.inner.presets.set_aliases(batch);
        Ok(written)
    }

    async fn virtual_sinks(&self) -> Result<Vec<VirtualSink>, PatchbayError> {
        Ok(self.inner.presets.virtual_sinks())
    }

    async fn add_virtual_sink(&self, sink: VirtualSink) -> Result<(), PatchbayError> {
        if sink.name.trim().is_empty() {
            return Err(PatchbayError::Internal("sink name is empty".into()));
        }
        if !(1..=64).contains(&sink.channels) {
            return Err(PatchbayError::Internal(format!(
                "channel count {} out of range (1–64)",
                sink.channels
            )));
        }
        tel::set(tel::SINK_NAME, sink.name.clone());
        tel::set(tel::SINK_CHANNELS, i64::from(sink.channels));
        self.blocking(move |this| {
            this.inner.presets.add_virtual_sink(sink);
            ensure_virtual_sinks(&this.inner.store, &this.inner.presets, &this.inner.engine);
        })
        .await
    }

    async fn remove_virtual_sink(&self, name: String) -> Result<(), PatchbayError> {
        if !self.inner.presets.remove_virtual_sink(&name) {
            return Err(PatchbayError::not_found("virtual sink", &name));
        }
        // Tear down the companion capture source first, so OBS stops
        // listing a device whose bus is about to vanish.
        {
            let name = name.clone();
            self.blocking(move |_| crate::capture::remove(&name))
                .await?;
        }
        // Destroy the live node too — but ONLY if it carries the
        // patchbay.virtual tag (never an arbitrary node).
        let node_name = patchbay_proto::sink_node_name(&name);
        let live_id = self
            .inner
            .store
            .read()
            .nodes
            .values()
            .find(|n| n.name == node_name && n.virtual_sink)
            .map(|n| n.id);
        if let Some(id) = live_id {
            self.inner
                .engine
                .send(Command::DestroyNode { id })
                .map_err(PatchbayError::EngineUnavailable)?;
        }
        Ok(())
    }

    async fn views(&self) -> Result<Vec<CanvasView>, PatchbayError> {
        Ok(self.inner.presets.views())
    }

    async fn save_view(&self, view: CanvasView) -> Result<(), PatchbayError> {
        if view.name.trim().is_empty() {
            return Err(PatchbayError::Internal("view name is empty".into()));
        }
        self.blocking(move |this| this.inner.presets.save_view(view))
            .await
    }

    async fn delete_view(&self, name: String) -> Result<(), PatchbayError> {
        self.inner
            .presets
            .delete_view(&name)
            .then_some(())
            .ok_or_else(|| PatchbayError::not_found("view", &name))
    }

    async fn colors(&self) -> Result<Vec<ColorEntry>, PatchbayError> {
        Ok(self.inner.presets.colors())
    }

    async fn set_color(&self, target: String, color: String) -> Result<(), PatchbayError> {
        self.blocking(move |this| this.inner.presets.set_color(target, color))
            .await
    }

    async fn icons(&self, names: Vec<String>) -> Result<Vec<IconEntry>, PatchbayError> {
        // Disk lookups + reads — off the executor.
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            names
                .into_iter()
                .filter_map(|name| {
                    this.inner.icons.data_uri(&name).map(|data_uri| IconEntry {
                        icon_name: name,
                        data_uri,
                    })
                })
                .collect()
        })
        .await
        .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    async fn clock(&self) -> Result<ClockInfo, PatchbayError> {
        // Shells out — keep it off the async executor.
        tokio::task::spawn_blocking(crate::clock::clock_info)
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    async fn force_quantum(&self, frames: u32) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || crate::clock::force_quantum(frames))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    async fn clock_defaults(&self) -> Result<ClockDefaults, PatchbayError> {
        tokio::task::spawn_blocking(crate::clock::clock_defaults)
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    async fn set_clock_defaults(&self, defaults: ClockDefaults) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || crate::clock::set_clock_defaults(defaults))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
            .map_err(PatchbayError::Internal)
    }

    async fn latency_rules(&self) -> Result<Vec<LatencyRule>, PatchbayError> {
        Ok(self.inner.presets.latency_rules())
    }

    async fn set_latency_rule(&self, rule: LatencyRule) -> Result<(), PatchbayError> {
        if !(16..=8192).contains(&rule.quantum) {
            return Err(PatchbayError::Internal(format!(
                "quantum {} out of range",
                rule.quantum
            )));
        }
        let rules = self.inner.presets.set_latency_rule(rule);
        self.write_latency_dropin(rules).await
    }

    async fn remove_latency_rule(&self, pattern: String) -> Result<(), PatchbayError> {
        let rules = self
            .inner
            .presets
            .remove_latency_rule(&pattern)
            .ok_or_else(|| PatchbayError::not_found("latency rule", &pattern))?;
        self.write_latency_dropin(rules).await
    }

    async fn dante_status(&self) -> Result<DanteStatus, PatchbayError> {
        tokio::task::spawn_blocking(crate::dante::status)
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    async fn set_dante(&self, on: bool) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || crate::dante::set(on))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
            .map_err(PatchbayError::Internal)
    }

    async fn services(&self) -> Result<Vec<ServiceStatus>, PatchbayError> {
        tokio::task::spawn_blocking(crate::units::status_all)
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    async fn service_action(
        &self,
        unit: String,
        action: ServiceAction,
    ) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || crate::units::action(&unit, action))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
    }

    async fn dante_network(&self) -> Result<Vec<DanteDevice>, PatchbayError> {
        self.inner.dante.network().await
    }

    async fn dante_subscribe(
        &self,
        rx_device: String,
        rx_channel: u32,
        tx_device: String,
        tx_channel: String,
    ) -> Result<(), PatchbayError> {
        self.inner
            .dante
            .subscribe(&rx_device, rx_channel, &tx_device, &tx_channel)
            .await
    }

    async fn dante_unsubscribe(
        &self,
        rx_device: String,
        rx_channel: u32,
    ) -> Result<(), PatchbayError> {
        self.inner.dante.unsubscribe(&rx_device, rx_channel).await
    }

    async fn dante_config(&self) -> Result<Vec<DanteDeviceConfig>, PatchbayError> {
        Ok(self.inner.presets.dante_config())
    }

    async fn save_dante_config(&self) -> Result<u32, PatchbayError> {
        // Live ARC scan (mDNS + per-device channel query — seconds).
        let devices = self.inner.dante.network().await?;
        // Only persist devices we actually read (an unreachable device
        // reports empty channels; saving those would wipe good names).
        let cfg: Vec<DanteDeviceConfig> = devices
            .iter()
            .filter(|d| !d.unreachable)
            .map(DanteDeviceConfig::from_device)
            .collect();
        let n = u32::try_from(cfg.len()).unwrap_or(u32::MAX);
        self.inner.presets.set_dante_config(cfg);
        Ok(n)
    }

    async fn apply_dante_config(&self) -> Result<u32, PatchbayError> {
        let saved = self.inner.presets.dante_config();
        if saved.is_empty() {
            return Ok(0);
        }
        // Scan the live network so we only (re)write subscriptions that
        // actually differ — no needless ARC writes to the console.
        let live = self.inner.dante.network().await.unwrap_or_default();
        let mut current: HashMap<(String, u32), (String, String)> = HashMap::new();
        for d in &live {
            for s in &d.subscriptions {
                current.insert(
                    (d.name.clone(), s.rx_channel),
                    (s.tx_device.clone(), s.tx_channel.clone()),
                );
            }
        }
        let mut applied = 0u32;
        for dev in &saved {
            for s in &dev.subscriptions {
                // Skip saved "unsubscribed" rows — apply never clears.
                if s.tx_channel.trim().is_empty() {
                    continue;
                }
                let want = (s.tx_device.clone(), s.tx_channel.clone());
                if current.get(&(dev.name.clone(), s.rx_channel)) == Some(&want) {
                    continue;
                }
                self.inner
                    .dante
                    .subscribe(&dev.name, s.rx_channel, &s.tx_device, &s.tx_channel)
                    .await?;
                applied = applied.saturating_add(1);
            }
        }
        Ok(applied)
    }
}
