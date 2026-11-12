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
mod families;
mod hosts;
mod icons;
mod layout;
mod mixes;
mod now;
mod page;
mod pages;
mod panels;
mod scenes;
mod session;
mod settings;
mod state;
mod theme;
mod ui;

pub use app::{PatchbayApp, Splash};

/// What a family's views draw with.
///
/// For the TF's and the Galaxy32's consoles: the device read indexed by
/// path, channel strips off it,
/// the shared strip component and controls, the empty state every page
/// uses, and one way to write: a [`kit::ParamWriter`], a param on the
/// device the page shows. Nothing wider of the window's state.
pub mod kit {
    pub use super::devices::console::{
        Leaves, Params, Strip, color_hex, count_under, numbered, strips,
    };
    pub use super::devices::strip::{ChannelStrip, ParamFacts};
    pub use super::devices::{ParamWriter, index_of, indexed, use_lane, use_param_writer};
    pub use super::ui::{Choice, ChoiceOption, EmptyState};
}
#[cfg(feature = "server")]
pub(crate) use families::kinds as family_view_kinds;
pub use families::{FamilyViews, register as register_family_views};
pub use hosts::{Dialer, EngineLink, Shell};
pub use page::{Page, PageFuture};
pub use state::View;
pub use theme::{CSS as THEME_CSS, NATIVE_CSS as THEME_NATIVE_CSS};

/// Show `view`: what a shell opens on (a screenshot, a deep link).
pub fn show(view: View) {
    *state::VIEW.write() = view;
}
pub use state::{PatchbayHandle, sleep_secs};
