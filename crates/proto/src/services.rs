//! Service trait for the patchbay domain.
//!
//! One service covers the whole surface: graph snapshot + live events,
//! link create/destroy, presets, aliases, clock control, and the Dante
//! stack. Served by the `patchbay` engine crate; consumed in-process by
//! the desktop app and over ws/iroh by remotes.

use facet::Facet;
use serde::{Deserialize, Serialize};
use vox::Tx;

use crate::types::{
    AliasEntry, ApplyReport, CanvasView, ClockDefaults, ClockInfo, ColorEntry, DanteDevice,
    DanteDeviceConfig, DanteStatus, GraphEvent, GraphSnapshot, IconEntry, LatencyRule, NamedRoute,
    RoutingPreset, ServiceAction, ServiceStatus, VirtualSink,
};

/// Typed error for patchbay service boundaries.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet, thiserror::Error)]
pub enum PatchbayError {
    /// Entity not found (port, link, preset, …).
    #[error("{entity} not found: {id}")]
    NotFound { entity: String, id: String },

    /// The PipeWire engine isn't running (no daemon, engine thread died).
    #[error("pipewire engine unavailable: {0}")]
    EngineUnavailable(String),

    /// Catch-all for unexpected failures.
    #[error("internal error: {0}")]
    Internal(String),
}

impl PatchbayError {
    pub fn not_found(entity: impl Into<String>, id: impl ToString) -> Self {
        Self::NotFound {
            entity: entity.into(),
            id: id.to_string(),
        }
    }
}

impl From<String> for PatchbayError {
    fn from(s: String) -> Self {
        Self::Internal(s)
    }
}

pub mod patchbay_service {
    use super::*;

    #[architect::rpc]
    pub trait PatchbayService {
        // ── Graph ────────────────────────────────────────────────────

        /// Complete current graph. Render from this, then apply
        /// `graph_events` incrementally.
        async fn graph(&self) -> Result<GraphSnapshot, PatchbayError>;

        /// Create a link between an output port and an input port
        /// (global ids). Created with `object.linger` so it survives
        /// the app exiting.
        async fn create_link(&self, output_port: u32, input_port: u32)
        -> Result<(), PatchbayError>;

        /// Destroy a link by global id.
        async fn destroy_link(&self, link_id: u32) -> Result<(), PatchbayError>;

        /// Bulk 1:1 wiring: link `output_node`'s output ports to
        /// `input_node`'s input ports paired by numeric suffix
        /// (`out7`/`playback_7` → channel 7). The direct-path tool —
        /// plain links add zero latency, unlike loopback sinks.
        /// Returns the number of links created.
        async fn connect_one_to_one(
            &self,
            output_node: String,
            input_node: String,
        ) -> Result<u32, PatchbayError>;

        /// Destroy every link running from `output_node` into
        /// `input_node`. Returns the number destroyed.
        async fn disconnect_nodes(
            &self,
            output_node: String,
            input_node: String,
        ) -> Result<u32, PatchbayError>;

        /// Every graph change, as it happens.
        #[subscribe]
        fn graph_events(&self) -> GraphEvent;

        // ── Presets (connection memory) ──────────────────────────────

        async fn list_presets(&self) -> Result<Vec<RoutingPreset>, PatchbayError>;

        /// Snapshot the current connections into a named preset
        /// (overwrites an existing preset of the same name).
        async fn save_preset(
            &self,
            name: String,
            description: String,
        ) -> Result<RoutingPreset, PatchbayError>;

        /// Re-apply a preset: create every remembered link whose
        /// endpoints exist. `exclusive` also destroys current links
        /// that are NOT in the preset (full-state restore).
        async fn apply_preset(
            &self,
            name: String,
            exclusive: bool,
        ) -> Result<ApplyReport, PatchbayError>;

        async fn delete_preset(&self, name: String) -> Result<(), PatchbayError>;

        // ── Named routes (explicit auto-connect) ─────────────────────

        async fn routes(&self) -> Result<Vec<NamedRoute>, PatchbayError>;

        /// Add or replace a named route (upsert by `route.name`).
        async fn set_route(&self, route: NamedRoute) -> Result<(), PatchbayError>;

        async fn delete_route(&self, name: String) -> Result<(), PatchbayError>;

        /// Resolve + apply every enabled route against the live graph
        /// now (idempotent — creates only missing links). Returns the
        /// number of links created. Routes also apply automatically
        /// whenever the graph settles after a node/port change.
        async fn apply_routes(&self) -> Result<u32, PatchbayError>;

