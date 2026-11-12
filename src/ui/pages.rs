//! Route and Mix — the two things you do, drawn by the family of the
//! device you are doing them to (`crate::ui::devices::context`).
//!
//! Each family registers its views by config `kind`
//! ([`crate::ui::families`]); a kind that registered none gets the
//! generic device view. The built-ins today:
//!
//! |            | Route                                   | Mix                 |
//! |------------|-----------------------------------------|---------------------|
//! | System     | apps → devices (macOS), the graph (Linux) | host mixes        |
//! | Dante      | the subscription grid                   | — it only routes    |
//! | a console  | its router (generic)                    | strips, mixers, …   |

use dioxus::prelude::*;

use crate::ui::canvas::GraphCanvas;
use crate::ui::devices::{self, ContextTag, DeviceMix, DeviceRoute, KIND_SYSTEM};
use crate::ui::families;
use crate::ui::now::NowView;
use crate::ui::panels::{SidePanel, Toolbar};
use crate::ui::state::{self, PANEL_OPEN};
use crate::ui::ui::EmptyState;

/// The context's adapter kind. Before the device list has been read —
/// or on an engine with no device layer — it is the system: that is what
/// the app is for when nothing else is there.
fn context_kind() -> String {
    devices::context().map_or_else(|| KIND_SYSTEM.to_owned(), |d| d.kind)
}

#[component]
pub fn RoutePage() -> Element {
    let kind = context_kind();
    // Keyed on the kind: another family's view is another component, so
    // its hooks start fresh rather than inheriting the last one's.
    rsx! { FamilyRoute { key: "{kind}", kind } }
}

#[component]
pub fn MixPage() -> Element {
    let kind = context_kind();
    rsx! { FamilyMix { key: "{kind}", kind } }
}

/// The Route page for `kind`: its family's view, or the generic one.
#[component]
pub fn FamilyRoute(kind: String) -> Element {
    families::views(&kind)
        .and_then(|v| v.route)
        .map_or_else(|| rsx! { DeviceRoute {} }, |view| view())
}

/// The Mix page for `kind`: its family's view, or the generic one.
#[component]
pub fn FamilyMix(kind: String) -> Element {
    families::views(&kind)
        .and_then(|v| v.mix)
        .map_or_else(|| rsx! { DeviceMix {} }, |view| view())
}

/// The system's Route page: where there is a `PipeWire` graph it IS the
/// system's routing; on macOS that job belongs to which device each app
/// plays to.
#[component]
pub fn SystemRoute() -> Element {
    if state::has_graph_now() {
        rsx! { GraphRoute {} }
    } else {
        rsx! {
            NowView {}
            devices::SystemDrawer {}
        }
    }
}

/// The Mix page of a family that only routes (the Dante network).
#[component]
pub fn OnlyRoutes() -> Element {
    rsx! {
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
