//! Now — what is making sound on this machine, and where it is going.
//!
//! The front door. Every app with an audio client, live levels, the
//! device each one plays to, and one click to capture any of them into
//! a mix. Under that: the mixes, the virtual devices apps can play into
//! or listen to, and the real devices.
//!
//! One `host_overview` RPC feeds the whole screen; `app_meters` polls
//! separately at meter rate because it is small and hot. Metering taps
//! exist only while this view is mounted and polling — see
//! `MixHub::app_meters`.
//!
//! `patchbay-proto` only, so it runs the same in the desktop shell and
//! the browser remote.

use std::collections::HashMap;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use patchbay_proto::{
    HostApp, HostDevice, HostOverview, HostProblem, MixConfig, MixSourceConfig, MixView,
};

use crate::state::{self, PatchbayHandle};
use crate::ui::level::{SILENT_DB, fall, peak_db};
use crate::ui::{ConfirmButton, EmptyState, ErrorBar, LevelBar, Status, StatusDot};

/// Meter poll period.
const METER_POLL_MS: u32 = 250;
/// Overview refresh, in meter polls (≈ 1 s).
const OVERVIEW_EVERY: u32 = 4;

// ─── State ──────────────────────────────────────────────────────────────

static OVERVIEW: GlobalSignal<HostOverview> = Signal::global(HostOverview::default);
/// `host_overview` answered at least once (until then "unsupported" is
/// unknown, not false).
static LOADED: GlobalSignal<bool> = Signal::global(|| false);
/// Displayed level per bundle id, dBFS with fall-off.
static LEVELS: GlobalSignal<HashMap<String, f64>> = Signal::global(HashMap::new);
static ERROR: GlobalSignal<String> = Signal::global(String::new);
/// Show apps that hold an audio client but aren't playing.
static SHOW_IDLE: GlobalSignal<bool> = Signal::global(|| false);
/// Bundle id whose "capture into…" picker is open.
static CAPTURING: GlobalSignal<Option<String>> = Signal::global(|| None);

// ─── Data ───────────────────────────────────────────────────────────────

async fn refresh(handle: &PatchbayHandle) {
    match handle.0.host_overview().await {
        Ok(o) => {
            if *OVERVIEW.peek() != o {
                *OVERVIEW.write() = o;
            }
            if !*LOADED.peek() {
                *LOADED.write() = true;
            }
        }
        Err(e) => *ERROR.write() = format!("host overview: {e}"),
    }
}

async fn poll_levels(handle: &PatchbayHandle) {
    let Ok(meters) = handle.0.app_meters().await else {
        return;
    };
    let prev = LEVELS.peek().clone();
    let next: HashMap<String, f64> = meters
        .into_iter()
        .map(|m| {
            let db = fall(prev.get(&m.bundle_id).copied(), peak_db(&[m.peak]));
            (m.bundle_id, db)
        })
        .collect();
    if *LEVELS.peek() != next {
        *LEVELS.write() = next;
    }
}

/// Add `bundle_id` to `mix` as a stereo-mixdown source and save.
fn capture_into(handle: PatchbayHandle, mix: MixConfig, bundle_id: String) {
    let mut cfg = mix;
    if cfg
        .sources
        .iter()
        .any(|s| s.is_app() && s.target == bundle_id && s.device.is_empty())
    {
        *CAPTURING.write() = None;
        return;
    }
    cfg.sources.push(MixSourceConfig::app(bundle_id));
    *CAPTURING.write() = None;
    spawn_forever(async move {
        match handle.0.save_mix(cfg).await {
            Ok(_) => refresh(&handle).await,
            Err(e) => *ERROR.write() = format!("capture: {e}"),
        }
    });
}

/// Make a new mix carrying just this app, into the first virtual device
/// that can be an output (the "Broadcast" mic, normally).
fn capture_into_new(handle: PatchbayHandle, app: &HostApp) {
    let o = OVERVIEW.peek();
    let output = o
        .virtual_devices
        .devices
        .iter()
        .find(|d| d.channels >= 2)
        .map(|d| d.uid.clone());
    let Some(device) = output else {
        "no virtual device to send a new mix to — make one first".clone_into(&mut ERROR.write());
        *CAPTURING.write() = None;
        return;
    };
    let name = unique_mix_name(&o.mixes, &app.name);
    let cfg = MixConfig {
        name,
        channels: 2,
        sources: Vec::new(),
        outputs: vec![patchbay_proto::MixOutputConfig::new(device)],
        enabled: None,
    };
    drop(o);
    capture_into(handle, cfg, app.bundle_id.clone());
}