        // ── Aliases (pretty names) ───────────────────────────────────

        async fn aliases(&self) -> Result<Vec<AliasEntry>, PatchbayError>;

        /// Set a display alias for `"node.name"` or `"node.name:port.name"`.
        /// An empty alias clears the entry.
        async fn set_alias(&self, target: String, alias: String) -> Result<(), PatchbayError>;

        /// Pull channel names from a REAPER ChanMap (`nameN=Label`,
        /// 0-based → channel N+1) into port aliases on `node`: every
        /// port whose numeric suffix is N+1 (playback/capture/monitor)
        /// gets the label. Empty `path` = the host's default chanmap
        /// (`~/.fasttrackstudio/Reaper/ChanMaps/<hostname>.ReaperChanMap`).
        /// Returns the number of aliases written.
        async fn import_chanmap(&self, node: String, path: String) -> Result<u32, PatchbayError>;

        /// Push `node`'s port aliases back into a REAPER ChanMap's
        /// `nameN=` lines (other lines preserved; file created if
        /// missing). Empty `path` = the host default. Returns the
        /// number of channel names written.
        async fn export_chanmap(&self, node: String, path: String) -> Result<u32, PatchbayError>;

        /// Alias `node`'s ports from a live Dante device's channel names
        /// over ARC — the network-sourced sibling of `import_chanmap`.
        /// `direction` is `"rx"` (received channels → name the
        /// capture/input proxy, e.g. `daw_inputs`) or `"tx"` (transmitted
        /// channels → name the playback/output proxy, e.g. `daw`). Ports
        /// are matched to channels by numeric suffix, so device channel
        /// 57 ("Engineer Vocal [DSP]") names `capture_57` — this only
        /// lines up when the device's channel numbering matches the
        /// node's port numbering 1:1.
        ///
        /// `device` must be a name from `dante_network()`. NOTE: the
        /// local Inferno virtual soundcard does not currently advertise
        /// itself in mDNS discovery (it appears only as a subscription
        /// *source*, e.g. `THEBATTLESHIP`), so naming a proxy from the
        /// local Inferno's own labels is not yet possible here — target
        /// a discovered console/interface whose numbering matches. Empty
        /// `device` falls back to the first discovered device. Returns
        /// the number of aliases written.
        async fn import_inferno_names(
            &self,
            node: String,
            device: String,
            direction: String,
        ) -> Result<u32, PatchbayError>;

        // ── Virtual sinks (named buses) ──────────────────────────────

        /// The persisted virtual sinks (whether or not currently live).
        async fn virtual_sinks(&self) -> Result<Vec<VirtualSink>, PatchbayError>;

        /// Persist a virtual sink and create it in the live graph.
        /// Re-created automatically on engine (re)connect.
        async fn add_virtual_sink(&self, sink: VirtualSink) -> Result<(), PatchbayError>;

        /// Remove a virtual sink from config and destroy its live node
        /// (if present). Only nodes carrying `patchbay.virtual` are
        /// ever destroyed.
        async fn remove_virtual_sink(&self, name: String) -> Result<(), PatchbayError>;

        // ── Saved canvas views ───────────────────────────────────────

        async fn views(&self) -> Result<Vec<CanvasView>, PatchbayError>;

        /// Save (upsert) a canvas view.
        async fn save_view(&self, view: CanvasView) -> Result<(), PatchbayError>;

        async fn delete_view(&self, name: String) -> Result<(), PatchbayError>;

        // ── Colors (cable/port identity) ─────────────────────────────

        async fn colors(&self) -> Result<Vec<ColorEntry>, PatchbayError>;

        /// Set a color for `"node.name"` or `"node.name:port.name"`.
        /// An empty color clears the entry.
        async fn set_color(&self, target: String, color: String) -> Result<(), PatchbayError>;

        // ── Icons ────────────────────────────────────────────────────

        /// Resolve the given freedesktop icon names against this host's
        /// icon themes and return each hit as a `data:` URI. Unknown
        /// names are simply absent from the result.
        async fn icons(&self, names: Vec<String>) -> Result<Vec<IconEntry>, PatchbayError>;

        // ── Clock ────────────────────────────────────────────────────

        async fn clock(&self) -> Result<ClockInfo, PatchbayError>;

