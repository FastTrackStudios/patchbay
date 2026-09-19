//! Service trait for the patchbay domain.
//!
//! One service covers the whole surface: graph snapshot + live events,
//! link create/destroy, presets, aliases, clock control, and the Dante
//! stack. Served by the `patchbay` engine crate; consumed in-process by
//! the desktop app and over ws/iroh by remotes.

use facet::Facet;
use serde::{Deserialize, Serialize};

use crate::devices::{
    DeviceChannel, DeviceCrosspoint, DeviceEventWire, DeviceParamValue, DeviceRestoreReport,
    DeviceSnapshotInfo, DeviceSummary, DeviceView, ParamView,
};
use crate::mixes::{
    AggregateView, AppMeter, HostOverview, HostTargets, MixConfig, MixMeters, MixView,
    VirtualDeviceView, VirtualDevicesStatus,
};
use crate::types::{
    AliasEntry, AppStream, ApplyReport, CanvasView, ClockDefaults, ClockInfo, ColorEntry,
    DanteDevice, DanteDeviceConfig, DanteStatus, GraphEvent, GraphSnapshot, IconEntry, LatencyRule,
    ListenAddress, MeterLevel, NamedRoute, PermissionsStatus, RoutingPreset, ServiceAction,
    ServiceStatus, VirtualSink,
};

// `Facet`'s derive for a `#[repr(C)]` enum generates discriminant
// arithmetic we don't control, and the lint attributes to the derive
// token rather than the item — so the allow has to scope a module.
// Nothing hand-written in here does arithmetic.
mod error {
    #![allow(clippy::arithmetic_side_effects)]

    use super::{Deserialize, Facet, Serialize};

    /// Typed error for patchbay service boundaries.
    #[repr(C)]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet, thiserror::Error)]
    pub enum PatchbayError {
        /// Entity not found (port, link, preset, …).
        #[error("{entity} not found: {id}")]
        NotFound { entity: String, id: String },

        /// The `PipeWire` engine isn't running (no daemon, engine thread died).
        #[error("pipewire engine unavailable: {0}")]
        EngineUnavailable(String),

        /// Catch-all for unexpected failures.
        #[error("internal error: {0}")]
        Internal(String),

        /// An external device refused or failed an operation. `code`
        /// is a stable `snake_case` tag for agents (`offline`,
        /// `timeout`, `unknown_param`, `unknown_port`, `invalid_value`,
        /// `read_only`, `disruptive_write`, `unsupported`, `protocol`,
        /// `transport`, `ambiguous_device`).
        #[error("device error ({code}): {message}")]
        Device { code: String, message: String },
    }

    impl PatchbayError {
        /// A "no such thing" error naming what was looked up and by what.
        pub fn not_found(entity: impl Into<String>, id: &impl ToString) -> Self {
            Self::NotFound {
                entity: entity.into(),
                id: id.to_string(),
            }
        }
    }

    impl PatchbayError {
        /// A [`PatchbayError::Device`] with a stable code.
        pub fn device(code: impl Into<String>, message: impl Into<String>) -> Self {
            Self::Device {
                code: code.into(),
                message: message.into(),
            }
        }
    }

    impl From<String> for PatchbayError {
        fn from(s: String) -> Self {
            Self::Internal(s)
        }
    }
}

pub use error::PatchbayError;

pub mod patchbay_service {
    // The `#[architect::rpc]` macro expands to items that reference
    // every type in the trait signature plus the derive traits, so the
    // glob is load-bearing here rather than laziness.
    #[allow(clippy::wildcard_imports)]
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

        // ── Application streams ──────────────────────────────────────

        /// Applications currently playing audio, with the sink each one
        /// is playing into.
        async fn app_streams(&self) -> Result<Vec<AppStream>, PatchbayError>;

        /// Move a playing application's audio to `sink` (a `node.name`,
        /// e.g. a patchbay virtual sink).
        ///
        /// Takes the stream's `index` from [`AppStream`] — indices are
        /// unstable, so list immediately before moving.
        async fn move_app_stream(&self, index: u32, sink: String) -> Result<(), PatchbayError>;

        // ── Metering ─────────────────────────────────────────────────

        /// Declare which nodes should be metered, by `node.name`.
        ///
        /// Replaces the whole set: pass what is currently on screen,
        /// pass empty to stop metering. Each tapped node costs a `parec`
        /// child process, so this is deliberately explicit rather than
        /// metering everything. Returns the number of live taps.
        async fn set_metered(&self, nodes: Vec<String>) -> Result<u32, PatchbayError>;

        /// Current peak levels for every metered node.
        ///
        /// Poll this while meters are visible; a node whose tap has gone
        /// quiet or failed reports silence rather than a stale value.
        async fn meters(&self) -> Result<Vec<MeterLevel>, PatchbayError>;

