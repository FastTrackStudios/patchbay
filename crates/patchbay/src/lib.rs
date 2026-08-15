//! Patchbay — the headless PipeWire studio-routing engine.
//!
//! Owns a dedicated PipeWire main-loop thread (registry listener →
//! graph mirror → `GraphEvent` stream; link create/destroy commands in
//! the other direction — the helvum engine design, GUI-free), plus
//! presets (connection memory), display aliases, clock control and the
//! Dante stack switches. Everything is exposed through
//! [`patchbay_proto::PatchbayService`]; GUIs are vox remotes.
//!
//! Apps depend on this facade (or `patchbay-ui`), never on internals.

pub mod chanmap;
pub mod clock;
pub mod dante;
mod dante_net;
mod engine;
mod enrich;
mod icons;
mod latency;
mod presets;
mod service;
mod store;
mod units;

pub use patchbay_proto as proto;
pub use service::PatchbayBackend;