/// `REAPER`, then `REAPER 2`, … — never silently replace a saved mix.
fn unique_mix_name(mixes: &[MixView], base: &str) -> String {
    let taken = |n: &str| mixes.iter().any(|m| m.config.name == n);
    if !taken(base) {
        return base.to_owned();
    }
    // Bounded: with N saved mixes one of the first N+1 candidates is
    // always free.
    (2_u32..=u32::try_from(mixes.len().saturating_add(2)).unwrap_or(u32::MAX))
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| base.to_owned())
}

// ─── Pure helpers ───────────────────────────────────────────────────────

/// Where an app's audio is going (and coming from), by device name.
fn app_route(app: &HostApp, devices: &[HostDevice]) -> String {
    let name = |uid: &String| {
        devices
            .iter()
            .find(|d| &d.uid == uid)
            .map_or_else(|| uid.clone(), |d| d.name.clone())
    };
    let mut parts: Vec<String> = app
        .output_devices
        .iter()
        .map(|u| format!("→ {}", name(u)))
        .collect();
    parts.extend(app.input_devices.iter().map(|u| format!("← {}", name(u))));
    parts.join("  ")
}

/// A device's badges: what the system uses it for, and whether anything
/// has it open.
fn device_tags(d: &HostDevice) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if d.is_default_output() {
        tags.push("default out");
    }
    if d.is_default_input() {
        tags.push("default in");
    }
    if d.in_use {
        tags.push("in use");
    }
    tags
}

/// One line describing a mix: what feeds it, where it goes.
fn mix_summary(m: &MixView) -> String {
    let outs: Vec<&str> = m.outputs.iter().map(|o| o.device_name.as_str()).collect();
    let sources = m.config.sources.len();
    let plural = if sources == 1 { "source" } else { "sources" };
    if outs.is_empty() {
        format!("{sources} {plural}")
    } else {
        format!("{sources} {plural} → {}", outs.join(", "))
    }
}

// ─── View ───────────────────────────────────────────────────────────────

#[component]
pub fn NowView() -> Element {
    let handle = state::use_patchbay();
    let poll = handle;
    use_future(move || {
        let handle = poll.clone();
        async move {
            let mut tick: u32 = 0;
            loop {
                if tick == 0 {
                    refresh(&handle).await;
                }
                poll_levels(&handle).await;
                tick = tick.saturating_add(1) % OVERVIEW_EVERY;
                crate::state::sleep_ms(METER_POLL_MS).await;
            }
        }
    });

    let o = OVERVIEW.read().clone();
    let loaded = *LOADED.read();
    let show_idle = *SHOW_IDLE.read();
    let error = ERROR.read().clone();

    if loaded && !o.supported {
        return rsx! {
            div { class: "view-body",
                EmptyState { title: "Host audio needs macOS",
                    p {
                        "Apps, taps and virtual devices come from Core Audio. On Linux the "
                        "PipeWire graph in the Graph view is the router."
                    }
                }
            }
        };
    }

    let shown: Vec<HostApp> = o
        .apps
        .iter()
        .filter(|a| show_idle || a.playing || a.recording)
        .cloned()
        .collect();

    rsx! {
        div { class: "view-head",
            span { class: "view-title", "Now" }
            span { class: "view-sub", "what is making sound on this machine" }
            div { class: "view-head-actions",
                button {
                    class: if show_idle { "chip on" } else { "chip" },
                    title: "Apps holding an audio client without playing — browsers and daemons mostly",
                    onclick: move |_| {
                        let v = *SHOW_IDLE.peek();
                        *SHOW_IDLE.write() = !v;
                    },
                    "show idle apps"
                }
            }
        }
        ErrorBar { message: error, on_dismiss: move |()| ERROR.write().clear() }
        div { class: "view-body now-body",
            if !o.problems.is_empty() {
                div { class: "problems",
                    for (i, p) in o.problems.iter().enumerate() {
                        Problem { key: "{i}", problem: p.clone() }
                    }
                }
            }

            section { class: "now-section",
                h3 { class: "section-label", "Apps" }
                if shown.is_empty() {
                    p { class: "dim-note",
                        if loaded { "Nothing is playing." } else { "Reading the host…" }
                    }
                }
                for app in shown {
                    AppRow { key: "{app.bundle_id}", app, devices: o.devices.clone(), mixes: o.mixes.clone() }
                }
            }

            div { class: "now-columns",
                section { class: "now-section",
                    h3 { class: "section-label", "Mixes" }
                    if o.mixes.is_empty() {
                        p { class: "dim-note", "No mixes yet — capture an app above to make one." }
                    }
                    for m in o.mixes.iter() {
                        MixCard { key: "{m.config.name}", mix: m.clone() }
                    }
                }
                section { class: "now-section",
                    h3 { class: "section-label", "Devices" }
                    Devices { devices: o.devices.clone(), virtual_uids: virtual_uids(&o) }
                }
            }
        }
    }
}

