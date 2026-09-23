//! Patchbay — the headless `PipeWire` studio-routing engine.
//!
//! Owns a dedicated `PipeWire` main-loop thread (registry listener →
//! graph mirror → `GraphEvent` stream; link create/destroy commands in
//! the other direction — the helvum engine design, GUI-free), plus
//! presets (connection memory), display aliases, clock control and the
//! Dante stack switches. Everything is exposed through
//! [`patchbay_proto::PatchbayService`]; GUIs are vox remotes.
//!
//! Apps depend on this facade (or `patchbay-ui`), never on internals.

mod capture;
pub mod chanmap;
pub mod clock;
pub mod dante;
mod dante_net;
mod devices;
mod engine;
mod enrich;
mod icons;
mod latency;
mod meters;
mod mixes;
mod net;
mod peers;
pub mod permissions;
/// Pure decision logic — graph + config in, engine commands out.
mod plan;
mod presets;
mod service;
mod settle;
mod store;
mod streams;
mod telemetry;
mod units;

pub use patchbay_proto as proto;
pub use service::PatchbayBackend;

pub use net::record_bound;
pub use peers::start as start_peer_discovery;

/// The listen address saved in the config, if any.
///
/// Read before the engine starts, so the shell knows where to bind
/// without constructing a backend first. Empty/absent = use the
/// built-in default.
#[must_use]
pub fn configured_bind() -> Option<String> {
    let bind = presets::PresetStore::open().bind();
    (!bind.trim().is_empty()).then_some(bind)
}
