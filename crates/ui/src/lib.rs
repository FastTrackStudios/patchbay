// Lint debt: workspace flipped dead_code/unused to warn (task cleanup);
// this crate predates that — burn down separately.
#![allow(dead_code, unused)]

//! Patchbay UI — Dioxus components for the PipeWire studio-routing app.
//!
//! Pure client surface: renders from global signals fed by
//! [`apply_graph_event`] / the fetch helpers, and talks back through the
//! [`PatchbayHandle`] (a `PatchbayServiceClient` provided via context by
//! the shell — desktop in-process today, browser remote later).

mod app;
mod canvas;
mod dante_grid;
mod layout;
mod panels;
mod state;

pub use app::PatchbayApp;
pub use state::{PatchbayHandle, apply_graph_event, refresh_all, replace_graph, sleep_secs};