        /// Set many aliases at once, persisted as ONE write.
        ///
        /// Naming a 128-channel bank one `set_alias` at a time meant 128
        /// round-trips AND 128 full rewrites of the config file; this is
        /// the bulk path every importer/renamer should use.
        async fn set_aliases(&self, entries: Vec<AliasEntry>) -> Result<u32, PatchbayError>;

        /// Pull channel names from a REAPER `ChanMap` (`nameN=Label`,
        /// 0-based → channel N+1) into port aliases on `node`: every
        /// port whose numeric suffix is N+1 (playback/capture/monitor)
        /// gets the label. Empty `path` = the host's default chanmap
        /// (`~/.fasttrackstudio/Reaper/ChanMaps/<hostname>.ReaperChanMap`).
        /// Returns the number of aliases written.
        async fn import_chanmap(&self, node: String, path: String) -> Result<u32, PatchbayError>;

        /// Push `node`'s port aliases back into a REAPER `ChanMap`'s
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
        /// applied on `PipeWire` restart (services panel).
        async fn set_clock_defaults(&self, defaults: ClockDefaults) -> Result<(), PatchbayError>;

        // ── Per-app latency rules ────────────────────────────────────

        async fn latency_rules(&self) -> Result<Vec<LatencyRule>, PatchbayError>;

        /// Add or replace the rule for `rule.pattern` and rewrite the
        /// `WirePlumber` drop-in. Takes effect when a matching node is
        /// (re)created — restart the app or `WirePlumber`.
        async fn set_latency_rule(&self, rule: LatencyRule) -> Result<(), PatchbayError>;

        /// Remove the rule matching `pattern` and rewrite the drop-in.
        async fn remove_latency_rule(&self, pattern: String) -> Result<(), PatchbayError>;

        // ── Dante / Inferno stack ────────────────────────────────────

        async fn dante_status(&self) -> Result<DanteStatus, PatchbayError>;

        /// Bring `dante.target` up or down (systemd user unit).
        async fn set_dante(&self, on: bool) -> Result<(), PatchbayError>;

        // ── Managed services (rig health) ────────────────────────────

        /// Status of every managed audio-stack unit (`PipeWire`,
        /// `WirePlumber`, statime, Inferno nodes, routing links, …).
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

        // ── Mixes (Loopback / OBS-style host audio) ─────────────────
        //
        // Sources (apps, input devices, system audio) summed into outputs
        // (e.g. the "Broadcast" virtual mic). Needs macOS; elsewhere
        // `host_targets().supported` is false and mixes stay stopped.

        /// Every saved mix with its live state.
        async fn list_mixes(&self) -> Result<Vec<MixView>, PatchbayError>;

        /// Create or replace a mix (by name) and (re)start it.
        async fn save_mix(&self, mix: MixConfig) -> Result<MixView, PatchbayError>;

        /// Stop and delete a mix.
        async fn delete_mix(&self, name: String) -> Result<(), PatchbayError>;

        /// Change a source's gain / mute live (and save it).
        async fn set_mix_source(
            &self,
            name: String,
            index: u32,
            gain_db: f64,
            muted: bool,
        ) -> Result<(), PatchbayError>;

        /// Change an output's gain / mute live (and save it).
        async fn set_mix_output(
            &self,
            name: String,
            index: u32,
            gain_db: f64,
            muted: bool,
        ) -> Result<(), PatchbayError>;

        /// Peak levels of every running mix since the previous call.
        async fn mix_meters(&self) -> Result<Vec<MixMeters>, PatchbayError>;

        /// Apps and devices a mix can use on this host.
        async fn host_targets(&self) -> Result<HostTargets, PatchbayError>;

        /// Everything the dashboard shows — apps with the devices they
        /// are playing to, devices, virtual devices, aggregates, mixes
        /// and anything wrong — in one read, so a live view costs one
        /// round-trip rather than ten.
        async fn host_overview(&self) -> Result<HostOverview, PatchbayError>;

        /// Peak level of every app the metering probe covers, since the
        /// previous call.
        ///
        /// Metering an app costs a process tap, so the probe follows
        /// what is actually playing and is capped; an app that isn't
        /// covered simply has no entry. Taps here never mute — this
        /// only reads.
        async fn app_meters(&self) -> Result<Vec<AppMeter>, PatchbayError>;

        // ── Virtual devices (Patchbay.driver, macOS) ─────────────────
        //
        // Loopback devices managed at runtime — no coreaudiod restart.
        // `device` accepts the uid or the display name.

        /// Whether the driver is loaded, and the devices it publishes.
        async fn virtual_devices(&self) -> Result<VirtualDevicesStatus, PatchbayError>;

        /// Create a virtual device (`channels` 1–64).
        async fn create_virtual_device(
            &self,
            name: String,
            channels: u32,
        ) -> Result<VirtualDeviceView, PatchbayError>;

        /// Rename a virtual device (its uid — and apps' selection — stays).
        async fn rename_virtual_device(
            &self,
            device: String,
            name: String,
        ) -> Result<(), PatchbayError>;

