//! `PatchbayService` implementation over the engine + preset store.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc;

use parking_lot::RwLock;
use patchbay_proto::services::patchbay_service::{
    PatchbayServiceStreamSource, patchbay_service_stream_service_descriptor, stream_serve,
};
use patchbay_proto::{
    AliasEntry, ApplyReport, CanvasView, ClockDefaults, ClockInfo, ColorEntry, DanteDevice,
    DanteDeviceConfig, DanteStatus, GraphEvent, GraphSnapshot, IconEntry, LatencyRule, NamedRoute,
    PatchbayError, PatchbayService, PortDirection, PresetLink, RouteEndpoint, RoutingPreset,
    ServiceAction, ServiceStatus, VirtualSink, patchbay_service_service_descriptor,
    serve_patchbay_service,
};

use crate::engine::{self, Command, EngineHandle};
use crate::presets::PresetStore;
use crate::store::GraphStore;

/// The headless patchbay backend: PipeWire engine thread, graph
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
}

impl Default for PatchbayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl PatchbayBackend {
    /// Spawn the PipeWire thread and the event pump. The replay window
    /// covers the snapshot→subscribe gap: clients fetch `graph()` first,
    /// then apply (idempotent) events from the recent past.
    pub fn new() -> Self {
        let store = Arc::new(RwLock::new(GraphStore::default()));
        let presets = Arc::new(PresetStore::open());
        let (events_tx, events_rx) = mpsc::channel::<GraphEvent>();
        let enrich_tx = events_tx.clone();
        let poll_tx = events_tx.clone();
        let engine = engine::spawn(store.clone(), events_tx);

        // Live node-state poller: pw-dump every couple seconds and emit
        // NodeStateChanged deltas. This is the free "is anything going
        // through here" signal (running/idle/suspended) — PipeWire has
        // no per-port level API, so activity state is the honest,
        // no-tap answer. Read-only shell-out; only runs while the app
        // is up. Quiet graphs emit nothing.
        {
            let store = store.clone();
            std::thread::Builder::new()
                .name("patchbay-states".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(2000));
                        crate::enrich::poll_node_states(&store, &poll_tx);
                    }
                })
                .expect("spawn patchbay-states thread");
        }
        // Debounce gate for the pw-dump enrichment pass.
        let enrich_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Debounce gate for named-route auto-apply (a node/port burst
        // schedules ONE apply once the graph settles).
        let routes_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Big window: a single app connecting is a burst of hundreds of
        // port events, a PipeWire reconnect is ~2000 — a small ring
        // drops the middle of the burst and clients silently lose
        // nodes (REAPER "not showing up" was exactly this).
        let events_hub = architect::PubSub::sliding(16_384);
        let pump = events_hub.clone();
        {
            let store = store.clone();
            let presets = presets.clone();
            let engine = engine.clone();
            std::thread::Builder::new()
                .name("patchbay-events".into())
                .spawn(move || {
                    while let Ok(ev) = events_rx.recv() {
                        // Any node addition schedules a debounced
                        // pw-dump enrichment (full props the registry
                        // subset lacks — node.group, application.*).
                        if matches!(ev, GraphEvent::NodeAdded(_))
                            && !enrich_pending.swap(true, std::sync::atomic::Ordering::SeqCst)
                        {
                            let store = store.clone();
                            let tx = enrich_tx.clone();
                            let pending = enrich_pending.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_millis(1500));
                                pending.store(false, std::sync::atomic::Ordering::SeqCst);
                                crate::enrich::enrich_nodes(&store, &tx);
                            });
                        }
                        // Nodes/ports appearing (REAPER launches, Inferno
                        // re-opens) schedule a debounced named-route apply
                        // — the explicit auto-connect. Only creates the
                        // links the user declared; no routes = no-op.
                        if matches!(ev, GraphEvent::NodeAdded(_) | GraphEvent::PortAdded(_))
                            && !routes_pending.load(std::sync::atomic::Ordering::SeqCst)
                            && !presets.routes().is_empty()
                        {
                            routes_pending.store(true, std::sync::atomic::Ordering::SeqCst);
                            let store = store.clone();
                            let presets = presets.clone();
                            let engine = engine.clone();
                            let pending = routes_pending.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_millis(2000));
                                pending.store(false, std::sync::atomic::Ordering::SeqCst);
                                apply_named_routes(&store, &presets, &engine);
                            });
                        }
                        match &ev {
                            // REAPER (re)appeared: pick up its channel
                            // names from the host chanmap once the
                            // ports have registered.
                            GraphEvent::NodeAdded(n) if n.name == "REAPER" => {
                                let store = store.clone();
                                let presets = presets.clone();
                                std::thread::spawn(move || {
                                    std::thread::sleep(std::time::Duration::from_secs(2));
                                    auto_import_chanmap(&store, &presets, "REAPER");
                                });
                            }
                            // Engine reconnected (PipeWire restart):
                            // re-create the persisted virtual sinks
                            // once the fresh mirror has settled.
                            GraphEvent::Reset => {
                                let store = store.clone();
                                let presets = presets.clone();
                                let engine = engine.clone();
                                std::thread::spawn(move || {
                                    std::thread::sleep(std::time::Duration::from_secs(3));
                                    ensure_virtual_sinks(&store, &presets, &engine);
                                    apply_named_routes(&store, &presets, &engine);
                                });
                            }
                            _ => {}
                        }
                        pump.publish(ev);
                    }
                    tracing::warn!("patchbay event pump ended (engine gone)");
                })
                .expect("spawn patchbay-events thread");
        }
        // First connect emits no Reset — seed the virtual sinks once
        // the initial mirror is up.
        {
            let store = store.clone();
            let presets = presets.clone();
            let engine = engine.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                ensure_virtual_sinks(&store, &presets, &engine);
                apply_named_routes(&store, &presets, &engine);
            });
        }
        Self {
            inner: Arc::new(Inner {
                store,
                engine,
                events_hub,
                presets,
                dante: crate::dante_net::DanteEndpoints::default(),
                icons: crate::icons::IconCache::default(),
            }),
        }
    }

    /// A fresh `LayerRouter` serving this backend — the RPC layer plus
    /// its `#[subscribe]` stream sibling over the same impl.
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
                .ok_or_else(|| PatchbayError::not_found("output port", output_port))?
                .id;
            let in_node = store
                .node_of_port(input_port)
                .ok_or_else(|| PatchbayError::not_found("input port", input_port))?
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
    /// Rewrite the WirePlumber drop-in from `rules` (blocking I/O +
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