/// UIDs published by `Patchbay.driver` — the loopbacks we own, which are
/// worth marking apart from every other virtual device on the machine.
fn virtual_uids(o: &HostOverview) -> Vec<String> {
    o.virtual_devices
        .devices
        .iter()
        .map(|d| d.uid.clone())
        .collect()
}

#[component]
fn Problem(problem: HostProblem) -> Element {
    let handle = state::use_patchbay();
    let fix = problem.fix.clone();
    rsx! {
        div { class: if problem.severity == "error" { "problem bad" } else { "problem" },
            div { class: "problem-text",
                span { class: "problem-summary", "{problem.summary}" }
                if !problem.detail.is_empty() {
                    span { class: "problem-detail", "{problem.detail}" }
                }
            }
            if fix == "request_permissions" {
                button {
                    class: "chip",
                    onclick: move |_| {
                        let handle = handle.clone();
                        spawn_forever(async move {
                            if let Err(e) = handle.0.request_permissions().await {
                                *ERROR.write() = format!("permissions: {e}");
                            }
                        });
                    },
                    "Ask again"
                }
            }
        }
    }
}

#[component]
fn AppRow(app: HostApp, devices: Vec<HostDevice>, mixes: Vec<MixView>) -> Element {
    let handle = state::use_patchbay();
    let db = LEVELS
        .read()
        .get(&app.bundle_id)
        .copied()
        .unwrap_or(SILENT_DB);
    let status = if app.playing {
        Status::Ok
    } else if app.recording {
        Status::Busy
    } else {
        Status::Idle
    };
    let what = if app.playing {
        "playing"
    } else if app.recording {
        "recording"
    } else {
        "idle"
    };
    let route = app_route(&app, &devices);
    let capturing = CAPTURING.read().as_deref() == Some(app.bundle_id.as_str());
    let bundle = app.bundle_id.clone();
    let color = state::auto_color(&app.name);
    let initial = app
        .name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();

    rsx! {
        div { class: "app-row",
            div { class: "app-badge", style: "background: {color};", "{initial}" }
            StatusDot { status, title: "{what}" }
            div { class: "app-name", title: "{app.bundle_id}", "{app.name}" }
            LevelBar { db }
            div { class: "app-route", "{route}" }
            button {
                class: if capturing { "chip on" } else { "chip" },
                title: "Send this app's audio into a mix",
                onclick: move |_| {
                    let open = CAPTURING.peek().as_deref() == Some(bundle.as_str());
                    *CAPTURING.write() = (!open).then(|| bundle.clone());
                },
                "capture"
            }
        }
        if capturing {
            div { class: "capture-picker",
                span { class: "dim-note", "into:" }
                for m in mixes.iter() {
                    {
                        let handle = handle.clone();
                        let cfg = m.config.clone();
                        let bundle = app.bundle_id.clone();
                        rsx! {
                            button {
                                key: "{m.config.name}",
                                class: "chip",
                                onclick: move |_| capture_into(handle.clone(), cfg.clone(), bundle.clone()),
                                "{m.config.name}"
                            }
                        }
                    }
                }
                {
                    let app = app.clone();
                    rsx! {
                        button {
                            class: "chip",
                            onclick: move |_| capture_into_new(handle.clone(), &app),
                            "a new mix"
                        }
                    }
                }
                button {
                    class: "chip",
                    onclick: move |_| *CAPTURING.write() = None,
                    "cancel"
                }
            }
        }
    }
}