        /// Remove a virtual device.
        async fn remove_virtual_device(&self, device: String) -> Result<(), PatchbayError>;

        /// Public aggregate devices Patchbay made.
        async fn aggregates(&self) -> Result<Vec<AggregateView>, PatchbayError>;

        /// Create a public aggregate of `devices` (uids; the first clocks
        /// it), e.g. Galaxy32 + Patchbay for a DAW.
        async fn create_aggregate(
            &self,
            name: String,
            devices: Vec<String>,
        ) -> Result<AggregateView, PatchbayError>;

        /// Remove an aggregate Patchbay made (by uid).
        async fn remove_aggregate(&self, uid: String) -> Result<(), PatchbayError>;

        // ── External devices (hardware adapters) ─────────────────────
        //
        // `id` everywhere accepts the device id (`vendor:model:serial`),
        // the config entry name, or a unique case-insensitive substring
        // of id / model / serial. Channels on the wire are 0-based.

        /// Every configured device and its link state (no device I/O).
        async fn list_devices(&self) -> Result<Vec<DeviceSummary>, PatchbayError>;

        /// Full state read from the device now: port groups,
        /// crosspoints, params.
        async fn device(&self, id: String) -> Result<DeviceView, PatchbayError>;

        /// Params whose path is inside `prefix` (whole segments; empty =
        /// all), read from the device now.
        async fn device_params(
            &self,
            id: String,
            prefix: String,
        ) -> Result<Vec<ParamView>, PatchbayError>;

        /// Write one param. Disruptive params (clock, sample rate) are
        /// refused unless `allow_disruptive`. Returns the param as read
        /// back from the device after the write.
        async fn set_device_param(
            &self,
            id: String,
            path: String,
            value: DeviceParamValue,
            allow_disruptive: bool,
        ) -> Result<ParamView, PatchbayError>;

        /// Patch `source` (or nothing) into router output `output`.
        /// Returns the crosspoint as read back from the device.
        async fn set_device_route(
            &self,
            id: String,
            output: DeviceChannel,
            source: Option<DeviceChannel>,
        ) -> Result<DeviceCrosspoint, PatchbayError>;

        /// Every device event (all devices, tagged with the id).
        #[subscribe]
        fn device_events(&self) -> DeviceEventWire;

        /// Save the device's current writable params + crosspoints under
        /// `name` (upsert). `include` / `exclude` are path prefixes
        /// (`mixer/1/strip/16`, `route/DIGI_OUT0`); empty include = all.
        async fn save_device_snapshot(
            &self,
            id: String,
            name: String,
            include: Vec<String>,
            exclude: Vec<String>,
        ) -> Result<DeviceSnapshotInfo, PatchbayError>;

        async fn list_device_snapshots(&self) -> Result<Vec<DeviceSnapshotInfo>, PatchbayError>;

        async fn delete_device_snapshot(&self, name: String) -> Result<(), PatchbayError>;

        /// What `restore_device_snapshot` would change right now (reads
        /// the live device, writes nothing). `only` narrows the plan to
        /// path prefixes (empty = the whole snapshot).
        async fn diff_device_snapshot(
            &self,
            name: String,
            only: Vec<String>,
            allow_disruptive: bool,
        ) -> Result<DeviceRestoreReport, PatchbayError>;

        /// Diff against live, then write ONLY the differences (params via
        /// set_param, crosspoints via set_route) and report per item.
        /// `dry_run` = plan only.
        async fn restore_device_snapshot(
            &self,
            name: String,
            only: Vec<String>,
            dry_run: bool,
            allow_disruptive: bool,
        ) -> Result<DeviceRestoreReport, PatchbayError>;

        // ── Network ──────────────────────────────────────────────────

        /// Where the RPC and the browser remote listen, and where they
        /// will listen next start.
        async fn listen_address(&self) -> Result<ListenAddress, PatchbayError>;

        /// Set the listen address (`127.0.0.1:4046` for this machine
        /// only, `0.0.0.0:4046` for the LAN). Saved; it takes effect
        /// when Patchbay next starts, because rebinding a live server
        /// would drop every connected remote.
        ///
        /// **The RPC is unauthenticated.** Anything that can reach it
        /// can re-route this machine's audio and write to the consoles
        /// the device adapters are connected to, so only open it on a
        /// network you control.
        async fn set_listen_address(&self, bind: String) -> Result<ListenAddress, PatchbayError>;

        // ── Host privacy permissions (macOS) ─────────────────────────

        /// System Audio Recording / Microphone / Local Network state of
        /// the process serving the engine (`Patchbay.app` when bundled).
        async fn permissions(&self) -> Result<PermissionsStatus, PatchbayError>;

        /// Ask the app to re-run its permission flow: system prompts for
        /// anything undecided, an alert offering System Settings for
        /// anything denied. Returns at once (the flow runs in the app);
        /// poll `permissions` for the outcome.
        async fn request_permissions(&self) -> Result<PermissionsStatus, PatchbayError>;
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
