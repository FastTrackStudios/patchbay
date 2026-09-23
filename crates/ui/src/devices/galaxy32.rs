//! Antelope Galaxy 32 as a console: the four mixers, the line-in trims,
//! the AFX insert grid and the clock. Its router is the device's Route
//! page (`super::RoutePane`).
//!
//! The device carries no names, colours or icons of its own — every
//! label here comes from its static page tables. Patchbay's aliases are
//! where human names for these channels live.
//!
//! Its routing is not notified: another client moving a crosspoint
//! shows up when the adapter's mirror next refreshes, so the router
//! says how fresh it is instead of pretending to be live.

use dioxus::prelude::*;
use patchbay_proto::{DeviceParamValue, DeviceView};

use super::console::{GALAXY_LEAVES, Params, count_under, numbered, strips};
use super::strip::{ChannelStrip, ParamFacts};
use crate::state;

/// Which page of the device is showing.
static PAGE: GlobalSignal<Page> = Signal::global(|| Page::Mixer(1));

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    /// 1-based mixer number.
    Mixer(u32),
    Trim,
    Afx,
    Clock,
}

/// How far behind the hardware panel the router can be.
///
/// The adapter re-reads every 5 s because the Manager Server never
/// notifies routing changes made elsewhere.
pub(super) const ROUTER_LAG: &str = "Routing changes made on the hardware panel or in the Antelope \
                          software appear here within about five seconds — the device \
                          doesn't announce them. Changes made here are immediate.";

#[component]
pub fn Galaxy32Console(view: DeviceView) -> Element {
    let mixers = count_under(&view, "mixer");
    let page = *PAGE.read();

    rsx! {
        div { class: "console",
            div { class: "console-bar",
                for n in 1..=mixers {
                    button {
                        key: "mix{n}",
                        class: if page == Page::Mixer(n) { "chip on" } else { "chip" },
                        onclick: move |_| *PAGE.write() = Page::Mixer(n),
                        "Mix {n}"
                    }
                }
                button {
                    class: if page == Page::Trim { "chip on" } else { "chip" },
                    onclick: move |_| *PAGE.write() = Page::Trim,
                    "Trim"
                }
                button {
                    class: if page == Page::Afx { "chip on" } else { "chip" },
                    onclick: move |_| *PAGE.write() = Page::Afx,
                    "AFX"
                }
                button {
                    class: if page == Page::Clock { "chip on" } else { "chip" },
                    onclick: move |_| *PAGE.write() = Page::Clock,
                    "Clock"
                }
            }
            match page {
                Page::Mixer(n) => rsx! { Mixer { view, mixer: n } },
                Page::Trim => rsx! { Trim { view } },
                Page::Afx => rsx! { Afx { view } },
                Page::Clock => rsx! { Clock { view } },
            }
        }
    }
}

/// One of the four mixers: its master, then its 32 channels.
#[component]
fn Mixer(view: DeviceView, mixer: u32) -> Element {
    let params = Params::index(&view);
    let base = format!("mixer/{mixer}");
    let mut prefixes = vec![(format!("{base}/master"), "MASTER".to_owned())];
    prefixes.extend(numbered(
        &format!("{base}/strip"),
        "",
        count_under(&view, &format!("{base}/strip")),
    ));
    // `numbered` labels "` 1`"; the channel number alone reads better
    // on a strip with no name.
    for (_, position) in prefixes.iter_mut().skip(1) {
        *position = position.trim().to_owned();
    }
    let strips = strips(&params, prefixes, GALAXY_LEAVES);
    let facts = strips
        .first()
        .map(|s| ParamFacts::read(&params, s, GALAXY_LEAVES))
        .unwrap_or_default();
    let facts = use_signal(|| facts);

    rsx! {
        p { class: "dim-note console-note",
            "Mix {mixer}'s channels are fed by the router's MIX{mixer} IN page — patch a source "
            "there and it arrives on the strip of the same number. Levels are attenuation only: "
            "0 dB is unity, there is no boost."
        }
        div { class: "dev-strips",
            if strips.is_empty() {
                p { class: "dim-note", "This mixer reported no channels." }
            }
            for s in strips {
                ChannelStrip { key: "{s.prefix}", strip: s, leaves: GALAXY_LEAVES, params_of: facts }
            }
        }
    }
}

