//! Graph, preset, and status types shared across the wire.

use facet::Facet;
use serde::{Deserialize, Serialize};

// ─── Graph model ────────────────────────────────────────────────────────

/// What kind of media flows through a node/port. Derived from
/// `media.class` the way helvum does it (substring match); `Other`
/// covers control/metadata nodes we still want visible.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
pub enum MediaKind {
    Audio,
    Video,
    Midi,
    Other,
}

/// Port direction as `PipeWire` reports it (`port.direction`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
pub enum PortDirection {
    Input,
    Output,
}

/// A node's live processing state (`info.state` from `pw-dump`).
///
/// This is the closest thing `PipeWire` gives to "is anything happening
/// here" without tapping the audio itself: `Running` = the node is
/// actively cycling in a driven graph; `Idle` = negotiated but not
/// being driven (a paused stream, a source with nothing to send);
/// `Suspended` = closed / no clients. NOTE: hardware devices pinned
/// always-running read `Running` even during digital silence — this
/// tracks *activity*, not signal presence.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, Facet)]
pub enum NodeState {
    /// Not yet polled, or an `info.state` value we don't classify.
    #[default]
    Unknown,
    Suspended,
    Idle,
    Running,
}

impl NodeState {
    /// Parse a `pw-dump` `info.state` string.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "idle" => Self::Idle,
            "suspended" => Self::Suspended,
            _ => Self::Unknown,
        }
    }
}

/// A `PipeWire` node (device, stream, virtual sink/source, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct PwNode {
    /// `PipeWire` global id (unstable across restarts — never persist).
    pub id: u32,
    /// `node.name` — the stable identity used by presets/aliases.
    pub name: String,
    /// Display label: `node.nick` → `node.description` → `node.name`.
    pub label: String,
    /// Raw `media.class` (e.g. `Audio/Sink`, `Stream/Output/Audio`).
    pub media_class: String,
    pub media_kind: MediaKind,
    /// `application.name` when the node belongs to an app.
    pub app_name: String,
    /// The node's latency request (`node.latency`, e.g. `"64/48000"`),
    /// empty when unset.
    pub latency: String,
    /// `application.icon-name` (freedesktop icon id), empty when unset.
    pub icon_name: String,
    /// `node.group` — links related nodes (a loopback's sink half and
    /// forwarder stream share one). Only present via bound node info
    /// (registry globals omit it).
    pub group: String,
    /// This node is a patchbay-created virtual sink (`patchbay.virtual`
    /// prop) — the only nodes the UI may destroy.
    pub virtual_sink: bool,
    /// Live processing state (`running`/`idle`/`suspended`), polled
    /// out-of-band via `pw-dump` — registry globals don't carry it.
    /// `#[facet(default)]` so a client built with this field can still
    /// decode snapshots from an older engine that never sends it (it
    /// reads back as `Unknown`).
    #[facet(default)]
    pub state: NodeState,
}

/// A port on a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct PwPort {
    /// `PipeWire` global id.
    pub id: u32,
    /// Owning node's global id.
    pub node_id: u32,
    /// `port.name` (e.g. `playback_97`, `capture_FL`) — stable identity.
    pub name: String,
    pub direction: PortDirection,
    pub media_kind: MediaKind,
}

/// A link between an output port and an input port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct PwLink {
    /// `PipeWire` global id.
    pub id: u32,
    pub output_node: u32,
    pub output_port: u32,
    pub input_node: u32,
    pub input_port: u32,
    /// Whether the link is in `Active` state (data flowing).
    pub active: bool,
}

/// Complete graph snapshot — what a client renders from on connect;
/// afterwards it applies [`GraphEvent`]s incrementally.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct GraphSnapshot {
    pub nodes: Vec<PwNode>,
    pub ports: Vec<PwPort>,
    pub links: Vec<PwLink>,
}

/// Incremental graph change, streamed via `#[subscribe]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
#[repr(u8)]
pub enum GraphEvent {
    /// The engine's `PipeWire` connection dropped (daemon restart) —
    /// clients must clear their mirror; the reconnect re-announces
    /// everything with fresh ids.
    Reset,
    NodeAdded(PwNode),
    NodeRemoved {
        id: u32,
    },
    /// A node's live processing state changed (`running`/`idle`/…).
    NodeStateChanged {
        id: u32,
        state: NodeState,
    },
    PortAdded(PwPort),
    PortRemoved {
        id: u32,
        node_id: u32,
    },
    LinkAdded(PwLink),
    LinkStateChanged {
        id: u32,
        active: bool,
    },
    LinkRemoved {
        id: u32,
    },
}