use patchbay_proto::sink_node_name;

/// Create any persisted virtual sink that isn't in the live graph.
fn ensure_virtual_sinks(
    store: &Arc<RwLock<GraphStore>>,
    presets: &Arc<PresetStore>,
    engine: &EngineHandle,
) {
    for sink in presets.virtual_sinks() {
        let node_name = sink_node_name(&sink.name);
        let live = store.read().nodes.values().any(|n| n.name == node_name);
        if live {
            continue;
        }
        tracing::info!(name = %sink.name, "creating virtual sink");
        if let Err(e) = engine.send(Command::CreateVirtualSink {
            node_name,
            description: sink.name.clone(),
            channels: sink.channels.max(1),
        }) {
            tracing::warn!("virtual sink create failed: {e}");
        }
    }
}

/// Normalize an alias/port name for route matching: lowercase, drop a
/// leading `"N - "` channel-number prefix and a trailing `[DSP]`,
/// collapse whitespace. So `"81 - Engineer Vocal [DSP]"` and
/// `"Engineer Vocal"` compare equal (but " L"/" R" is kept — stereo
/// halves stay distinct).
fn norm_route_name(s: &str) -> String {
    let s = s.trim();
    // Strip a leading "<digits> - ".
    let s = match s.split_once(" - ") {
        Some((pre, rest)) if !pre.is_empty() && pre.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => s,
    };
    let mut s = s.trim().to_string();
    // Strip a trailing "[DSP]" (any case).
    let lower = s.to_lowercase();
    if lower.ends_with("[dsp]") {
        s.truncate(s.len() - "[dsp]".len());
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Resolve one route endpoint to a live port id of the given direction,
/// matching the port's ALIAS (or raw name) normalized. `ep.node`, when
/// set, narrows to a node whose name / label / alias matches.
fn resolve_route_endpoint(
    store: &GraphStore,
    aliases: &HashMap<String, String>,
    ep: &RouteEndpoint,
    dir: PortDirection,
) -> Option<u32> {
    let want_port = norm_route_name(&ep.port);
    if want_port.is_empty() {
        return None;
    }
    let want_node = ep.node.trim().to_lowercase();
    for p in store.ports.values() {
        if p.direction != dir {
            continue;
        }
        let Some(node) = store.nodes.get(&p.node_id) else {
            continue;
        };
        if !want_node.is_empty() {
            let node_alias = aliases
                .get(&node.name)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if node.name.to_lowercase() != want_node
                && node.label.to_lowercase() != want_node
                && node_alias != want_node
            {
                continue;
            }
        }
        let alias = aliases.get(&format!("{}:{}", node.name, p.name));
        let cand = alias.map(String::as_str).unwrap_or(&p.name);
        if norm_route_name(cand) == want_port {
            return Some(p.id);
        }
    }
    None
}

/// The port sentinel that turns a route into a whole-node BANK route:
/// pair the two nodes' ports 1:1 by numeric suffix (like
/// `connect_one_to_one`) instead of matching a single named port.
const BANK_PORT: &str = "*";

/// Resolve a node by `node.name` / label / alias (case-insensitive).
fn resolve_node<'a>(
    store: &'a GraphStore,
    aliases: &HashMap<String, String>,
    query: &str,
) -> Option<&'a patchbay_proto::PwNode> {
    let q = query.trim().to_lowercase();
    store.nodes.values().find(|n| {
        n.name.to_lowercase() == q
            || n.label.to_lowercase() == q
            || aliases.get(&n.name).map(|s| s.to_lowercase()).as_deref() == Some(q.as_str())
    })
}

