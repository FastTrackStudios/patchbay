//! The host's own audio layer as a device page.
//!
//! The `system-audio` adapter exists so the host shows up in
//! `patchbay device` like any other device, and it reports its state as
//! params: `device/<uid>/…` per device, `app/<pid>/…` per audio process.
//! That is exactly right on the wire and unreadable as a tree — it is
//! why this tab used to open on rows like `app/25000 · 5 param(s)`.
//!
//! Here it is a list, docked under the system's Route page — which is the
//! live version of the same information, with meters and the routing
//! controls. This adds what that page has no room for: sample rates,
//! transports, and what the system's defaults are.

use dioxus::prelude::*;
use patchbay_proto::DeviceView;

use super::console::Params;

/// One `device/<uid>/…` group, gathered.
#[derive(Debug, Clone, PartialEq)]
struct HostDeviceRow {
    uid: String,
    name: String,
    kind: String,
    inputs: String,
    outputs: String,
    sample_rate: String,
    transport: String,
    running: bool,
}

/// Aggregates Patchbay builds to run a mix or a monitor. They are real
/// devices and the adapter is right to report them, but they exist only
/// while a mix runs — listing them here is like listing a program's
/// temporary files. The user's own aggregates (`patchbay.aggregate.*`)
/// are deliberately not in this list.
const INTERNAL_PREFIXES: [&str; 2] = ["patchbay.mix.", "patchbay.monitor."];

fn internal(uid: &str) -> bool {
    INTERNAL_PREFIXES.iter().any(|p| uid.starts_with(p))
}

/// The uids the adapter reported, in the order it reported them.
fn device_uids(view: &DeviceView) -> Vec<String> {
    let mut uids = Vec::new();
    for p in &view.params {
        let Some(rest) = p.path.strip_prefix("device/") else {
            continue;
        };
        // `device/<uid>/<leaf>` — a uid can itself contain slashes
        // (`AppleHDAEngineOutput:1B,0,1,2:0` does not, but Core Audio
        // makes no promise), so take everything before the last one.
        let Some((uid, _leaf)) = rest.rsplit_once('/') else {
            continue;
        };
        if !internal(uid) && !uids.iter().any(|u| u == uid) {
            uids.push(uid.to_owned());
        }
    }
    uids
}

fn rows(view: &DeviceView) -> Vec<HostDeviceRow> {
    let params = Params::index(view);
    device_uids(view)
        .into_iter()
        .map(|uid| {
            let at = |leaf: &str| params.show(&format!("device/{uid}/{leaf}"));
            HostDeviceRow {
                name: {
                    let n = at("name");
                    if n.is_empty() { uid.clone() } else { n }
                },
                kind: at("kind"),
                inputs: at("inputs"),
                outputs: at("outputs"),
                sample_rate: at("sample_rate"),
                transport: at("transport"),
                running: params
                    .toggle(&format!("device/{uid}/running"))
                    .unwrap_or(false),
                uid,
            }
        })
        .collect()
}

#[component]
pub fn CoreAudioView(view: DeviceView) -> Element {
    let params = Params::index(&view);
    let backend = params.text("backend").unwrap_or_default();
    let default_out = params.text("default/output").unwrap_or_default();
    let default_in = params.text("default/input").unwrap_or_default();
    let rows = rows(&view);

    rsx! {
        div { class: "console",
            div { class: "console-bar",
                span { class: "dim-note", "backend: {backend}" }
                if !default_out.is_empty() {
                    span { class: "mix-badge", "default out: {default_out}" }
                }
                if !default_in.is_empty() {
                    span { class: "mix-badge", "default in: {default_in}" }
                }
            }
            div { class: "host-devices",
                for d in rows {
                    div { class: "device-row", key: "{d.uid}",
                        crate::ui::StatusDot {
                            status: if d.running { crate::ui::Status::Ok } else { crate::ui::Status::Idle },
                            title: if d.running { "in use" } else { "idle" },
                        }
                        div { class: "device-name", title: "{d.uid}", "{d.name}" }
                        span { class: "dim-note", "{d.inputs} in / {d.outputs} out" }
                        if !d.sample_rate.is_empty() {
                            span { class: "dim-note", "{d.sample_rate} Hz" }
                        }
                        if !d.transport.is_empty() {
                            span { class: "mix-badge", "{d.transport}" }
                        }
                        if !d.kind.is_empty() {
                            span { class: "mix-badge", "{d.kind}" }
                        }
                    }
                }
            }
            p { class: "dim-note console-note",
                "Read-only by design — Patchbay never changes the system's defaults."
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

    fn view(params: Vec<ParamView>) -> DeviceView {
        DeviceView {
            summary: DeviceSummary {
                id: "host:coreaudio:system-audio".into(),
                name: "system-audio".into(),
                kind: "system-audio".into(),
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
    fn devices_gather_out_of_the_flat_param_paths() {
        let v = view(vec![
            text("backend", "coreaudio"),
            text("device/Galaxy_UID/name", "Galaxy32"),
            text("device/Galaxy_UID/inputs", "64"),
            text("device/Galaxy_UID/outputs", "64"),
            text("device/Galaxy_UID/kind", "hardware"),
            text("device/Broadcast_UID/name", "Broadcast"),
            text("device/Broadcast_UID/inputs", "2"),
            // App params belong to a different section and must not
            // turn into devices.
            text("app/25000/name", "REAPER"),
        ]);
        let r = rows(&v);
        assert_eq!(r.len(), 2);
        let first = r.first().expect("galaxy");
        assert_eq!(first.name, "Galaxy32");
        assert_eq!(first.inputs, "64");
        assert_eq!(first.kind, "hardware");
        assert_eq!(r.get(1).map(|d| d.name.as_str()), Some("Broadcast"));
    }

    #[test]
    fn patchbays_own_plumbing_stays_out_of_the_device_list() {
        let v = view(vec![
            text(
                "device/patchbay.mix.tap-1.4242/name",
                "patchbay mix Discord",
            ),
            text("device/patchbay.monitor.probe.1/name", "patchbay meters"),
            text("device/patchbay.aggregate.REAPER-IO/name", "REAPER I/O"),
            text("device/Galaxy_UID/name", "Galaxy32"),
        ]);
        let names: Vec<String> = rows(&v).into_iter().map(|d| d.name).collect();
        // The user's own aggregate stays; the mix and meter plumbing goes.
        assert_eq!(names, vec!["REAPER I/O".to_owned(), "Galaxy32".to_owned()]);
    }

    #[test]
    fn a_device_with_no_name_still_lists_under_its_uid() {
        let v = view(vec![text("device/Odd_UID/inputs", "1")]);
        assert_eq!(rows(&v).first().map(|d| d.name.as_str()), Some("Odd_UID"));
    }
}
