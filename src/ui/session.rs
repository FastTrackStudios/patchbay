//! Where you were — per device, like the appearance.
//!
//! A phone reloads the page constantly (it slept, the tab was evicted,
//! the app was reinstalled), and every reload used to land on the first
//! device's Route page at the default zoom. This puts you back: the view,
//! the device, which machine it is on, and the Dante grid's zoom and
//! safe-edit switch. `localStorage`, so the desktop window and a phone
//! each keep their own place.

use dioxus::prelude::*;

use crate::ui::state::{VIEW, View};

const STORE_KEY: &str = "patchbay.session";

/// The stored session has been read; until then nothing is written, or
/// the defaults this starts with would overwrite it.
static LOADED: GlobalSignal<bool> = Signal::global(|| false);

/// Another machine to go back to, as soon as it is connected:
/// `(addr, device)`. Cleared the moment the user picks anything else.
pub static WANT: GlobalSignal<Option<(String, Option<String>)>> = Signal::global(|| None);

#[derive(Debug, Default, PartialEq, Eq)]
struct Session {
    view: Option<View>,
    /// Empty = the home engine.
    host: String,
    device: String,
    cell: Option<u32>,
    safe_edit: Option<bool>,
}

impl Session {
    fn encode(&self) -> String {
        let view = self.view.map_or("", View::label);
        let cell = self.cell.map(|c| c.to_string()).unwrap_or_default();
        let safe = self.safe_edit.map_or("", |s| if s { "1" } else { "0" });
        format!(
            "view={view};host={};device={};cell={cell};safe={safe}",
            self.host, self.device
        )
    }

    /// Tolerant, like the appearance: what this build doesn't recognise
    /// is skipped and the rest still applies.
    fn decode(stored: &str) -> Self {
        let mut out = Self::default();
        for (key, value) in stored.split(';').filter_map(|kv| kv.split_once('=')) {
            let value = value.trim();
            match key.trim() {
                "view" => out.view = View::ALL.into_iter().find(|v| v.label() == value),
                "host" => value.clone_into(&mut out.host),
                "device" => value.clone_into(&mut out.device),
                "cell" => out.cell = value.parse().ok().filter(|c| (8..=64).contains(c)),
                "safe" => {
                    out.safe_edit = match value {
                        "1" => Some(true),
                        "0" => Some(false),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        out
    }
}

fn apply(s: Session) {
    if let Some(view) = s.view {
        *VIEW.write() = view;
    }
    if let (Some(cell), Some(safe)) = (s.cell, s.safe_edit) {
        crate::ui::dante_grid::restore(cell, safe);
    }
    let device = (!s.device.is_empty()).then_some(s.device);
    if s.host.is_empty() {
        if let Some(device) = device {
            crate::ui::devices::prefer(device);
        }
    } else {
        // Its engine isn't connected yet; `hosts` goes live when it is.
        *WANT.write() = Some((s.host, device));
    }
}

fn current() -> Session {
    let (cell, safe) = crate::ui::dante_grid::prefs();
    // Still on the way back to another machine: that is where "here" is,
    // not the home engine being shown in the meantime.
    let (host, device) = WANT.read().clone().map_or_else(
        || {
            (
                crate::ui::hosts::LIVE.read().clone().unwrap_or_default(),
                crate::ui::devices::selected_key().unwrap_or_default(),
            )
        },
        |(host, device)| (host, device.unwrap_or_default()),
    );
    Session {
        view: Some(*VIEW.read()),
        host,
        device,
        cell: Some(cell),
        safe_edit: Some(safe),
    }
}

/// Restore the session once, then keep it saved. Call from the app root,
/// above the live engine.
pub fn use_session() {
    use_future(|| async {
        let js = format!(
            r#"try {{ return localStorage.getItem("{STORE_KEY}") || ""; }} catch (_) {{ return ""; }}"#
        );
        if let Ok(stored) = document::eval(&js).join::<String>().await
            && !stored.is_empty()
        {
            apply(Session::decode(&stored));
        }
        *LOADED.write() = true;
    });

    use_effect(|| {
        let encoded = current().encode();
        if *LOADED.read() {
            // Device keys and host names come from config files: keep a
            // quote or backslash in one from ending the string early.
            let safe = encoded.replace('\\', "\\\\").replace('"', "\\\"");
            document::eval(&format!(
                r#"try {{ localStorage.setItem("{STORE_KEY}", "{safe}"); }} catch (_) {{}}"#
            ));
        }
    });
}