/// Issue a CreateLink for `out → inp` unless it already exists. Returns
/// whether a command was sent.
fn ensure_link(store: &GraphStore, engine: &EngineHandle, out: u32, inp: u32) -> bool {
    if store.link_between(out, inp).is_some() {
        return false;
    }
    let (Some(on), Some(inn)) = (store.node_of_port(out), store.node_of_port(inp)) else {
        return false;
    };
    engine
        .send(Command::CreateLink {
            output_node: on.id,
            output_port: out,
            input_node: inn.id,
            input_port: inp,
        })
        .is_ok()
}

/// Apply every enabled named route against the live graph: create the
/// missing link(s) for each route whose endpoints resolve. Idempotent —
/// never destroys anything, never duplicates an existing link. Returns
/// the number of links created. Runs on graph-settle and on the
/// `apply_routes` RPC.
fn apply_named_routes(
    store: &Arc<RwLock<GraphStore>>,
    presets: &Arc<PresetStore>,
    engine: &EngineHandle,
) -> u32 {
    let routes = presets.routes();
    if routes.is_empty() {
        return 0;
    }
    let aliases: HashMap<String, String> = presets
        .aliases()
        .into_iter()
        .map(|a| (a.target, a.alias))
        .collect();
    let store_r = store.read();
    let mut created = 0;
    for route in routes.iter().filter(|r| r.enabled) {
        // Bank route: pair the whole output node to the whole input node
        // 1:1 by numeric suffix (out<N>/capture_N → in<N>/playback_N).
        if route.from.port.trim() == BANK_PORT || route.to.port.trim() == BANK_PORT {
            let (Some(on), Some(inn)) = (
                resolve_node(&store_r, &aliases, &route.from.node),
                resolve_node(&store_r, &aliases, &route.to.node),
            ) else {
                continue;
            };
            let by_channel = |node: u32, dir: PortDirection| {
                let mut m = std::collections::BTreeMap::new();
                for p in store_r.ports.values() {
                    // Skip MIDI ports: REAPER exposes both `in18` and
                    // `MIDI Input 18`, which collide on channel 18 —
                    // pairing must land on the AUDIO port.
                    if p.node_id == node
                        && p.direction == dir
                        && p.media_kind != patchbay_proto::MediaKind::Midi
                    {
                        if let Some(ch) = crate::chanmap::channel_of_port(&p.name) {
                            m.entry(ch).or_insert(p.id);
                        }
                    }
                }
                m
            };
            let outs = by_channel(on.id, PortDirection::Output);
            let ins = by_channel(inn.id, PortDirection::Input);
            for (ch, out) in outs {
                if let Some(&inp) = ins.get(&ch) {
                    if ensure_link(&store_r, engine, out, inp) {
                        created += 1;
                    }
                }
            }
            continue;
        }
        let (Some(out), Some(inp)) = (
            resolve_route_endpoint(&store_r, &aliases, &route.from, PortDirection::Output),
            resolve_route_endpoint(&store_r, &aliases, &route.to, PortDirection::Input),
        ) else {
            continue;
        };
        if ensure_link(&store_r, engine, out, inp) {
            tracing::info!(route = %route.name, out, inp, "named route applied");
            created += 1;
        }
    }
    created
}

