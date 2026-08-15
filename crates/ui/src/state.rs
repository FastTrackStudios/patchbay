//! Global signals + the service handle.

use std::collections::HashMap;
use std::sync::Arc;

use dioxus::prelude::*;
use patchbay_proto::{
    ApplyReport, ClockInfo, DanteDevice, DanteStatus, GraphEvent, GraphSnapshot, MediaKind,
    PatchbayServiceClient, PortDirection, RoutingPreset, ServiceStatus, VirtualSink,
};

/// The service client, provided via context by the shell.
#[derive(Clone)]
pub struct PatchbayHandle(pub Arc<PatchbayServiceClient>);

/// Convenience accessor for components.
pub fn use_patchbay() -> PatchbayHandle {
    use_context::<PatchbayHandle>()
}

// ─── Data mirrors ───────────────────────────────────────────────────────

pub static GRAPH: GlobalSignal<GraphSnapshot> = Signal::global(GraphSnapshot::default);
/// `target → alias` (`node.name` or `node.name:port.name`).
pub static ALIASES: GlobalSignal<HashMap<String, String>> = Signal::global(HashMap::new);
/// `target → color` (`node.name` or `node.name:port.name`).
pub static COLORS: GlobalSignal<HashMap<String, String>> = Signal::global(HashMap::new);
/// `icon_name → data: URI` (empty string = looked up, not found — so
/// misses aren't re-requested every graph change).
pub static ICONS: GlobalSignal<HashMap<String, String>> = Signal::global(HashMap::new);
pub static PRESETS: GlobalSignal<Vec<RoutingPreset>> = Signal::global(Vec::new);
pub static CLOCK: GlobalSignal<ClockInfo> = Signal::global(ClockInfo::default);
pub static DANTE: GlobalSignal<DanteStatus> = Signal::global(DanteStatus::default);
pub static SERVICES: GlobalSignal<Vec<ServiceStatus>> = Signal::global(Vec::new);
pub static LATENCY_RULES: GlobalSignal<Vec<patchbay_proto::LatencyRule>> = Signal::global(Vec::new);
pub static VIRTUAL_SINKS: GlobalSignal<Vec<VirtualSink>> = Signal::global(Vec::new);
pub static VIEWS: GlobalSignal<Vec<patchbay_proto::CanvasView>> = Signal::global(Vec::new);
pub static CLOCK_DEFAULTS: GlobalSignal<patchbay_proto::ClockDefaults> =
    Signal::global(patchbay_proto::ClockDefaults::default);
pub static DANTE_DEVICES: GlobalSignal<Vec<DanteDevice>> = Signal::global(Vec::new);
/// Dante grid fetch in flight.
pub static DANTE_LOADING: GlobalSignal<bool> = Signal::global(|| false);
/// Last dante grid error (empty = fine).
pub static DANTE_ERROR: GlobalSignal<String> = Signal::global(String::new);

/// Which main view is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Patchbay,
    Dante,
}
pub static VIEW: GlobalSignal<View> = Signal::global(|| View::Patchbay);

// ─── View state ─────────────────────────────────────────────────────────

