// Lint debt: workspace flipped dead_code/unused to warn (task cleanup);
// this crate predates that — burn down separately.
#![allow(dead_code, unused)]

//! Patchbay wire contract — PipeWire studio-routing types + services.
//!
//! The patchbay domain manages the live PipeWire graph (nodes / ports /
//! links), named routing presets (connection memory à la RaySession's
//! jackpatch), display aliases for cryptic node/port names, the graph
//! clock (quantum / rate), and the Dante/Inferno AoIP stack state.
//!
//! Everything a GUI needs goes over [`PatchbayService`] — the engine is
//! 100% headless and every UI (desktop, browser, tablet) is a vox remote.

mod types;

pub mod services;

pub use services::{
    PatchbayError, PatchbayService, PatchbayServiceClient, PatchbayServiceDispatcher,
    PatchbayServiceLayer, patchbay_service_layer, patchbay_service_service_descriptor,
    serve_patchbay_service,
};
pub use types::*;