/// Non-destructive chanmap import: name `node`'s channels from the
/// host's default ReaperChanMap, skipping channels the user already
/// aliased in the patchbay. Missing chanmap file = silently nothing.
fn auto_import_chanmap(store: &Arc<RwLock<GraphStore>>, presets: &Arc<PresetStore>, node: &str) {
    let Ok(names) = crate::chanmap::read_names("") else {
        return;
    };
    let ports: Vec<String> = {
        let s = store.read();
        let Some(n) = s.nodes.values().find(|n| n.name == node) else {
            return;
        };
        s.ports
            .values()
            .filter(|p| p.node_id == n.id)
            .map(|p| p.name.clone())
            .collect()
    };
    let mut written = 0u32;
    for port in ports {
        let Some(channel) = crate::chanmap::channel_of_port(&port) else {
            continue;
        };
        let target = format!("{node}:{port}");
        if presets.has_alias(&target) {
            continue;
        }
        if let Some(name) = names.get(&channel) {
            presets.set_alias(target, name.clone());
            written += 1;
        }
    }
    if written > 0 {
        tracing::info!(node, written, "auto-imported chanmap names");
    }
}

impl PatchbayServiceStreamSource for PatchbayBackend {
    fn graph_events_hub(&self) -> &architect::PubSub<GraphEvent> {
        &self.inner.events_hub
    }
}

impl PatchbayService for PatchbayBackend {
    async fn graph(&self) -> Result<GraphSnapshot, PatchbayError> {
        Ok(self.inner.store.read().snapshot())
    }

    async fn create_link(&self, output_port: u32, input_port: u32) -> Result<(), PatchbayError> {
        self.create_link_inner(output_port, input_port).map(|_| ())
    }