// SelfRef compatibility: `GraphEvent` has no lifetime parameters, so
// `Ref<'a>` is just `Self`.
#[allow(unsafe_code)]
unsafe impl vox_types::Reborrow for GraphEvent {
    type Ref<'a> = Self;
}

// ─── Presets (connection memory) ────────────────────────────────────────

/// One remembered connection, keyed by stable names (never global ids).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
pub struct PresetLink {
    pub output_node: String,
    pub output_port: String,
    pub input_node: String,
    pub input_port: String,
}

/// A named routing preset — a saved set of connections that can be
/// re-applied later (missing endpoints are reported, not fatal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct RoutingPreset {
    pub name: String,
    pub description: String,
    pub links: Vec<PresetLink>,
}

/// What happened when a preset was applied.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct ApplyReport {
    /// Links newly created.
    pub created: u32,
    /// Links that already existed.
    pub existing: u32,
    /// Links whose endpoints aren't currently in the graph.
    pub missing: Vec<PresetLink>,
    /// Links destroyed (exclusive mode only).
    pub destroyed: u32,
}

// ─── Named routes (explicit auto-connect) ───────────────────────────────

/// One endpoint of a [`NamedRoute`], addressed by SEMANTIC name and
/// resolved against the live graph at apply time (so it survives the
/// channel being renumbered).
///
/// `node` narrows which node to look in — a `node.name` or a node alias,
/// empty = any node. `port` is a port's alias or raw `port.name`, matched
/// after normalization: the `"N - "` channel-number prefix and a trailing
/// `[DSP]` are stripped and the compare is case-insensitive, so
/// `"Engineer TB"` matches the live alias `"81 - Engineer TB [DSP]"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct RouteEndpoint {
    pub node: String,
    pub port: String,
}

/// An explicit auto-connect rule: keep `from`'s output port linked to
/// `to`'s input port whenever BOTH resolve in the live graph.
///
/// Applied idempotently — it only ever *creates* the missing link,
/// never tears down anything else — on graph settle and on demand.
/// Because the endpoints are addressed by alias, a route keeps working
/// when the underlying channel numbers move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct NamedRoute {
    /// Unique label ("Engineer TB → REAPER"); upsert key.
    pub name: String,
    /// Output side (the source).
    pub from: RouteEndpoint,
    /// Input side (the destination).
    pub to: RouteEndpoint,
    /// Disabled routes persist but are skipped by apply.
    pub enabled: bool,
}

// ─── Aliases (pretty names) ─────────────────────────────────────────────

/// Display alias for a node (`target = node.name`) or a port
/// (`target = "node.name:port.name"`). Pure presentation — `PipeWire`
/// names are never rewritten, so nothing else on the system breaks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct AliasEntry {
    pub target: String,
    pub alias: String,
}

// ─── Colors (cable/port identity) ───────────────────────────────────────

/// User-set color for a node (`target = node.name`) or a port
/// (`target = "node.name:port.name"`), as a CSS color (`#rrggbb`).
///
/// Cables inherit: output-port color → output-node color → media-kind
/// default. Pure presentation, persisted alongside aliases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct ColorEntry {
    pub target: String,
    pub color: String,
}

/// A resolved application icon: the freedesktop icon name plus its
/// image as a `data:` URI (so remotes render it without filesystem
/// access to this host's icon themes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct IconEntry {
    pub icon_name: String,
    pub data_uri: String,
}

// ─── Virtual sinks (named buses) ────────────────────────────────────────

/// A patchbay-owned null-audio sink (a named bus): persisted in config
/// and re-created whenever the engine (re)connects, so buses survive
/// `PipeWire` restarts even though `object.linger` alone doesn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct VirtualSink {
    /// Display name; the node name is derived (`patchbay.<slug>`).
    pub name: String,
    /// Channel count: 1 = mono, 2 = stereo (FL/FR), n = AUX0..n-1.
    pub channels: u32,
    /// Also expose this bus as a first-class CAPTURE source, so OBS and
    /// friends list it under "Audio Input Capture" by name.
    ///
    /// The bus already has a `.monitor`, but recorders either hide
    /// monitors or bury them as "Monitor of …"; a companion virtual
    /// source shows up as a real device called after the bus.
    ///
    /// Defaults to false so an existing config (and an existing rig)
    /// behaves exactly as before.
    #[serde(default)]
    #[facet(default)]
    pub capturable: bool,
}

/// Source name for a capturable bus (`patchbay.stems_bus` →
/// `patchbay.stems_bus-src`).
///
/// Shared by the engine (creation) and any UI that wants to tell the
/// user what to pick in OBS.
#[must_use]
pub fn capture_source_name(sink_node_name: &str) -> String {
    format!("{sink_node_name}-src")
}