/// Output ports armed for connecting (click an input row to link).
/// One entry for a plain port, two for a condensed stereo pair.
pub static ARMED_OUTPUTS: GlobalSignal<Vec<u32>> = Signal::global(Vec::new);
/// Node id whose inspector is open.
pub static SELECTED_NODE: GlobalSignal<Option<u32>> = Signal::global(|| None);
pub static SEARCH: GlobalSignal<String> = Signal::global(String::new);
/// Which media domain the graph shows (Audio | MIDI | Video tabs).
/// `Other`-kind ports ride along in the Audio tab.
pub static MEDIA_TAB: GlobalSignal<MediaKind> = Signal::global(|| MediaKind::Audio);
/// Canvas zoom factor.
pub static ZOOM: GlobalSignal<f64> = Signal::global(|| 1.0);
/// Canvas pan offset (px, pre-zoom screen space).
pub static PAN: GlobalSignal<(f64, f64)> = Signal::global(|| (0.0, 0.0));
pub static HIDE_UNCONNECTED: GlobalSignal<bool> = Signal::global(|| false);
/// Drop sinks' monitor ports from the canvas entirely.
pub static HIDE_MONITORS: GlobalSignal<bool> = Signal::global(|| false);
/// Port-group expansion (`node.name/direction/prefix` → expanded).
/// Groups default to collapsed — that's the whole point with 128-channel
/// Inferno nodes.
pub static EXPANDED_GROUPS: GlobalSignal<HashMap<String, bool>> = Signal::global(HashMap::new);
/// Outcome of the last preset apply, for the status line.
pub static LAST_REPORT: GlobalSignal<Option<(String, ApplyReport)>> = Signal::global(|| None);
/// Last preset diff preview (name, human summary).
pub static PRESET_DIFF: GlobalSignal<Option<(String, String)>> = Signal::global(|| None);
/// Node id under the pointer — cables off its signal path dim.
pub static HOVERED_NODE: GlobalSignal<Option<u32>> = Signal::global(|| None);
/// Per-column collapse (Inputs | Applications | Outputs): collapsed
/// columns render cards as headers only, cables converging on them.
pub static COLLAPSED_COLS: GlobalSignal<[bool; 4]> = Signal::global(|| [false; 4]);

// ─── Drag-to-connect ────────────────────────────────────────────────────

/// What a canvas drag started from.
#[derive(Clone, PartialEq)]
pub enum DragSource {
    /// Port row(s): direction + port ids ([L, R] for a pair, the whole
    /// bank for a collapsed group row).
    Ports(PortDirection, Vec<u32>),
    /// A node header (bulk 1:1 connect on drop).
    Node(String),
}

/// Active drag gesture: source, start anchor and current pointer, both
/// in world coordinates.
#[derive(Clone, PartialEq)]
pub struct Drag {
    pub source: DragSource,
    pub start: (f64, f64),
    pub current: (f64, f64),
}

pub static DRAG: GlobalSignal<Option<Drag>> = Signal::global(|| None);

// ─── Undo (link operations) ─────────────────────────────────────────────

/// One undoable gesture: the (output_port, input_port, created) set it
/// performed. Undo re-applies each entry inverted.
pub type LinkOp = Vec<(u32, u32, bool)>;

pub static UNDO: GlobalSignal<Vec<LinkOp>> = Signal::global(Vec::new);

fn record_op(op: LinkOp) {
    if op.is_empty() {
        return;
    }
    let mut undo = UNDO.write();
    undo.push(op);
    let excess = undo.len().saturating_sub(50);
    if excess > 0 {
        undo.drain(..excess);
    }
}

// ─── Mutations ──────────────────────────────────────────────────────────

/// Fold one engine event into the graph mirror (idempotent — replayed
/// events after a snapshot fetch are harmless).
pub fn apply_graph_event(ev: &GraphEvent) {
    let mut g = GRAPH.write();
    match ev {
        GraphEvent::Reset => *g = GraphSnapshot::default(),
        GraphEvent::NodeAdded(n) => {
            g.nodes.retain(|x| x.id != n.id);
            g.nodes.push(n.clone());
            g.nodes.sort_by_key(|x| x.id);
        }
        GraphEvent::NodeRemoved { id } => g.nodes.retain(|x| x.id != *id),
        GraphEvent::NodeStateChanged { id, state } => {
            if let Some(n) = g.nodes.iter_mut().find(|x| x.id == *id) {
                n.state = *state;
            }
        }
        GraphEvent::PortAdded(p) => {
            g.ports.retain(|x| x.id != p.id);
            g.ports.push(p.clone());
            g.ports.sort_by_key(|x| x.id);
        }
        GraphEvent::PortRemoved { id, .. } => g.ports.retain(|x| x.id != *id),
        GraphEvent::LinkAdded(l) => {
            g.links.retain(|x| x.id != l.id);
            g.links.push(l.clone());
            g.links.sort_by_key(|x| x.id);
        }
        GraphEvent::LinkStateChanged { id, active } => {
            if let Some(l) = g.links.iter_mut().find(|x| x.id == *id) {
                l.active = *active;
            }
        }
        GraphEvent::LinkRemoved { id } => g.links.retain(|x| x.id != *id),
    }
}

