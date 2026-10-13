//! One channel strip, rendered from a [`Strip`] and written back
//! through the device's own params.
//!
//! Shared by every console view: a TF input, a TF mix master and a
//! Galaxy mixer channel are the same object with different leaves, so
//! they are the same component with a different [`Leaves`].

use dioxus::prelude::*;
use patchbay_proto::{DeviceParamKind, DeviceParamValue};

use super::console::{Leaves, Strip, color_hex};
use crate::state;
use crate::ui::level::fmt_db;

/// A strip's fader, over the device's own dB range.
///
/// Not the mix [`crate::ui::Fader`]: that one is fixed to -60..+12 with
/// an -∞ slot below, which is right for a software gain and wrong for a
/// console whose own range the device reports (-138..+10 on a TF,
/// -96..0 on the Galaxy, where 0 is unity and there is no boost).
#[component]
fn StripFader(path: String, value_db: f64, min_db: f64, max_db: f64, writable: bool) -> Element {
    let handle = state::use_patchbay();
    let reset = handle.clone();
    let reset_path = path.clone();
    // A TF's -32768 (-327.68 dB) is "off", far below its own minimum:
    // clamp for the control's sake, the device keeps the real value.
    let shown = value_db.clamp(min_db, max_db);
    rsx! {
        input {
            r#type: "range",
            class: "vfader",
            min: "{min_db}",
            max: "{max_db}",
            step: "0.5",
            value: "{shown}",
            disabled: !writable,
            title: "{fmt_db(value_db)} — double-click for 0 dB",
            oninput: move |e| {
                if let Ok(db) = e.value().parse::<f64>() {
                    super::set_param(handle.clone(), path.clone(), DeviceParamValue::Level(db));
                }
            },
            ondoubleclick: move |_| {
                if writable {
                    super::set_param(
                        reset.clone(),
                        reset_path.clone(),
                        DeviceParamValue::Level(0.0_f64.clamp(min_db, max_db)),
                    );
                }
            },
        }
    }
}

/// An enum param as a `<select>` — the TF's colour and icon vocabularies.
#[component]
fn Choice(
    path: String,
    options: Vec<String>,
    selected: String,
    writable: bool,
    class: String,
) -> Element {
    let handle = state::use_patchbay();
    if options.is_empty() {
        return rsx! {};
    }
    rsx! {
        select {
            class: "{class}",
            disabled: !writable,
            onchange: move |e| {
                if let Ok(i) = e.value().parse::<u32>() {
                    super::set_param(handle.clone(), path.clone(), DeviceParamValue::Enum(i));
                }
            },
            for (i, opt) in options.iter().enumerate() {
                option {
                    value: "{i}",
                    selected: *opt == selected,
                    "{opt}"
                }
            }
        }
    }
}

/// One channel strip.
#[component]
pub fn ChannelStrip(strip: Strip, leaves: Leaves, params_of: Signal<ParamFacts>) -> Element {
    let handle = state::use_patchbay();
    let facts = params_of.read().clone();
    let at = |leaf: &str| format!("{}/{leaf}", strip.prefix);
    let color = color_hex(&strip.color);
    let on = strip.on;
    let silent = strip.silent();

    // `on` is already "passing audio"; a device whose leaf is a mute had
    // it inverted on the way in, so invert it back on the way out.
    let toggle_on = {
        let handle = handle.clone();
        let path = at(leaves.on);
        move |_| {
            let Some(current) = on else { return };
            let wire = if leaves.on_inverted {
                current // muted := !on := current
            } else {
                !current
            };
            super::set_param(handle.clone(), path.clone(), DeviceParamValue::Toggle(wire));
        }
    };
    let rename = {
        let path = at(leaves.name);
        move |name: String| {
            super::set_param(handle.clone(), path.clone(), DeviceParamValue::Text(name));
        }
    };

    rsx! {
        div { class: if silent { "dev-strip silent" } else { "dev-strip" },
            div {
                class: "dev-strip-color",
                style: if color.is_empty() { String::new() } else { format!("background: {color};") },
            }
            div { class: "dev-strip-head",
                div { class: "dev-strip-pos", "{strip.position}" }
                if leaves.name.is_empty() {
                    div { class: "dev-strip-name static", "{strip.title()}" }
                } else {
                    crate::ui::TextField {
                        class: "dev-strip-name".to_owned(),
                        value: strip.name.clone(),
                        placeholder: strip.position.clone(),
                        on_commit: rename,
                    }
                }
                if !strip.icon.is_empty() {
                    Choice {
                        path: at(leaves.icon),
                        options: facts.icon_options.clone(),
                        selected: strip.icon.clone(),
                        writable: facts.icon_writable,
                        class: "dev-strip-pick".to_owned(),
                    }
                }
            }
            div { class: "dev-strip-body",
                if let Some(db) = strip.level_db {
                    StripFader {
                        path: at(leaves.level),
                        value_db: db,
                        min_db: facts.min_db,
                        max_db: facts.max_db,
                        writable: facts.level_writable,
                    }
                }
            }
            div { class: "dev-strip-db",
                if let Some(db) = strip.level_db { "{fmt_db(db)}" } else { "—" }
            }
            if on.is_some() {
                button {
                    class: if on == Some(true) { "chip strip-on on" } else { "chip strip-on" },
                    title: if leaves.on_inverted { "mute / unmute" } else { "the console's ON key" },
                    onclick: toggle_on,
                    if on == Some(true) { "on" } else { "off" }
                }
            }
            if !strip.color.is_empty() {
                Choice {
                    path: at(leaves.color),
                    options: facts.color_options.clone(),
                    selected: strip.color.clone(),
                    writable: facts.color_writable,
                    class: "dev-strip-pick".to_owned(),
                }
            }
        }
    }
}

/// What every strip in a bank shares: the level range the device
/// reports, whether its parts are writable, and the colour/icon
/// vocabularies.
///
/// Read once per bank from the first strip that has them, because they
/// are properties of the device, not of the channel — and the TF's
/// vocabularies grow at runtime, so they have to come off the live
/// param rather than a table in here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParamFacts {
    pub min_db: f64,
    pub max_db: f64,
    pub level_writable: bool,
    pub color_options: Vec<String>,
    pub color_writable: bool,
    pub icon_options: Vec<String>,
    pub icon_writable: bool,
}

impl ParamFacts {
    /// Read the shared facts off `sample`'s params.
    #[must_use]
    pub fn read(params: &super::console::Params<'_>, sample: &Strip, leaves: Leaves) -> Self {
        let at = |leaf: &str| format!("{}/{leaf}", sample.prefix);
        let level = params.get(&at(leaves.level));
        let (min_db, max_db) = match level.map(|p| &p.kind) {
            Some(DeviceParamKind::Level { min_db, max_db }) => (*min_db, *max_db),
            _ => (-96.0, 0.0),
        };
        Self {
            // A TF reports -138 dB; a fader that long is unusable and
            // every dB of it below -80 is silence anyway.
            min_db: min_db.max(-80.0),
            max_db,
            level_writable: level.is_some_and(|p| p.writable),
            color_options: params.options(&at(leaves.color)),
            color_writable: params.writable(&at(leaves.color)),
            icon_options: params.options(&at(leaves.icon)),
            icon_writable: params.writable(&at(leaves.icon)),
        }
    }
}
