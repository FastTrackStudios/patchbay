//! Root component: toolbar + canvas + side panel + status bar.
//!
//! Takes no props — the shell provides a [`crate::PatchbayHandle`] via
//! context and feeds [`crate::apply_graph_event`] from the subscribe
//! stream, so the same component mounts in the desktop app and any
//! future browser remote.

use dioxus::prelude::*;

use crate::canvas::GraphCanvas;
use crate::dante_grid::DanteGrid;
use crate::panels::{SidePanel, StatusBar, Toolbar};
use crate::state::{ARMED_OUTPUTS, VIEW, View};

static CSS: &str = include_str!("style.css");

#[component]
pub fn PatchbayApp() -> Element {
    let view = *VIEW.read();
    let handle = crate::state::use_patchbay();

    // Pre-scan the Dante network in the background right after launch,
    // so the Dante tab opens populated instead of blank-then-scanning.
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
        document::Style { {CSS} }
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
            div { class: "topbar",
                span { class: "app-title", "Patchbay" }
                div { class: "view-tabs",
                    button {
                        class: if view == View::Patchbay { "tab on" } else { "tab" },
                        onclick: move |_| *VIEW.write() = View::Patchbay,
                        "Patchbay"
                    }
                    button {
                        class: if view == View::Dante { "tab on" } else { "tab" },
                        onclick: move |_| *VIEW.write() = View::Dante,
                        "Dante"
                    }
                }
                if view == View::Patchbay {
                    Toolbar {}
                }
            }
            match view {
                View::Patchbay => rsx! {
                    div { class: "main-split",
                        GraphCanvas {}
                        SidePanel {}
                    }
                },
                View::Dante => rsx! {
                    DanteGrid {}
                },
            }
            StatusBar {}
        }
    }
}