/// Replace the graph mirror wholesale (periodic reconcile — the event
/// stream can drop under burst; the snapshot is always authoritative).
/// Skips the signal write when nothing changed so it never causes
/// re-renders on a quiet graph.
pub fn replace_graph(snap: GraphSnapshot) {
    if *GRAPH.peek() != snap {
        *GRAPH.write() = snap;
    }
}

/// Fetch everything renderable (initial mount + manual refresh).
pub async fn refresh_all(handle: &PatchbayHandle) {
    match handle.0.graph().await {
        Ok(snap) => *GRAPH.write() = snap,
        Err(e) => tracing::warn!("graph fetch failed: {e:?}"),
    }
    refresh_meta(handle).await;
}

/// The cheap non-graph state (aliases, presets, clock, dante).
pub async fn refresh_meta(handle: &PatchbayHandle) {
    if let Ok(aliases) = handle.0.aliases().await {
        *ALIASES.write() = aliases.into_iter().map(|a| (a.target, a.alias)).collect();
    }
    if let Ok(colors) = handle.0.colors().await {
        *COLORS.write() = colors.into_iter().map(|c| (c.target, c.color)).collect();
    }
    if let Ok(presets) = handle.0.list_presets().await {
        *PRESETS.write() = presets;
    }
    if let Ok(clock) = handle.0.clock().await {
        *CLOCK.write() = clock;
    }
    if let Ok(dante) = handle.0.dante_status().await {
        *DANTE.write() = dante;
    }
    if let Ok(services) = handle.0.services().await {
        *SERVICES.write() = services;
    }
    if let Ok(rules) = handle.0.latency_rules().await {
        *LATENCY_RULES.write() = rules;
    }
    if let Ok(sinks) = handle.0.virtual_sinks().await {
        *VIRTUAL_SINKS.write() = sinks;
    }
    if let Ok(views) = handle.0.views().await {
        *VIEWS.write() = views;
    }
    if let Ok(defaults) = handle.0.clock_defaults().await {
        *CLOCK_DEFAULTS.write() = defaults;
    }
}

/// Re-scan the Dante network (mDNS + per-device ARC — seconds).
pub async fn refresh_dante(handle: &PatchbayHandle) {
    *DANTE_LOADING.write() = true;
    match handle.0.dante_network().await {
        Ok(devices) => {
            *DANTE_DEVICES.write() = devices;
            DANTE_ERROR.write().clear();
        }
        Err(e) => *DANTE_ERROR.write() = format!("dante scan failed: {e}"),
    }
    *DANTE_LOADING.write() = false;
}

/// Connect-or-disconnect a set of (output, input) port pairs (helvum's
/// toggle semantics), recorded as ONE undo step. The graph mirror
/// updates via the event stream.
pub fn apply_link_toggles(handle: PatchbayHandle, pairs: Vec<(u32, u32)>) {
    let mut op: LinkOp = Vec::new();
    let mut work: Vec<(Option<u32>, u32, u32)> = Vec::new();
    {
        let graph = GRAPH.peek();
        for (out, inp) in pairs {
            let existing = graph
                .links
                .iter()
                .find(|l| l.output_port == out && l.input_port == inp)
                .map(|l| l.id);
            op.push((out, inp, existing.is_none()));
            work.push((existing, out, inp));
        }
    }
    record_op(op);
    spawn(async move {
        for (existing, out, inp) in work {
            let res = match existing {
                Some(id) => handle.0.destroy_link(id).await,
                None => handle.0.create_link(out, inp).await,
            };
            if let Err(e) = res {
                tracing::warn!("link toggle failed: {e:?}");
            }
        }
    });
}