        /// Force the graph quantum (frames); `0` returns to automatic.
        async fn force_quantum(&self, frames: u32) -> Result<(), PatchbayError>;

        /// The patchbay-owned clock-defaults drop-in (zeros = none).
        async fn clock_defaults(&self) -> Result<ClockDefaults, PatchbayError>;

        /// Write (all-zero = delete) the clock-defaults drop-in. Wins
        /// over the flake's 50-quantum.conf by filename ordering;
        /// applied on PipeWire restart (services panel).
        async fn set_clock_defaults(&self, defaults: ClockDefaults) -> Result<(), PatchbayError>;

        // ── Per-app latency rules ────────────────────────────────────

        async fn latency_rules(&self) -> Result<Vec<LatencyRule>, PatchbayError>;

        /// Add or replace the rule for `rule.pattern` and rewrite the
        /// WirePlumber drop-in. Takes effect when a matching node is
        /// (re)created — restart the app or WirePlumber.
        async fn set_latency_rule(&self, rule: LatencyRule) -> Result<(), PatchbayError>;

        /// Remove the rule matching `pattern` and rewrite the drop-in.
        async fn remove_latency_rule(&self, pattern: String) -> Result<(), PatchbayError>;

        // ── Dante / Inferno stack ────────────────────────────────────

        async fn dante_status(&self) -> Result<DanteStatus, PatchbayError>;

        /// Bring `dante.target` up or down (systemd user unit).
        async fn set_dante(&self, on: bool) -> Result<(), PatchbayError>;

        // ── Managed services (rig health) ────────────────────────────

        /// Status of every managed audio-stack unit (PipeWire,
        /// WirePlumber, statime, Inferno nodes, routing links, …).
        async fn services(&self) -> Result<Vec<ServiceStatus>, PatchbayError>;

        /// Start/stop/restart one managed unit. Only whitelisted units
        /// are accepted — this is not a general systemctl proxy.
        async fn service_action(
            &self,
            unit: String,
            action: ServiceAction,
        ) -> Result<(), PatchbayError>;

        // ── Dante network (ARC routing grid) ─────────────────────────

        /// Discover Dante devices (mDNS) and fetch each one's TX/RX
        /// channel lists + live subscriptions over ARC. Slowish
        /// (seconds — network round-trips); call on demand.
        async fn dante_network(&self) -> Result<Vec<DanteDevice>, PatchbayError>;

        /// Subscribe `rx_device`'s channel `rx_channel` to
        /// `tx_device`'s channel named `tx_channel`.
        async fn dante_subscribe(
            &self,
            rx_device: String,
            rx_channel: u32,
            tx_device: String,
            tx_channel: String,
        ) -> Result<(), PatchbayError>;

        /// Clear the subscription on `rx_device`'s channel `rx_channel`.
        async fn dante_unsubscribe(
            &self,
            rx_device: String,
            rx_channel: u32,
        ) -> Result<(), PatchbayError>;

        // ── Dante config (persisted routing) ─────────────────────────

        /// The saved Dante routing snapshot (channel names +
        /// subscriptions of every device), from config.
        async fn dante_config(&self) -> Result<Vec<DanteDeviceConfig>, PatchbayError>;

        /// Scan the live Dante network (mDNS + ARC) and persist a
        /// snapshot of every reachable device — its TX/RX channel names
        /// and subscriptions. Returns the number of devices saved.
        async fn save_dante_config(&self) -> Result<u32, PatchbayError>;

        /// Re-apply the saved subscriptions to the live network:
        /// non-destructive (only sets subscriptions that differ from
        /// what's live, never clears anything). Returns the number of
        /// subscriptions (re)applied. This writes to the Dante hardware
        /// over ARC — an explicit, on-demand restore, never automatic.
        async fn apply_dante_config(&self) -> Result<u32, PatchbayError>;
    }
}

pub use patchbay_service::{
    PatchbayService, PatchbayServiceClient, Service as PatchbayServiceLayer,
    layer as patchbay_service_layer, patchbay_service_rpc_service_descriptor,
    patchbay_service_rpc_service_descriptor as patchbay_service_service_descriptor,
    serve as serve_patchbay_service,
};
pub use patchbay_service::{
    PatchbayServiceRpcDispatcher as PatchbayServiceDispatcher, PatchbayServiceStreamClient,
    patchbay_service_stream_service_descriptor, stream_serve as patchbay_service_stream_serve,
};
