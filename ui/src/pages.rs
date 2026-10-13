//! Route and Mix — the two things you do, dispatched on the device you
//! are doing them to (`crate::devices::context`).
//!
//! |            | Route                                   | Mix                 |
//! |------------|-----------------------------------------|---------------------|
//! | System     | apps → devices (macOS), the graph (Linux) | host mixes        |
//! | Dante      | the subscription grid                   | — it only routes    |
//! | a console  | its router                              | strips, mixers, …   |
//!
//! The views themselves are unchanged and know nothing about this; the
//! table above is all this module is.

use dioxus::prelude::*;

use crate::canvas::GraphCanvas;
use crate::dante_grid::DanteGrid;
use crate::devices::{self, ContextTag, DeviceMix, DeviceRoute, KIND_DANTE, KIND_SYSTEM};
use crate::mixes::MixesView;
use crate::now::NowView;
use crate::panels::{SidePanel, Toolbar};
use crate::state::{self, PANEL_OPEN};
use crate::ui::EmptyState;

/// The context's adapter kind. Before the device list has been read —
/// or on an engine with no device layer — it is the system: that is what
/// the app is for when nothing else is there.
fn context_kind() -> String {
    devices::context().map_or_else(|| KIND_SYSTEM.to_owned(), |d| d.kind)
}

#[component]
pub fn RoutePage() -> Element {
    match context_kind().as_str() {
        // Where there is a PipeWire graph it IS the system's routing; on
        // macOS that job belongs to which device each app plays to.
        KIND_SYSTEM if state::has_graph_now() => rsx! { GraphRoute {} },
        KIND_SYSTEM => rsx! {
            NowView {}
            devices::SystemDrawer {}
        },
        KIND_DANTE => rsx! { DanteGrid {} },
        _ => rsx! { DeviceRoute {} },
    }
}

#[component]
pub fn MixPage() -> Element {
    match context_kind().as_str() {
        KIND_SYSTEM => rsx! { MixesView {} },
        KIND_DANTE => rsx! {
            div { class: "view-head",
                span { class: "view-title", "Mix" }
                ContextTag {}
            }
            div { class: "view-body",
                EmptyState { title: "Dante only routes",
                    p {
                        "A Dante network carries channels from transmitters to receivers "
                        "unchanged — there are no levels to set on it. Mixing happens on "
                        "the devices at either end: pick one in the rail."
                    }
                }
            }
        },
        _ => rsx! { DeviceMix {} },
    }
}

/// The `PipeWire` node canvas with its toolbar and side panel.
#[component]
fn GraphRoute() -> Element {
    rsx! {
        div { class: "topbar",
            Toolbar {}
            button {
                class: if *PANEL_OPEN.read() { "chip on panel-toggle" } else { "chip panel-toggle" },
                title: "Presets, the inspector and virtual sinks",
                onclick: move |_| {
                    let open = *PANEL_OPEN.peek();
                    *PANEL_OPEN.write() = !open;
                },
                "panel"
            }
        }
        div { class: if *PANEL_OPEN.read() { "main-split panel-open" } else { "main-split" },
            GraphCanvas {}
            button {
                class: "side-panel-scrim",
                "aria-label": "close panel",
                onclick: move |_| *PANEL_OPEN.write() = false,
            }
            SidePanel {}
        }
    }
}
