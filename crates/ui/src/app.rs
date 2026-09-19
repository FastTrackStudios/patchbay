//! Root component: icon rail + view outlet + status bar.
//!
//! Takes no props — the shell provides a [`crate::PatchbayHandle`] via
//! context and feeds [`crate::apply_graph_event`] from the subscribe
//! stream, so the same component mounts in the desktop app and any
//! future browser remote.

use dioxus::prelude::*;

use crate::canvas::GraphCanvas;
use crate::dante_grid::DanteGrid;
use crate::devices::DevicesView;
use crate::mixes::MixesView;
use crate::now::NowView;
use crate::panels::{SidePanel, StatusBar, Toolbar};
use crate::state::{ARMED_OUTPUTS, VIEW, View};

#[component]
pub fn PatchbayApp() -> Element {
    let view = *VIEW.read();
    let handle = crate::state::use_patchbay();

    // Pre-scan the Dante network in the background right after launch,
    // so the Network view opens populated instead of blank-then-scanning.
    let prescan = handle.clone();
    use_future(move || {
        let handle = prescan.clone();
        async move {
            crate::state::sleep_secs(1).await;
            if crate::state::DANTE_DEVICES.peek().is_empty() && !*crate::state::DANTE_LOADING.peek()
            {
                crate::state::refresh_dante(&handle).await;
            }
        }
    });
    rsx! {
        document::Style { {crate::theme::CSS} }
        div {
            class: "patchbay-root",
            tabindex: "0",
            onkeydown: move |e: Event<KeyboardData>| {
                if e.key() == Key::Escape {
                    ARMED_OUTPUTS.write().clear();
                    *crate::state::DRAG.write() = None;
                }
                // Ctrl+Z: undo the last link gesture.
                if e.modifiers().ctrl()
                    && matches!(e.key(), Key::Character(ref c) if c == "z" || c == "Z")
                {
                    crate::state::undo_last(handle.clone());
                }
            },
            div { class: "shell",
                Rail { current: view }
                div { class: "view-outlet",
                    match view {
                        View::Now => rsx! { NowView {} },
                        View::Graph => rsx! {
                            div { class: "topbar", Toolbar {} }
                            div { class: "main-split",
                                GraphCanvas {}
                                SidePanel {}
                            }
                        },
                        View::Network => rsx! { DanteGrid {} },
                        View::Devices => rsx! { DevicesView {} },
                        View::Mixes => rsx! { MixesView {} },
                    }
                }
            }
            StatusBar {}
        }
    }
}

/// The view switcher down the left edge.
#[component]
fn Rail(current: View) -> Element {
    rsx! {
        nav { class: "rail",
            div { class: "rail-brand", "Patchbay" }
            for view in View::ALL {
                button {
                    key: "{view.label()}",
                    class: if view == current { "rail-item on" } else { "rail-item" },
                    title: "{view.hint()}",
                    onclick: move |_| *VIEW.write() = view,
                    span { class: "rail-glyph", "{view.glyph()}" }
                    span { "{view.label()}" }
                }
            }
        }
    }
}
