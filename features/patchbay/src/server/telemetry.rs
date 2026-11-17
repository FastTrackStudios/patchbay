//! Wide-event field names for the patchbay domain.
//!
//! The span IS the wide event: `architect`'s `LayerRouter` already opens
//! one span per dispatched RPC carrying `rpc.service` / `rpc.method`, and
//! this module adds the patchbay-specific business context onto it via
//! [`architect_telemetry::wide`].
//!
//! Names live here as constants because **renaming a field breaks every
//! saved query**. Add a name, never repurpose one.
//!
//! # What belongs on the event
//!
//! The outcome of a unit of work, with enough context to answer "why
//! didn't my rig wire up" without a second look: which route, how many
//! links it made, whether the engine was even reachable. Successes ride
//! the span only — a log line per created link is exactly the scatter
//! the wide-event pattern exists to delete.
//!
//! # What must never appear
//!
//! `PipeWire` global ids are fine (small, bounded, and meaningless off-box).
//! Node and port NAMES are bounded by the hardware and apps on one
//! machine, so they are safe and genuinely useful. Do not add anything
//! resembling a credential, and do not add a field whose cardinality is
//! unbounded in the bad way (a filesystem path, a whole config body).

/// Set a wide-event field on the current span.
///
/// Thin re-export so call sites read as `telemetry::set(...)` and the
/// crate has exactly one place that knows how enrichment is done.
pub(crate) use architect_telemetry::wide::set;
/// Set a field from anything `Display`.
pub(crate) use architect_telemetry::wide::set_display;

// ── Graph shape ─────────────────────────────────────────────────────
/// Nodes in the mirror at the time of the call.
pub(crate) const GRAPH_NODES: &str = "patchbay.graph.nodes";
/// Ports in the mirror at the time of the call.
pub(crate) const GRAPH_PORTS: &str = "patchbay.graph.ports";
/// Links in the mirror at the time of the call.
pub(crate) const GRAPH_LINKS: &str = "patchbay.graph.links";

// ── Engine ──────────────────────────────────────────────────────────
/// Why a unit of work ran: `settle` | `rpc` | `reconnect`.
pub(crate) const TRIGGER: &str = "patchbay.trigger";
/// Commands actually handed to the `PipeWire` thread.
pub(crate) const COMMANDS_SENT: &str = "patchbay.commands.sent";
/// Commands the engine refused (it was down / reconnecting).
pub(crate) const COMMANDS_FAILED: &str = "patchbay.commands.failed";

// ── Links ───────────────────────────────────────────────────────────
pub(crate) const LINK_OUTPUT_PORT: &str = "patchbay.link.output_port";
pub(crate) const LINK_INPUT_PORT: &str = "patchbay.link.input_port";
/// The link already existed, so no command was sent.
pub(crate) const LINK_ALREADY_PRESENT: &str = "patchbay.link.already_present";

// ── Named routes ────────────────────────────────────────────────────
/// Routes considered (enabled only).
pub(crate) const ROUTES_CONSIDERED: &str = "patchbay.routes.considered";
/// Routes whose BOTH endpoints resolved against the live graph.
pub(crate) const ROUTES_RESOLVED: &str = "patchbay.routes.resolved";
/// Links the apply actually created.
pub(crate) const ROUTES_LINKS_CREATED: &str = "patchbay.routes.links_created";
/// Route names that did not resolve — the "why is my rig not wired"
/// field. Bounded by the user's own config.
pub(crate) const ROUTES_UNRESOLVED: &str = "patchbay.routes.unresolved";

// ── Presets ─────────────────────────────────────────────────────────
pub(crate) const PRESET_NAME: &str = "patchbay.preset.name";
pub(crate) const PRESET_EXCLUSIVE: &str = "patchbay.preset.exclusive";
pub(crate) const PRESET_CREATED: &str = "patchbay.preset.created";
pub(crate) const PRESET_EXISTING: &str = "patchbay.preset.existing";
pub(crate) const PRESET_MISSING: &str = "patchbay.preset.missing";
pub(crate) const PRESET_DESTROYED: &str = "patchbay.preset.destroyed";

// ── Virtual sinks ───────────────────────────────────────────────────
pub(crate) const SINKS_CONFIGURED: &str = "patchbay.sinks.configured";
/// Sinks that were missing from the live graph and got created.
pub(crate) const SINKS_CREATED: &str = "patchbay.sinks.created";
pub(crate) const SINK_NAME: &str = "patchbay.sink.name";
/// Capture sources created so recorders (OBS) can see a bus by name.
pub(crate) const CAPTURE_SOURCES_CREATED: &str = "patchbay.sinks.capture_sources_created";
pub(crate) const SINK_CHANNELS: &str = "patchbay.sink.channels";

// ── Aliases / chanmap ───────────────────────────────────────────────
pub(crate) const NODE_NAME: &str = "patchbay.node.name";
pub(crate) const ALIASES_WRITTEN: &str = "patchbay.aliases.written";

// ── Settle ──────────────────────────────────────────────────────────
/// Graph events seen in the burst that preceded this settle.
pub(crate) const SETTLE_EVENTS: &str = "patchbay.settle.events";
/// How long the burst lasted, first event to quiet.
pub(crate) const SETTLE_BURST_MS: &str = "patchbay.settle.burst_ms";

// ── Metering ────────────────────────────────────────────────────────
/// Nodes a client asked to meter.
pub(crate) const METERS_REQUESTED: &str = "patchbay.meters.requested";
/// Taps actually running — lower than requested when a node vanished or
/// its source could not be recorded.
pub(crate) const METERS_ACTIVE: &str = "patchbay.meters.active";

// ── Application streams ─────────────────────────────────────────────
/// App streams `pipewire-pulse` reported.
pub(crate) const STREAMS_LISTED: &str = "patchbay.streams.listed";
/// Sink-input index being moved.
pub(crate) const STREAM_INDEX: &str = "patchbay.stream.index";
/// `node.name` of the sink it is being moved to.
pub(crate) const STREAM_TARGET_SINK: &str = "patchbay.stream.target_sink";
