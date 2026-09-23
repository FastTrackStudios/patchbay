//! Inline SVG icons.
//!
//! Drawn here rather than typed as glyphs: the webviews this runs in
//! (`WKWebView`, `WebKitGTK`, whatever browser a phone brings) each pick
//! their own font for `◉` or `⚙`, at their own weight and baseline.
//! Strokes on a 24-unit grid in `currentColor` look the same everywhere
//! and follow the theme for free. Nothing is fetched — a studio network
//! is often an island.

use dioxus::prelude::*;

use crate::state::View;

/// The rail / tab-bar icon for a view.
#[component]
pub fn ViewIcon(view: View) -> Element {
    rsx! {
        svg {
            class: "rail-icon",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            match view {
                // Two ports and the cable between them.
                View::Route => rsx! {
                    circle { cx: "5.5", cy: "7", r: "2.5" }
                    circle { cx: "18.5", cy: "17", r: "2.5" }
                    path { d: "M8 7c5.5 0 2.5 10 8 10" }
                },
                // Three faders.
                View::Mix => rsx! {
                    path { d: "M6 3.5v17M12 3.5v17M18 3.5v17" }
                    rect { x: "3.8", y: "13", width: "4.4", height: "3.4", rx: "1", fill: "currentColor", stroke: "none" }
                    rect { x: "9.8", y: "6.5", width: "4.4", height: "3.4", rx: "1", fill: "currentColor", stroke: "none" }
                    rect { x: "15.8", y: "10.5", width: "4.4", height: "3.4", rx: "1", fill: "currentColor", stroke: "none" }
                },
                // A stack of saved states.
                View::Scenes => rsx! {
                    path { d: "M12 3.5 3 8l9 4.5L21 8z" }
                    path { d: "M3.5 12.2 12 16.5l8.5-4.3" }
                    path { d: "M3.5 16.2 12 20.5l8.5-4.3" }
                },
                // A cog: hub, ring, eight teeth.
                View::Settings => rsx! {
                    circle { cx: "12", cy: "12", r: "2.6" }
                    circle { cx: "12", cy: "12", r: "6.4" }
                    path {
                        d: "M12 2.6v3M12 18.4v3M2.6 12h3M18.4 12h3M5.35 5.35l2.1 2.1M16.55 16.55l2.1 2.1M18.65 5.35l-2.1 2.1M7.45 16.55l-2.1 2.1",
                        stroke_width: "2.6",
                        stroke_linecap: "butt",
                    }
                },
            }
        }
    }
}

/// What kind of thing the current device is (`DeviceSummary::kind`).
#[component]
pub fn ContextIcon(kind: String) -> Element {
    rsx! {
        svg {
            class: "rail-icon",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            match kind.as_str() {
                // This machine: a laptop.
                crate::devices::KIND_SYSTEM => rsx! {
                    rect { x: "4.5", y: "5", width: "15", height: "10.5", rx: "1.8" }
                    path { d: "M2.5 19h19" }
                },
                // A network: three nodes off one switch.
                crate::devices::KIND_DANTE => rsx! {
                    circle { cx: "12", cy: "5.5", r: "2.3" }
                    circle { cx: "5", cy: "18.5", r: "2.3" }
                    circle { cx: "19", cy: "18.5", r: "2.3" }
                    path { d: "M12 7.8v4.7M12 12.5 6.4 16.7M12 12.5l5.6 4.2" }
                },
                // Hardware: two rack units.
                _ => rsx! {
                    rect { x: "3", y: "4.5", width: "18", height: "6.5", rx: "1.8" }
                    rect { x: "3", y: "13", width: "18", height: "6.5", rx: "1.8" }
                    path { d: "M6.6 7.75h.01M6.6 16.25h.01M10 7.75h.01M10 16.25h.01", stroke_width: "2.4" }
                },
            }
        }
    }
}

/// The Patchbay mark: ports and the cables between them, as on the app
/// icon, in the accent colour.
#[component]
pub fn Mark(#[props(default = "rail-mark".to_owned())] class: String) -> Element {
    rsx! {
        svg {
            class: "{class}",
            view_box: "0 0 32 32",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2.2",
            stroke_linecap: "round",
            "aria-hidden": "true",
            circle { cx: "7", cy: "8", r: "3" }
            circle { cx: "7", cy: "16", r: "3" }
            circle { cx: "7", cy: "24", r: "3", opacity: "0.4" }
            circle { cx: "25", cy: "8", r: "3", opacity: "0.4" }
            circle { cx: "25", cy: "16", r: "3" }
            circle { cx: "25", cy: "24", r: "3" }
            path { d: "M10 8c7 0 5 8 12 8" }
            path { d: "M10 16c7 0 5 8 12 8" }
        }
    }
}
