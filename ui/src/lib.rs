//! Patchbay UI — Dioxus components for the `PipeWire` studio-routing app.
//!
//! Pure client surface. A shell (the desktop app, the browser remote)
//! provides a [`Shell`] via context — its own engine connection and a way
//! to dial others — and mounts [`PatchbayApp`]; everything else, from
//! bridging the engine's event streams into the UI's mirrors to switching
//! between several engines, happens in here.

mod app;
mod appearance;
mod canvas;
mod dante_grid;
mod devices;
mod hosts;
mod icons;
mod layout;
mod mixes;
mod now;
mod pages;
mod panels;
mod scenes;
mod session;
mod settings;
mod state;
mod theme;
mod ui;

pub use app::{PatchbayApp, Splash};
pub use hosts::{Dialer, EngineLink, Shell};
pub use state::{PatchbayHandle, sleep_secs};
