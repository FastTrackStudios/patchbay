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

/// Port direction as PipeWire reports it (`port.direction`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
pub enum PortDirection {
    Input,
    Output,
}

/// A node's live processing state (`info.state` from `pw-dump`).
///
/// This is the closest thing PipeWire gives to "is anything happening
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
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "idle" => Self::Idle,
            "suspended" => Self::Suspended,
            _ => Self::Unknown,
        }
    }
}

/// A PipeWire node (device, stream, virtual sink/source, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct PwNode {
    /// PipeWire global id (unstable across restarts — never persist).
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
    /// PipeWire global id.
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
    /// PipeWire global id.
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
    /// The engine's PipeWire connection dropped (daemon restart) —
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

// SelfRef compatibility: GraphEvent has no lifetime parameters, so Ref<'a> = Self.
#[allow(unsafe_code)]
unsafe impl vox_types::Reborrow for GraphEvent {
    type Ref<'a> = GraphEvent;
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
/// `to`'s input port whenever BOTH resolve in the live graph. Applied
/// idempotently — it only ever *creates* the missing link, never tears
/// down anything else — on graph settle and on demand. Because the
/// endpoints are addressed by alias, a route keeps working when the
/// underlying channel numbers move.
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
/// (`target = "node.name:port.name"`). Pure presentation — PipeWire
/// names are never rewritten, so nothing else on the system breaks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct AliasEntry {
    pub target: String,
    pub alias: String,
}

// ─── Colors (cable/port identity) ───────────────────────────────────────

/// User-set color for a node (`target = node.name`) or a port
/// (`target = "node.name:port.name"`). CSS color string (`#rrggbb`).
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
/// PipeWire restarts even though `object.linger` alone doesn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct VirtualSink {
    /// Display name; the node name is derived (`patchbay.<slug>`).
    pub name: String,
    /// Channel count: 1 = mono, 2 = stereo (FL/FR), n = AUX0..n-1.
    pub channels: u32,
}

/// Node name for a virtual sink ("Stems Bus" → `patchbay.stems_bus`) —
/// shared by the engine (creation) and UIs (live-state matching).
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

/// Graph clock defaults, materialized as a PipeWire drop-in — the
/// runtime-editable version of the flake's `50-quantum.conf`. Zero
/// fields mean "not set here" (fall through to the flake/system
/// config). Applied on PipeWire restart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct ClockDefaults {
    pub quantum: u32,
    pub min_quantum: u32,
    pub max_quantum: u32,
}

/// Per-app latency rule, materialized as a WirePlumber drop-in.
///
/// While a matching node is running, the graph runs at `quantum`
/// (`force` = `node.force-quantum`, a hard pin; otherwise
/// `node.latency`, a request the driver honors as the minimum among
/// running nodes). When the app closes, the graph returns to its idle
/// default — that's how REAPER runs at 64 while everything
/// non-critical idles at 1024. Applied when the node is created:
/// restart the app or WirePlumber after changing rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct LatencyRule {
    /// `node.name` to match; prefix with `~` for a regex
    /// (WirePlumber match syntax).
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
    /// Short display label ("PipeWire", "PTP clock (statime)").
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
/// channel NAMES plus its live subscriptions. Saved so the studio's
/// Dante patch (Galaxy32 → Inferno, etc.) survives power-cycles and can
/// be re-applied with one command, and so channel names are available
/// offline for name-addressed routing. IP / ARC port are rediscovered on
/// each scan, so they aren't stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteDeviceConfig {
    pub name: String,
    pub tx: Vec<DanteChannel>,
    pub rx: Vec<DanteChannel>,
    pub subscriptions: Vec<DanteSubscription>,
}

impl DanteDeviceConfig {
    /// Drop the transient live fields (ip / arc_port / unreachable) from
    /// a scanned device to get the persistable form.
    pub fn from_device(d: &DanteDevice) -> Self {
        Self {
            name: d.name.clone(),
            tx: d.tx.clone(),
            rx: d.rx.clone(),
            subscriptions: d.subscriptions.clone(),
        }
    }
}

/// State of the `dante.target` AoIP stack on this host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct DanteStatus {
    /// Whether `dante.target` exists on this host at all.
    pub installed: bool,
    /// Whether the target is active.
    pub active: bool,
    pub units: Vec<UnitStatus>,
}