/// Single-pair convenience over [`apply_link_toggles`].
pub fn toggle_link(handle: PatchbayHandle, output_port: u32, input_port: u32) {
    apply_link_toggles(handle, vec![(output_port, input_port)]);
}

/// Destroy the given links (a clicked cable), recorded as one undo step.
pub fn disconnect_links(handle: PatchbayHandle, ids: Vec<u32>) {
    let op: LinkOp = {
        let graph = GRAPH.peek();
        ids.iter()
            .filter_map(|id| {
                let l = graph.links.iter().find(|l| l.id == *id)?;
                Some((l.output_port, l.input_port, false))
            })
            .collect()
    };
    record_op(op);
    spawn(async move {
        for id in ids {
            if let Err(e) = handle.0.destroy_link(id).await {
                tracing::warn!("cable disconnect failed: {e:?}");
            }
        }
    });
}

/// Undo the most recent link gesture: created links are destroyed,
/// destroyed links re-created. Endpoints that left the graph since are
/// skipped.
pub fn undo_last(handle: PatchbayHandle) {
    let Some(op) = UNDO.write().pop() else { return };
    let mut destroy: Vec<u32> = Vec::new();
    let mut create: Vec<(u32, u32)> = Vec::new();
    {
        let graph = GRAPH.peek();
        for (out, inp, created) in op {
            if created {
                if let Some(l) = graph
                    .links
                    .iter()
                    .find(|l| l.output_port == out && l.input_port == inp)
                {
                    destroy.push(l.id);
                }
            } else {
                create.push((out, inp));
            }
        }
    }
    spawn(async move {
        for id in destroy {
            if let Err(e) = handle.0.destroy_link(id).await {
                tracing::warn!("undo destroy failed: {e:?}");
            }
        }
        for (out, inp) in create {
            if let Err(e) = handle.0.create_link(out, inp).await {
                tracing::warn!("undo create failed: {e:?}");
            }
        }
    });
}

/// Portable async sleep (tokio on native, gloo on wasm).
pub async fn sleep_secs(secs: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new((secs * 1000) as u32).await;
}

/// The user-pickable cable/port color palette.
pub const PALETTE: [&str; 8] = [
    "#e05c52", // red
    "#ff9f43", // orange
    "#ffd479", // yellow
    "#58d68d", // green
    "#39c2b9", // teal
    "#4a90d9", // blue
    "#b487ff", // purple
    "#ff7ab8", // pink
];

/// Deterministic fallback color from a node name — the same hue every
/// session, so unclassified nodes are still tellable-apart.
pub fn auto_color(name: &str) -> String {
    let mut h: u32 = 2_166_136_261;
    for b in name.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16_777_619);
    }
    format!("hsl({}, 60%, 60%)", h % 360)
}

/// FTS instrument-category color for a label ("28 - Guitar 1 L" →
/// guitars sky-blue), via music-catalog — the same scheme
/// dynamic-template paints REAPER tracks with (drums red, bass yellow,
/// guitars blue, acoustic cyan, keys green, synths violet, vocals
/// pink…).
///
/// Resolution: the whole label, then progressively dropping trailing
/// words ("Kick In" → "Kick"; "Guitar 1 Mix L" → "Guitar 1" →
/// "guitar"), then single words last-to-first ("Engineer Vocal" →
/// "vocal"), then studio-utility words (mic/talkback/mix/…).
pub fn category_color(label: &str) -> Option<String> {
    let mut words: Vec<&str> = label.split_whitespace().collect();
    while !words.is_empty() {
        let candidate = words.join(" ");
        if let Some(c) = music_catalog::lookup::color_for_region(&candidate) {
            return Some(c.to_hex_string());
        }
        words.pop();
    }
    // Word-level pass, most-significant (last) word first.
    for word in label.split_whitespace().rev() {
        let w = word.to_lowercase();
        if w.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Some(c) = music_catalog::lookup::color_for_name(&w) {
            return Some(c.to_hex_string());
        }
        if let Some(c) = utility_color(&w) {
            return Some(c);
        }
    }
    None
}