// ─── Port-name numbering ────────────────────────────────────────────────

/// Split a port name into its non-numeric prefix and trailing channel
/// number: `playback_97` → `("playback_", 97)`.
///
/// `None` when there is no trailing digit run, or when the name is
/// *entirely* digits (that is a bare number, not a numbered channel).
/// This is the one definition of "which channel is this port" — the
/// engine (chanmap import/export, 1:1 bulk wiring) and every UI
/// (grouping, stereo pairing, inspector ordering) share it.
#[must_use]
pub fn split_port_number(port_name: &str) -> Option<(&str, u64)> {
    let digits = port_name
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .count();
    if digits == 0 || digits == port_name.len() {
        return None;
    }
    let split = port_name.len().checked_sub(digits)?;
    let (prefix, num) = port_name.split_at(split);
    num.parse().ok().map(|n| (prefix, n))
}

/// The 1-based channel a port belongs to, from its numeric suffix
/// (`playback_97` → `97`). See [`split_port_number`].
#[must_use]
pub fn channel_of_port(port_name: &str) -> Option<u32> {
    split_port_number(port_name).and_then(|(_, n)| u32::try_from(n).ok())
}

/// Node name for a virtual sink ("Stems Bus" → `patchbay.stems_bus`) —
/// shared by the engine (creation) and UIs (live-state matching).
#[must_use]
pub fn sink_node_name(display: &str) -> String {
    let slug: String = display
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("patchbay.{slug}")
}

// ─── Metering ───────────────────────────────────────────────────────────

/// Live peak level for one node, one entry per tapped channel (0.0–1.0).
///
/// `PipeWire`'s registry carries no level, so this comes from recording
/// the node's monitor source and measuring it. Metering costs a process
/// per node, so it is opt-in: a client calls `set_metered` with the
/// nodes it is currently showing and polls `meters` while they're on
/// screen. Nothing is tapped until asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MeterLevel {
    /// `node.name` of the metered node.
    pub node_name: String,
    /// Peak per channel since the last read, 0.0–1.0. Empty when the
    /// tap has not produced data yet.
    pub peak: Vec<f32>,
}

// ─── Application streams ────────────────────────────────────────────────

/// A running application's audio stream, as `pipewire-pulse` sees it.
///
/// Port links wire devices; they cannot move a playing app between
/// sinks. That is a sink-input operation, and this is the handle for it
/// — "send Firefox to the Stems bus" without touching Firefox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct AppStream {
    /// Pulse sink-input index — the handle `move_app_stream` takes.
    /// Unstable across restarts; never persist it.
    pub index: u32,
    /// Display name (`application.name`, falling back to the stream or
    /// node name).
    pub app_name: String,
    /// `application.process.binary`, empty when unknown.
    pub binary: String,
    /// What the app calls this stream (`media.name`).
    pub media_name: String,
    /// Index of the sink it currently plays into.
    pub sink_index: u32,
    /// `node.name` of that sink.
    pub sink_name: String,
    /// The stream is paused. Still movable — it will land on the new
    /// sink when it resumes.
    pub corked: bool,
}

// ─── Saved canvas views ─────────────────────────────────────────────────

/// A saved graph-canvas view: pan/zoom/collapse state under a name, so
/// "FOH" / "Broadcast" layouts are one click away on any client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct CanvasView {
    pub name: String,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    /// Per-column collapse (Inputs | Applications | Groups | Outputs).
    pub collapsed_cols: Vec<bool>,
    pub hide_unconnected: bool,
    pub hide_monitors: bool,
}

// ─── Clock / latency ────────────────────────────────────────────────────

/// Graph clock defaults, materialized as a `PipeWire` drop-in.
///
/// The runtime-editable version of the flake's `50-quantum.conf`. Zero
/// fields mean "not set here" (fall through to the flake/system
/// config). Applied on `PipeWire` restart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct ClockDefaults {
    pub quantum: u32,
    pub min_quantum: u32,
    pub max_quantum: u32,
}

/// Per-app latency rule, materialized as a `WirePlumber` drop-in.
///
/// While a matching node is running, the graph runs at `quantum`
/// (`force` = `node.force-quantum`, a hard pin; otherwise
/// `node.latency`, a request the driver honors as the minimum among
/// running nodes). When the app closes, the graph returns to its idle
/// default — that's how REAPER runs at 64 while everything
/// non-critical idles at 1024. Applied when the node is created:
/// restart the app or `WirePlumber` after changing rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct LatencyRule {
    /// `node.name` to match; prefix with `~` for a regex
    /// (`WirePlumber` match syntax).
    pub pattern: String,
    /// Quantum in frames (32…2048).
    pub quantum: u32,
    /// Hard pin (`node.force-quantum`) instead of a request.
    pub force: bool,
}

