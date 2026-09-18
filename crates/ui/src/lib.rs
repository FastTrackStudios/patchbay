//! Patchbay UI — Dioxus components for the `PipeWire` studio-routing app.
//!
//! Pure client surface: renders from global signals fed by
//! [`apply_graph_event`] / the fetch helpers, and talks back through the
//! [`PatchbayHandle`] (a `PatchbayServiceClient` provided via context by
//! the shell — desktop in-process today, browser remote later).

mod app;
mod canvas;
mod dante_grid;
mod devices;
mod layout;
mod mixes;
mod panels;
mod state;

pub use app::PatchbayApp;
pub use devices::apply_device_event;
pub use state::{PatchbayHandle, apply_graph_event, refresh_all, replace_graph, sleep_secs};