/// Studio-plumbing words that aren't instruments but deserve stable,
/// muted colors (comms/utility slate-ish) instead of hash noise.
fn utility_color(word: &str) -> Option<String> {
    use music_catalog::instruments::groups;
    let c = match word {
        // A bare "mic" with no instrument word before it is a vocal mic.
        "mic" | "mics" => music_catalog::instruments::vocals::LEAD,
        "talkback" | "speakers" | "daw" | "chat" | "voice" => groups::GUIDE,
        "mix" | "broadcast" | "monitor" | "monitors" => groups::REFERENCE,
        "spare" | "system" | "notifications" | "utility" => groups::STEM_SPLIT,
        _ => return None,
    };
    Some(c.to_hex_string())
}

/// Color for a condensed stereo-pair row: categorize the pair label
/// ("Guitar 1 L/R" → guitar blue) before falling back to the node.
pub fn pair_color(node_name: &str, node_label: &str, pair_label: &str) -> String {
    let base = pair_label.strip_suffix(" L/R").unwrap_or(pair_label);
    category_color(base).unwrap_or_else(|| node_color(node_name, node_label))
}

/// Resolved color for a node: user-set → instrument category (from the
/// display label) → stable name hash. Cables take the color of the
/// node they come FROM.
pub fn node_color(node_name: &str, node_label: &str) -> String {
    if let Some(c) = COLORS.read().get(node_name) {
        return c.clone();
    }
    let label = node_label_display(node_name, node_label);
    category_color(&label).unwrap_or_else(|| auto_color(node_name))
}

fn node_label_display(name: &str, label: &str) -> String {
    ALIASES
        .read()
        .get(name)
        .cloned()
        .unwrap_or_else(|| label.to_string())
}

/// Resolved color for a port: user-set port color → instrument
/// category from the channel's display name ("Kick" → drums red) →
/// the owning node's color.
pub fn port_color(node_name: &str, node_label: &str, port_name: &str) -> String {
    if let Some(c) = COLORS.read().get(&format!("{node_name}:{port_name}")) {
        return c.clone();
    }
    // Channel-number prefix stripped so "28 - Guitar 1" categorizes.
    let display = port_label(node_name, port_name);
    let digits = port_name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    let chan = if digits > 0 && digits < port_name.len() {
        port_name[port_name.len() - digits..].parse::<u64>().ok()
    } else {
        None
    };
    let display = crate::layout::strip_channel_prefix(&display, chan);
    if let Some(c) = category_color(&display) {
        return c;
    }
    node_color(node_name, node_label)
}

/// The icon lookup key for a node: the explicit `application.icon-name`
/// when present, else the app name, else the node name — the server
/// resolves any of them via icon dirs + the .desktop index ("REAPER" →
/// cockos-reaper.png). Devices that match nothing cost one cached miss.
pub fn icon_candidate(node: &patchbay_proto::PwNode) -> String {
    if !node.icon_name.is_empty() {
        return node.icon_name.clone();
    }
    let fallback = if node.app_name.is_empty() {
        &node.name
    } else {
        &node.app_name
    };
    fallback.to_lowercase().replace(' ', "-")
}