/// Live graph clock settings (from `pw-metadata -n settings`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct ClockInfo {
    pub rate: u32,
    pub quantum: u32,
    /// Forced quantum, `0` when automatic.
    pub force_quantum: u32,
    /// Forced rate, `0` when automatic.
    pub force_rate: u32,
    pub min_quantum: u32,
    pub max_quantum: u32,
}

// ─── Dante / Inferno stack ──────────────────────────────────────────────

/// One systemd unit's state within the Dante stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct UnitStatus {
    pub unit: String,
    /// `active` / `inactive` / `failed` / `activating` / …
    pub state: String,
}

/// One managed audio-stack service (systemd user unit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct ServiceStatus {
    /// Unit name (`pipewire.service`, `statime-inferno.service`, …).
    pub unit: String,
    /// Short display label ("`PipeWire`", "PTP clock (statime)").
    pub label: String,
    /// `ActiveState`: active / inactive / failed / activating / …
    pub state: String,
    /// `SubState`: running / dead / failed / start-pre / …
    pub sub_state: String,
    /// Whether the unit exists on this host at all.
    pub present: bool,
}

/// Action on a managed service.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

// ─── Dante network (ARC control via inferno-net) ────────────────────────

/// A channel on a Dante device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteChannel {
    /// 1-based channel number.
    pub number: u32,
    pub name: String,
}

/// One RX channel's subscription state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteSubscription {
    /// RX channel number on the owning device.
    pub rx_channel: u32,
    /// Subscribed-to TX channel name (empty = unsubscribed).
    pub tx_channel: String,
    /// Subscribed-to TX device name.
    pub tx_device: String,
    /// Raw ARC subscription status (`1` = healthy).
    pub status: u32,
}

/// A Dante device with its channel lists + live subscriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteDevice {
    pub name: String,
    pub ip: String,
    pub arc_port: u16,
    pub tx: Vec<DanteChannel>,
    pub rx: Vec<DanteChannel>,
    pub subscriptions: Vec<DanteSubscription>,
    /// Channel query failed (device visible on mDNS but ARC timed out).
    pub unreachable: bool,
}

/// A persisted snapshot of one Dante device's routing: its TX/RX
/// channel NAMES plus its live subscriptions.
///
/// Saved so the studio's Dante patch (Galaxy32 → Inferno, etc.)
/// survives power-cycles and can be re-applied with one command, and so
/// channel names are available offline for name-addressed routing. IP /
/// ARC port are rediscovered on each scan, so they aren't stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteDeviceConfig {
    pub name: String,
    pub tx: Vec<DanteChannel>,
    pub rx: Vec<DanteChannel>,
    pub subscriptions: Vec<DanteSubscription>,
}

impl DanteDeviceConfig {
    /// Drop the transient live fields (`ip` / `arc_port` / `unreachable`) from
    /// a scanned device to get the persistable form.
    #[must_use]
    pub fn from_device(d: &DanteDevice) -> Self {
        Self {
            name: d.name.clone(),
            tx: d.tx.clone(),
            rx: d.rx.clone(),
            subscriptions: d.subscriptions.clone(),
        }
    }
}

/// State of the `dante.target` `AoIP` stack on this host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteStatus {
    /// Whether `dante.target` exists on this host at all.
    pub installed: bool,
    /// Whether the target is active.
    pub active: bool,
    pub units: Vec<UnitStatus>,
}

// ─── Host privacy permissions (macOS) ───────────────────────────────────

/// Privacy permissions of the process serving the engine. On macOS they
/// belong to `Patchbay.app` (when the engine runs inside it) or to the
/// terminal that launched a headless `patchbay serve`.
///
/// Each state is one of `granted`, `denied`, `restricted`,
/// `not_determined`, `unknown`, or `not_applicable` (not macOS).
/// `local_network` can only be probed, not queried, so it is `granted`
/// or `blocked` (denied or not answered yet), or `unknown`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct PermissionsStatus {
    /// `macos`, `linux`, …
    pub platform: String,
    /// The engine runs inside `Patchbay.app`, which owns the grants.
    pub bundled: bool,
    /// System Audio Recording (Core Audio process taps).
    pub system_audio_recording: String,
    /// Microphone / audio-interface input.
    pub microphone: String,
    /// Local Network (Dante, Yamaha TF, … discovery on the LAN).
    pub local_network: String,
    /// A request flow (prompts / alert) is running right now.
    pub requesting: bool,
    /// Unix seconds of the last full check, `0` if never.
    pub checked_at: u64,
    /// What to do next, for humans and agents.
    pub note: String,
}