#[component]
fn MixCard(mix: MixView) -> Element {
    let handle = state::use_patchbay();
    let enabled = mix.config.is_enabled();
    let (badge, badge_class) = if !enabled {
        ("off", "mix-badge off")
    } else if mix.running {
        ("running", "mix-badge live")
    } else {
        ("stopped", "mix-badge bad")
    };
    let name = mix.config.name.clone();
    let cfg = mix.config.clone();
    rsx! {
        div { class: "mix-card",
            div { class: "mix-card-top",
                button {
                    class: "mix-card-name",
                    title: "Open in Mixes",
                    onclick: move |_| crate::mixes::open(&name),
                    "{mix.config.name}"
                }
                span { class: "{badge_class}", "{badge}" }
            }
            div { class: "dim-note", "{mix_summary(&mix)}" }
            if !mix.error.is_empty() {
                div { class: "mix-error", "{mix.error}" }
            }
            div { class: "mix-card-actions",
                button {
                    class: if enabled { "chip on" } else { "chip" },
                    onclick: move |_| {
                        let handle = handle.clone();
                        let mut cfg = cfg.clone();
                        cfg.enabled = Some(!enabled);
                        spawn_forever(async move {
                            match handle.0.save_mix(cfg).await {
                                Ok(_) => refresh(&handle).await,
                                Err(e) => *ERROR.write() = format!("mix: {e}"),
                            }
                        });
                    },
                    if enabled { "enabled" } else { "disabled" }
                }
            }
        }
    }
}

#[component]
fn Devices(devices: Vec<HostDevice>, virtual_uids: Vec<String>) -> Element {
    let mine = |d: &HostDevice| virtual_uids.contains(&d.uid);
    let any_ours = devices.iter().any(mine);
    rsx! {
        if any_ours {
            div { class: "device-group-label", "Patchbay" }
            for d in devices.iter().filter(|d| mine(d)) {
                DeviceRow { key: "{d.uid}", device: d.clone(), ours: true }
            }
        }
        div { class: "device-group-label", "System" }
        for d in devices.iter().filter(|d| !mine(d)) {
            DeviceRow { key: "{d.uid}", device: d.clone(), ours: false }
        }
    }
}

#[component]
fn DeviceRow(device: HostDevice, ours: bool) -> Element {
    let handle = state::use_patchbay();
    let tags = device_tags(&device);
    let uid = device.uid.clone();
    rsx! {
        div { class: "device-row",
            StatusDot {
                status: if device.in_use { Status::Ok } else { Status::Idle },
                title: if device.in_use { "in use" } else { "idle" },
            }
            div { class: "device-name", title: "{device.uid}", "{device.name}" }
            span { class: "dim-note",
                "{device.input_channels} in / {device.output_channels} out"
            }
            for t in tags {
                span { class: "mix-badge", "{t}" }
            }
            if ours {
                ConfirmButton {
                    label: "remove".to_owned(),
                    armed_label: "sure?".to_owned(),
                    on_confirm: move |()| {
                        let handle = handle.clone();
                        let uid = uid.clone();
                        spawn_forever(async move {
                            match handle.0.remove_virtual_device(uid).await {
                                Ok(()) => refresh(&handle).await,
                                Err(e) => *ERROR.write() = format!("remove: {e}"),
                            }
                        });
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, outs: &[&str], ins: &[&str]) -> HostApp {
        HostApp {
            bundle_id: format!("com.test.{name}"),
            name: name.to_owned(),
            playing: true,
            output_devices: outs.iter().map(|s| (*s).to_owned()).collect(),
            input_devices: ins.iter().map(|s| (*s).to_owned()).collect(),
            ..HostApp::default()
        }
    }

    fn device(uid: &str, name: &str) -> HostDevice {
        HostDevice {
            uid: uid.to_owned(),
            name: name.to_owned(),
            ..HostDevice::default()
        }
    }

    #[test]
    fn routes_read_as_directions_with_device_names() {
        let devices = vec![device("g32", "Galaxy32"), device("spk", "Speakers")];
        assert_eq!(
            app_route(&app("REAPER", &["g32"], &["g32"]), &devices),
            "→ Galaxy32  ← Galaxy32"
        );
        assert_eq!(
            app_route(&app("Brave", &["spk"], &[]), &devices),
            "→ Speakers"
        );
        assert_eq!(app_route(&app("Quiet", &[], &[]), &devices), "");
        // An unknown uid still says something rather than vanishing.
        assert_eq!(app_route(&app("X", &["ghost"], &[]), &devices), "→ ghost");
    }

    #[test]
    fn a_new_mix_never_overwrites_a_saved_one() {
        let mixes = vec![MixView {
            config: MixConfig {
                name: "REAPER".into(),
                channels: 2,
                sources: Vec::new(),
                outputs: Vec::new(),
                enabled: None,
            },
            running: false,
            error: String::new(),
            sources: Vec::new(),
            outputs: Vec::new(),
        }];
        assert_eq!(unique_mix_name(&mixes, "REAPER"), "REAPER 2");
        assert_eq!(unique_mix_name(&mixes, "Brave"), "Brave");
    }
}
