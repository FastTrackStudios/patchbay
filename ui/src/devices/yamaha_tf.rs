//! Yamaha TF as a console: channel strips with the names, colours,
//! icons, faders and ON keys the desk itself carries.
//!
//! Everything here is push-notified over RCP — the surface, TF Editor
//! and `StageMix` all write to the same console and their changes arrive
//! as `ParamChanged`, so this never polls.
//!
//! Over RCP the TF exposes no input patch, no output patch and no
//! metering. The view says so rather than implying otherwise; Dante
//! patching for an NY64-D card belongs to the Network view.

use dioxus::prelude::*;
use patchbay_proto::DeviceView;

use super::console::{Params, TF_LEAVES, count_under, numbered, strips};
use super::strip::{ChannelStrip, ParamFacts};

/// Which bank of strips is showing.
static BANK: GlobalSignal<usize> = Signal::global(|| 0);

/// A bank of the console: its label, and how to find its strips.
struct Bank {
    label: &'static str,
    /// `(prefix, position)` pairs.
    prefixes: Vec<(String, String)>,
}

/// The banks this console actually has, in surface order.
///
/// Counts come from the device's own param table (a TF1 has 32 inputs,
/// a TF5 has 48), so this is not a model-specific list.
fn banks(view: &DeviceView) -> Vec<Bank> {
    let numbered_bank = |base: &'static str, label: &'static str, name: &'static str| Bank {
        label,
        prefixes: numbered(base, name, count_under(view, base)),
    };
    let mut banks = vec![
        numbered_bank("in", "Inputs", "CH"),
        numbered_bank("stin", "ST IN", "ST IN"),
        numbered_bank("fxrtn", "FX RTN", "FX RTN"),
        numbered_bank("aux", "AUX", "AUX"),
        numbered_bank("matrix", "MATRIX", "MX"),
        numbered_bank("dca", "DCA", "DCA"),
    ];
    // Stereo and Sub aren't numbered — they are named positions.
    banks.push(Bank {
        label: "Stereo / Sub",
        prefixes: vec![
            ("stereo/l".to_owned(), "ST L".to_owned()),
            ("stereo/r".to_owned(), "ST R".to_owned()),
            ("sub".to_owned(), "SUB".to_owned()),
        ],
    });
    banks.retain(|b| !b.prefixes.is_empty());
    banks
}

/// The current scene, for the header — a desk's state is a scene, and
/// knowing which one is loaded is the difference between "these are the
/// levels" and "these were the levels".
fn scene_line(params: &Params<'_>) -> String {
    let current = params.text("scene/current").unwrap_or_default();
    let title = params.text("scene/title").unwrap_or_default();
    let modified = params.toggle("scene/modified").unwrap_or(false);
    if current.is_empty() && title.is_empty() {
        return String::new();
    }
    let edited = if modified { " (edited)" } else { "" };
    if title.is_empty() {
        format!("scene {current}{edited}")
    } else {
        format!("scene {current} · {title}{edited}")
    }
}

#[component]
pub fn YamahaTfConsole(view: DeviceView) -> Element {
    let params = Params::index(&view);
    let banks = banks(&view);
    let selected = (*BANK.read()).min(banks.len().saturating_sub(1));
    let scene = scene_line(&params);

    let Some(bank) = banks.get(selected) else {
        return rsx! {
            crate::ui::EmptyState { title: "Nothing to show yet".to_owned(),
                p { "The console hasn't reported any channels." }
            }
        };
    };
    let strips = strips(&params, bank.prefixes.clone(), TF_LEAVES);
    let facts = strips
        .first()
        .map(|s| ParamFacts::read(&params, s, TF_LEAVES))
        .unwrap_or_default();
    let facts = use_signal(|| facts);

    rsx! {
        div { class: "console",
            div { class: "console-bar",
                for (i, b) in banks.iter().enumerate() {
                    button {
                        key: "{b.label}",
                        class: if i == selected { "chip on" } else { "chip" },
                        onclick: move |_| *BANK.write() = i,
                        "{b.label}"
                    }
                }
                if !scene.is_empty() {
                    span { class: "console-scene", "{scene}" }
                }
            }
            div { class: "dev-strips",
                if strips.is_empty() {
                    p { class: "dim-note", "No channels in this bank." }
                }
                for s in strips {
                    ChannelStrip {
                        key: "{s.prefix}",
                        strip: s,
                        leaves: TF_LEAVES,
                        params_of: facts,
                    }
                }
            }
            p { class: "dim-note console-note",
                "Faders, names, colours, icons and sends come straight from the desk — the "
                "surface, TF Editor and StageMix all write here too, and their changes show up "
                "without a refresh. Input and output patching aren't on this protocol; Dante "
                "patching lives in Network."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchbay_proto::{
        DeviceLinkState, DeviceParamKind, DeviceParamValue, DeviceSummary, ParamView,
    };

    fn text(path: &str, v: &str) -> ParamView {
        ParamView {
            path: path.to_owned(),
            label: path.to_owned(),
            kind: DeviceParamKind::Text,
            value: DeviceParamValue::Text(v.to_owned()),
            writable: false,
            disruptive: false,
        }
    }

    fn toggle(path: &str, v: bool) -> ParamView {
        ParamView {
            path: path.to_owned(),
            label: path.to_owned(),
            kind: DeviceParamKind::Toggle,
            value: DeviceParamValue::Toggle(v),
            writable: false,
            disruptive: false,
        }
    }

    fn view(params: Vec<ParamView>) -> DeviceView {
        DeviceView {
            summary: DeviceSummary {
                id: "yamaha:tf:1".into(),
                name: "TF1".into(),
                kind: "yamaha-tf".into(),
                vendor: String::new(),
                model: String::new(),
                serial: String::new(),
                firmware: String::new(),
                transport: String::new(),
                state: DeviceLinkState::Online,
                error: String::new(),
            },
            inputs: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            params,
        }
    }

    #[test]
    fn the_scene_line_says_which_state_the_desk_is_in() {
        let v = view(vec![
            text("scene/current", "A05"),
            text("scene/title", "Sunday AM"),
            toggle("scene/modified", true),
        ]);
        assert_eq!(
            scene_line(&Params::index(&v)),
            "scene A05 · Sunday AM (edited)"
        );

        let v = view(vec![text("scene/current", "B01")]);
        assert_eq!(scene_line(&Params::index(&v)), "scene B01");

        // A desk that reports no scene says nothing rather than "scene".
        let v = view(Vec::new());
        assert_eq!(scene_line(&Params::index(&v)), "");
    }

    #[test]
    fn banks_follow_what_the_console_reported() {
        let mut params = vec![text("in/1/name", "a"), text("in/32/name", "b")];
        params.push(text("aux/1/name", "c"));
        params.push(text("aux/20/name", "d"));
        let v = view(params);
        let b = banks(&v);
        // Inputs and AUX are there; FX RTN, MATRIX and DCA are not, and
        // empty banks don't get a tab.
        let labels: Vec<&str> = b.iter().map(|x| x.label).collect();
        assert!(labels.contains(&"Inputs"));
        assert!(labels.contains(&"AUX"));
        assert!(!labels.contains(&"DCA"));
        let inputs = b.iter().find(|x| x.label == "Inputs").expect("inputs");
        assert_eq!(inputs.prefixes.len(), 32);
        assert_eq!(inputs.prefixes.first().map(|p| p.1.as_str()), Some("CH 1"));
    }
}