/// Line-in trims, and the ALL/MANUAL control that governs them.
#[component]
fn Trim(view: DeviceView) -> Element {
    let handle = state::use_patchbay();
    let params = Params::index(&view);
    let mode = params.choice("trim/line_in/control").unwrap_or_default();
    let options = params.options("trim/line_in/control");
    let all_mode = mode.eq_ignore_ascii_case("all");
    let count = count_under(&view, "trim/line_in");

    rsx! {
        div { class: if all_mode { "trim-warning on" } else { "trim-warning" },
            span { class: "section-label", "Trim control" }
            select {
                class: "dev-strip-pick",
                onchange: move |e| {
                    if let Ok(i) = e.value().parse::<u32>() {
                        super::set_param(
                            handle.clone(),
                            "trim/line_in/control".to_owned(),
                            DeviceParamValue::Enum(i),
                        );
                    }
                },
                for (i, opt) in options.iter().enumerate() {
                    option { value: "{i}", selected: *opt == mode, "{opt}" }
                }
            }
            if all_mode {
                span { class: "trim-warning-text",
                    "In ALL mode the device applies one trim to every line input — moving any "
                    "one of these moves all 32. Switch to MANUAL to set them individually."
                }
            }
        }
        div { class: "trim-grid",
            for n in 1..=count {
                {
                    let path = format!("trim/line_in/{n}");
                    let db = params.level(&path);
                    rsx! {
                        div { class: "trim-cell", key: "{path}",
                            span { class: "trim-num", "{n}" }
                            span { class: "trim-val",
                                if let Some(db) = db { "{db:.1} dBu" } else { "—" }
                            }
                        }
                    }
                }
            }
        }
        p { class: "dim-note console-note",
            "Trims are in dBu, and the top of the range is no attenuation. Set one in the "
            "Inspector below; this page shows them together so an odd one out is obvious."
        }
    }
}

/// The 32 × 8 insert grid: which effect sits in each slot.
#[component]
fn Afx(view: DeviceView) -> Element {
    let handle = state::use_patchbay();
    let params = Params::index(&view);
    let strips_n = count_under(&view, "afx/strip");
    let slots = count_under(&view, "afx/strip/1/slot");
    let options = params.options("afx/strip/1/slot/1/effect");

    rsx! {
        p { class: "dim-note console-note",
            "Which effect sits in each insert slot. The parameters inside an effect can't be "
            "read back from the device — only slots Patchbay itself has written this session "
            "have values, and those are in the Inspector."
        }
        div { class: "afx-scroll",
            table { class: "afx-grid",
                thead {
                    tr {
                        th { class: "afx-corner", "ch" }
                        for slot in 1..=slots {
                            th { key: "s{slot}", "{slot}" }
                        }
                    }
                }
                tbody {
                    for ch in 1..=strips_n {
                        tr { key: "c{ch}",
                            th { class: "afx-ch", "{ch}" }
                            for slot in 1..=slots {
                                {
                                    let path = format!("afx/strip/{ch}/slot/{slot}/effect");
                                    let current = params.choice(&path).unwrap_or_default();
                                    let empty = current.is_empty() || current.eq_ignore_ascii_case("none");
                                    let handle = handle.clone();
                                    let options = options.clone();
                                    rsx! {
                                        td { key: "{path}", class: if empty { "afx-cell" } else { "afx-cell filled" },
                                            select {
                                                onchange: move |e| {
                                                    if let Ok(i) = e.value().parse::<u32>() {
                                                        super::set_param(
                                                            handle.clone(),
                                                            path.clone(),
                                                            DeviceParamValue::Enum(i),
                                                        );
                                                    }
                                                },
                                                for (i, opt) in options.iter().enumerate() {
                                                    option { value: "{i}", selected: *opt == current, "{opt}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Clock and sample rate — shown, but changing them interrupts audio.
#[component]
fn Clock(view: DeviceView) -> Element {
    let params = Params::index(&view);
    let source = params.choice("clock/sync_source").unwrap_or_default();
    let rate = params.choice("clock/sample_rate").unwrap_or_default();
    let measured = params
        .get("clock/measured_rate")
        .map(|p| p.value.display(None));
    let locked = params.toggle("clock/locked");

    rsx! {
        div { class: "clock-facts",
            Fact { label: "Sync source".to_owned(), value: source }
            Fact { label: "Sample rate".to_owned(), value: rate }
            Fact { label: "Measured".to_owned(), value: measured.unwrap_or_default() }
            Fact {
                label: "Lock".to_owned(),
                value: match locked {
                    Some(true) => "locked".to_owned(),
                    Some(false) => "unlocked".to_owned(),
                    None => String::new(),
                },
            }
        }
        p { class: "dim-note console-note",
            "Sync source and sample rate are the only settings on this device that interrupt "
            "audio, so they are read-only here. Change one in the Inspector with "
            "\"allow disruptive writes\" ticked, when it is safe to drop the clock."
        }
    }
}

#[component]
fn Fact(label: String, value: String) -> Element {
    rsx! {
        div { class: "clock-fact",
            span { class: "section-label", "{label}" }
            span { class: "clock-fact-value",
                if value.is_empty() { "—" } else { "{value}" }
            }
        }
    }
}