/// Fetch any application icons the graph references that we haven't
/// looked up yet. Misses are cached as empty strings server-side of
/// this map so a node without an installed icon costs one request ever.
pub fn request_missing_icons(handle: PatchbayHandle) {
    let missing: Vec<String> = {
        let icons = ICONS.peek();
        let graph = GRAPH.peek();
        let mut names: Vec<String> = graph
            .nodes
            .iter()
            .map(icon_candidate)
            .filter(|name| !name.is_empty() && !icons.contains_key(name))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    if missing.is_empty() {
        return;
    }
    {
        // Mark pending immediately so a re-render can't double-request.
        let mut icons = ICONS.write();
        for name in &missing {
            icons.insert(name.clone(), String::new());
        }
    }
    spawn(async move {
        match handle.0.icons(missing).await {
            Ok(entries) => {
                let mut icons = ICONS.write();
                for e in entries {
                    icons.insert(e.icon_name, e.data_uri);
                }
            }
            Err(e) => tracing::warn!("icon fetch failed: {e:?}"),
        }
    });
}

/// Connect (or toggle) armed outputs into the clicked input ports,
/// zipped pair-wise: a stereo pair onto a stereo pair links L→L, R→R;
/// singles behave exactly like the old one-to-one toggle.
pub fn connect_armed(handle: PatchbayHandle, inputs: &[u32]) {
    let armed = ARMED_OUTPUTS.peek().clone();
    if armed.is_empty() {
        return;
    }
    let pairs: Vec<(u32, u32)> = armed.iter().copied().zip(inputs.iter().copied()).collect();
    apply_link_toggles(handle, pairs);
}

/// Complete an active drag released on a port row (`dir`, `ports`).
/// Port-drags connect when the sides are opposite (zipped pair-wise,
/// whichever side the drag started from); node-drags are handled by
/// the header's own mouseup.
pub fn complete_drag_on_ports(handle: PatchbayHandle, dir: PortDirection, ports: &[u32]) {
    let Some(drag) = DRAG.peek().clone() else {
        return;
    };
    let DragSource::Ports(from_dir, from_ports) = drag.source else {
        return;
    };
    if from_dir == dir {
        return;
    }
    let pairs: Vec<(u32, u32)> = match from_dir {
        PortDirection::Output => from_ports
            .iter()
            .copied()
            .zip(ports.iter().copied())
            .collect(),
        PortDirection::Input => ports
            .iter()
            .copied()
            .zip(from_ports.iter().copied())
            .collect(),
    };
    apply_link_toggles(handle, pairs);
}

/// Bulk 1:1 connect for a node-header drop (numeric-suffix pairing on
/// the server). Result lands in the preset/apply report line.
pub fn connect_nodes_bulk(handle: PatchbayHandle, from: String, to: String) {
    spawn(async move {
        match handle.0.connect_one_to_one(from.clone(), to.clone()).await {
            Ok(n) => {
                *LAST_REPORT.write() = Some((
                    format!("{from} → {to}"),
                    ApplyReport {
                        created: n,
                        ..ApplyReport::default()
                    },
                ));
            }
            Err(e) => tracing::warn!("bulk connect failed: {e:?}"),
        }
    });
}

/// Snapshot the current canvas state under a name.
pub fn capture_view(name: String) -> patchbay_proto::CanvasView {
    let (pan_x, pan_y) = *PAN.peek();
    patchbay_proto::CanvasView {
        name,
        zoom: *ZOOM.peek(),
        pan_x,
        pan_y,
        collapsed_cols: COLLAPSED_COLS.peek().to_vec(),
        hide_unconnected: *HIDE_UNCONNECTED.peek(),
        hide_monitors: *HIDE_MONITORS.peek(),
    }
}

/// Restore a saved canvas view.
pub fn apply_view(view: &patchbay_proto::CanvasView) {
    *ZOOM.write() = view.zoom.clamp(0.15, 3.0);
    *PAN.write() = (view.pan_x, view.pan_y);
    let mut cols = [false; 4];
    for (i, c) in view.collapsed_cols.iter().take(4).enumerate() {
        cols[i] = *c;
    }
    *COLLAPSED_COLS.write() = cols;
    *HIDE_UNCONNECTED.write() = view.hide_unconnected;
    *HIDE_MONITORS.write() = view.hide_monitors;
}

/// Display name for a node (alias wins).
pub fn node_label(name: &str, label: &str) -> String {
    ALIASES
        .read()
        .get(name)
        .cloned()
        .unwrap_or_else(|| label.to_string())
}

/// Display name for a port (alias wins).
pub fn port_label(node_name: &str, port_name: &str) -> String {
    ALIASES
        .read()
        .get(&format!("{node_name}:{port_name}"))
        .cloned()
        .unwrap_or_else(|| port_name.to_string())
}