    async fn destroy_link(&self, link_id: u32) -> Result<(), PatchbayError> {
        if !self.inner.store.read().links.contains_key(&link_id) {
            return Err(PatchbayError::not_found("link", link_id));
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
        // Pair by numeric suffix (out7 → playback_7). Non-numeric
        // ports don't participate.
        let pairs: Vec<(u32, u32)> = {
            let store = self.inner.store.read();
            let node_id = |name: &str| {
                store
                    .nodes
                    .values()
                    .find(|n| n.name == name)
                    .map(|n| n.id)
                    .ok_or_else(|| PatchbayError::not_found("node", name))
            };
            let out_id = node_id(&output_node)?;
            let in_id = node_id(&input_node)?;
            let by_channel = |node: u32, dir: patchbay_proto::PortDirection| {
                let mut m = std::collections::BTreeMap::new();
                for p in store.ports.values() {
                    // Skip MIDI ports (see apply_named_routes): `in18`
                    // and `MIDI Input 18` collide on channel 18.
                    if p.node_id == node
                        && p.direction == dir
                        && p.media_kind != patchbay_proto::MediaKind::Midi
                    {
                        if let Some(ch) = crate::chanmap::channel_of_port(&p.name) {
                            m.entry(ch).or_insert(p.id);
                        }
                    }
                }
                m
            };
            let outs = by_channel(out_id, patchbay_proto::PortDirection::Output);
            let ins = by_channel(in_id, patchbay_proto::PortDirection::Input);
            outs.into_iter()
                .filter_map(|(ch, out)| ins.get(&ch).map(|inp| (out, *inp)))
                .collect()
        };
        if pairs.is_empty() {
            return Err(PatchbayError::Internal(format!(
                "no numeric-suffix port pairs between {output_node} and {input_node}"
            )));
        }
        let mut created = 0;
        for (out, inp) in pairs {
            if self.create_link_inner(out, inp)? {
                created += 1;
            }
        }
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
                    format!("{output_node} or {input_node}"),
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
        Ok(link_ids.len() as u32)
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
        Ok(self.inner.presets.upsert_preset(name, description, links))
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
        let mut report = ApplyReport::default();

        // Create every remembered link whose endpoints are live.
        for link in &preset.links {
            let resolved = {
                let store = self.inner.store.read();
                let out = store.port_by_names(&link.output_node, &link.output_port);
                let inp = store.port_by_names(&link.input_node, &link.input_port);
                out.zip(inp)
            };
            match resolved {
                None => report.missing.push(link.clone()),
                Some((out, inp)) => match self.create_link_inner(out, inp)? {
                    true => report.created += 1,
                    false => report.existing += 1,
                },
            }
        }

        // Exclusive: tear down live links the preset doesn't contain.
        if exclusive {
            let wanted: HashSet<&PresetLink> = preset.links.iter().collect();
            let extras: Vec<u32> = {
                let store = self.inner.store.read();
                store
                    .links
                    .values()
                    .filter_map(|l| {
                        let named = PresetLink {
                            output_node: store.nodes.get(&l.output_node)?.name.clone(),
                            output_port: store.ports.get(&l.output_port)?.name.clone(),
                            input_node: store.nodes.get(&l.input_node)?.name.clone(),
                            input_port: store.ports.get(&l.input_port)?.name.clone(),
                        };
                        (!wanted.contains(&named)).then_some(l.id)
                    })
                    .collect()
            };
            for id in extras {
                self.inner
                    .engine
                    .send(Command::DestroyLink { id })
                    .map_err(PatchbayError::EngineUnavailable)?;
                report.destroyed += 1;
            }
        }
        Ok(report)
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
        self.inner.presets.set_route(route);
        // Apply immediately so a just-added route wires up now if both
        // ends are already present.
        apply_named_routes(&self.inner.store, &self.inner.presets, &self.inner.engine);
        Ok(())
    }

    async fn delete_route(&self, name: String) -> Result<(), PatchbayError> {
        self.inner
            .presets
            .delete_route(&name)
            .then_some(())
            .ok_or_else(|| PatchbayError::not_found("route", &name))
    }

    async fn apply_routes(&self) -> Result<u32, PatchbayError> {
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
        self.inner.presets.set_alias(target, alias);
        Ok(())
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
        let mut written = 0;
        for port in ports {
            let Some(channel) = crate::chanmap::channel_of_port(&port) else {
                continue;
            };
            if let Some(name) = names.get(&channel) {
                self.inner
                    .presets
                    .set_alias(format!("{node}:{port}"), name.clone());
                written += 1;
            }
        }
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
        Ok(names.len() as u32)
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
                if device.trim().is_empty() {
                    "<any>"
                } else {
                    &device
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
        let mut written = 0;
        for port in ports {
            let Some(channel) = crate::chanmap::channel_of_port(&port) else {
                continue;
            };
            if let Some(name) = names.get(&channel) {
                self.inner
                    .presets
                    .set_alias(format!("{node}:{port}"), name.clone());
                written += 1;
            }
        }
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
        self.inner.presets.add_virtual_sink(sink);
        ensure_virtual_sinks(&self.inner.store, &self.inner.presets, &self.inner.engine);
        Ok(())
    }

    async fn remove_virtual_sink(&self, name: String) -> Result<(), PatchbayError> {
        if !self.inner.presets.remove_virtual_sink(&name) {
            return Err(PatchbayError::not_found("virtual sink", &name));
        }
        // Destroy the live node too — but ONLY if it carries the
        // patchbay.virtual tag (never an arbitrary node).
        let node_name = sink_node_name(&name);
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
        self.inner.presets.save_view(view);
        Ok(())
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
        self.inner.presets.set_color(target, color);
        Ok(())
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
        let n = cfg.len() as u32;
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
        let mut applied = 0;
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
                applied += 1;
            }
        }
        Ok(applied)
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use patchbay_proto::{MediaKind, NodeState, PwNode, PwPort};

    fn node(id: u32, name: &str) -> PwNode {
        PwNode {
            id,
            name: name.into(),
            label: name.into(),
            media_class: String::new(),
            media_kind: MediaKind::Audio,
            app_name: String::new(),
            latency: String::new(),
            icon_name: String::new(),
            group: String::new(),
            virtual_sink: false,
            state: NodeState::Running,
        }
    }
    fn port(id: u32, node_id: u32, name: &str, dir: PortDirection) -> PwPort {
        PwPort {
            id,
            node_id,
            name: name.into(),
            direction: dir,
            media_kind: MediaKind::Audio,
        }
    }

    #[test]
    fn normalization_strips_prefix_and_dsp_but_keeps_lr() {
        assert_eq!(
            norm_route_name("81 - Engineer Vocal [DSP]"),
            "engineer vocal"
        );
        assert_eq!(norm_route_name("42 - Engineer Vocal"), "engineer vocal");
        assert_eq!(norm_route_name("Engineer Vocal"), "engineer vocal");
        // L/R must stay distinct — stereo halves are different channels.
        assert_ne!(
            norm_route_name("Vocal 1 Mix L"),
            norm_route_name("Vocal 1 Mix R")
        );
    }

    #[test]
    fn resolves_by_alias_across_channel_numbers() {
        // Inferno source outputs capture_96 (aliased with the [DSP] name
        // and a different channel number than REAPER's input).
        let mut store = GraphStore::default();
        store.nodes.insert(1, node(1, "Inferno source"));
        store.nodes.insert(2, node(2, "REAPER"));
        store
            .ports
            .insert(100, port(100, 1, "capture_96", PortDirection::Output));
        store
            .ports
            .insert(200, port(200, 2, "in5", PortDirection::Input));

        let mut aliases = HashMap::new();
        aliases.insert(
            "Inferno source:capture_96".into(),
            "96 - Engineer Vocal [DSP]".into(),
        );
        aliases.insert("REAPER:in5".into(), "5 - Engineer Vocal".into());

        let from = RouteEndpoint {
            node: "Inferno source".into(),
            port: "Engineer Vocal".into(),
        };
        let to = RouteEndpoint {
            node: "REAPER".into(),
            port: "Engineer Vocal".into(),
        };

        assert_eq!(
            resolve_route_endpoint(&store, &aliases, &from, PortDirection::Output),
            Some(100)
        );
        assert_eq!(
            resolve_route_endpoint(&store, &aliases, &to, PortDirection::Input),
            Some(200)
        );
        // Direction matters: the output-side spec must not match the input port.
        assert_eq!(
            resolve_route_endpoint(&store, &aliases, &from, PortDirection::Input),
            None
        );
    }

    #[test]
    fn node_filter_disambiguates_same_alias() {
        // Two output ports share the normalized name; the node filter picks one.
        let mut store = GraphStore::default();
        store.nodes.insert(1, node(1, "Inferno source"));
        store.nodes.insert(2, node(2, "Other Card"));
        store
            .ports
            .insert(100, port(100, 1, "capture_1", PortDirection::Output));
        store
            .ports
            .insert(101, port(101, 2, "out_1", PortDirection::Output));
        let mut aliases = HashMap::new();
        aliases.insert("Inferno source:capture_1".into(), "Talkback".into());
        aliases.insert("Other Card:out_1".into(), "Talkback".into());

        let ep = RouteEndpoint {
            node: "Other Card".into(),
            port: "Talkback".into(),
        };
        assert_eq!(
            resolve_route_endpoint(&store, &aliases, &ep, PortDirection::Output),
            Some(101)
        );
    }
}
