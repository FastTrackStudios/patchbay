//! Devices view — external hardware (Antelope Galaxy32, Yamaha TF, …)
//! through the generic device model.
//!
//! Left: the device's params grouped into sections by path prefix
//! (`mixer/1`, `monitor`, `trim`, …); each row is one parent path, so a
//! Galaxy32 mixer strip (`mixer/1/strip/16/{level,pan,mute,solo,send}`)
//! renders as a fader row. Right: the router as a crosspoint grid (one
//! output group × one source group at a time, `DanteGrid` style) and
//! named snapshots with a diff preview before any restore.
//!
//! Everything goes through `PatchbayService` (`patchbay-proto` only), so
//! it runs the same in the desktop shell and the wasm remote. Writes are
//! answered with the device's read-back, which is what the view shows.

use std::collections::{BTreeMap, HashSet};

use dioxus::prelude::*;
use patchbay_proto::{
    DeviceChannel, DeviceEventKind, DeviceEventWire, DeviceLinkState, DeviceParamKind,
    DeviceParamValue, DeviceRestoreReport, DeviceRestoreStatus, DeviceSnapshotInfo, DeviceSummary,
    DeviceView, ParamView, source_label,
};

pub mod console;
mod core_audio;
mod galaxy32;
mod strip;
mod yamaha_tf;

use crate::state::{self, PatchbayHandle};
use crate::ui::{EmptyState, ErrorBar, Status, StatusDot};

// ─── State ──────────────────────────────────────────────────────────────

static DEVICES: GlobalSignal<Vec<DeviceSummary>> = Signal::global(Vec::new);
/// Selected device (full id, or config name while unidentified).
static SELECTED: GlobalSignal<Option<String>> = Signal::global(|| None);
/// Last full read of the selected device.
static VIEW: GlobalSignal<Option<DeviceView>> = Signal::global(|| None);
static SNAPSHOTS: GlobalSignal<Vec<DeviceSnapshotInfo>> = Signal::global(Vec::new);
/// Last diff / restore report.
static REPORT: GlobalSignal<Option<DeviceRestoreReport>> = Signal::global(|| None);
static ERROR: GlobalSignal<String> = Signal::global(String::new);
static LOADING: GlobalSignal<bool> = Signal::global(|| false);
/// Bumped by events that need a full re-read (online, snapshot replaced).
static STALE: GlobalSignal<u64> = Signal::global(|| 0);
/// Unlocks writes to params flagged disruptive (clock, scene recall).
static ALLOW_DISRUPTIVE: GlobalSignal<bool> = Signal::global(|| false);
static FILTER: GlobalSignal<String> = Signal::global(String::new);
static OPEN_SECTIONS: GlobalSignal<HashSet<String>> = Signal::global(HashSet::new);
static OUT_GROUP: GlobalSignal<String> = Signal::global(String::new);
static SRC_GROUP: GlobalSignal<String> = Signal::global(String::new);
static SNAP_NAME: GlobalSignal<String> = Signal::global(String::new);
static SNAP_INCLUDE: GlobalSignal<String> = Signal::global(String::new);

/// Fold one device event into the view (call from the shell's
/// `device_events` subscription).
pub fn apply_device_event(ev: &DeviceEventWire) {
    match &ev.event {
        DeviceEventKind::Online | DeviceEventKind::Offline => {
            let online = matches!(ev.event, DeviceEventKind::Online);
            for d in DEVICES.write().iter_mut().filter(|d| d.id == ev.device) {
                d.state = if online {
                    DeviceLinkState::Online
                } else {
                    DeviceLinkState::Offline
                };
            }
            bump_stale();
        }
        DeviceEventKind::SnapshotReplaced => bump_stale(),
        DeviceEventKind::ParamChanged { path, value } => {
            let mut view = VIEW.write();
            if let Some(v) = view.as_mut().filter(|v| v.summary.id == ev.device)
                && let Some(p) = v.params.iter_mut().find(|p| &p.path == path)
            {
                p.value = value.clone();
            }
        }
        DeviceEventKind::RouteChanged { crosspoint } => {
            let mut view = VIEW.write();
            if let Some(v) = view.as_mut().filter(|v| v.summary.id == ev.device)
                && let Some(c) = v.routes.iter_mut().find(|c| c.output == crosspoint.output)
            {
                c.source.clone_from(&crosspoint.source);
            }
        }
    }
}

/// Human word for a link state (chip tooltip).
const fn state_label(s: DeviceLinkState) -> &'static str {
    match s {
        DeviceLinkState::Connecting => "connecting",
        DeviceLinkState::Online => "online",
        DeviceLinkState::Offline => "offline",
        DeviceLinkState::Disabled => "disabled",
        DeviceLinkState::Searching => "searching",
        DeviceLinkState::NotFound => "not found",
    }
}

/// Forget the previous engine's devices (see `hosts::reset`). A device
/// the switch asked for is selected up front, so the first poll loads it.
pub fn reset() {
    DEVICES.write().clear();
    *SELECTED.write() = crate::hosts::PENDING_DEVICE.write().take();
    *VIEW.write() = None;
    SNAPSHOTS.write().clear();
    *REPORT.write() = None;
    ERROR.write().clear();
    *LOADING.write() = false;
    OUT_GROUP.write().clear();
    SRC_GROUP.write().clear();
    OPEN_SECTIONS.write().clear();
    *CONTEXT_MENU.write() = false;
}

fn bump_stale() {
    let mut s = STALE.write();
    *s = s.wrapping_add(1);
}

pub fn device_key(d: &DeviceSummary) -> String {
    if d.id.is_empty() {
        d.name.clone()
    } else {
        d.id.clone()
    }
}

async fn refresh_list(handle: &PatchbayHandle) {
    let (list, snaps) =
        futures_util::join!(handle.0.list_devices(), handle.0.list_device_snapshots());
    match list {
        Ok(list) => {
            if SELECTED.peek().is_none()
                && let Some(first) = list.iter().find(|d| d.state == DeviceLinkState::Online)
            {
                *SELECTED.write() = Some(device_key(first));
            }
            if *DEVICES.peek() != list {
                *DEVICES.write() = list;
            }
        }
        Err(e) => *ERROR.write() = format!("list devices: {e}"),
    }
    if let Ok(snaps) = snaps {
        *SNAPSHOTS.write() = snaps;
    }
}

async fn load_selected(handle: &PatchbayHandle) {
    let Some(id) = SELECTED.peek().clone() else {
        return;
    };
    if !loads_a_view() {
        return;
    }
    *LOADING.write() = true;
    match handle.0.device(id).await {
        Ok(v) => {
            if OUT_GROUP.peek().is_empty()
                && let Some(g) = v.outputs.first()
            {
                OUT_GROUP.write().clone_from(&g.id);
            }
            *VIEW.write() = Some(v);
            ERROR.write().clear();
        }
        Err(e) => {
            *VIEW.write() = None;
            *ERROR.write() = e.to_string();
        }
    }
    *LOADING.write() = false;
}

fn set_param(handle: PatchbayHandle, path: String, value: DeviceParamValue) {
    let Some(id) = VIEW.peek().as_ref().map(|v| v.summary.id.clone()) else {
        return;
    };
    let allow = *ALLOW_DISRUPTIVE.peek();
    spawn(async move {
        match handle
            .0
            .set_device_param(id, path.clone(), value, allow)
            .await
        {
            Ok(p) => {
                let mut view = VIEW.write();
                if let Some(slot) = view
                    .as_mut()
                    .and_then(|v| v.params.iter_mut().find(|x| x.path == p.path))
                {
                    *slot = p;
                }
                ERROR.write().clear();
            }
            Err(e) => *ERROR.write() = format!("{path}: {e}"),
        }
    });
}

fn set_route(handle: PatchbayHandle, output: DeviceChannel, source: Option<DeviceChannel>) {
    let Some(id) = VIEW.peek().as_ref().map(|v| v.summary.id.clone()) else {
        return;
    };
    spawn(async move {
        match handle.0.set_device_route(id, output.clone(), source).await {
            Ok(cp) => {
                let mut view = VIEW.write();
                if let Some(c) = view
                    .as_mut()
                    .and_then(|v| v.routes.iter_mut().find(|c| c.output == cp.output))
                {
                    c.source = cp.source;
                }
                ERROR.write().clear();
            }
            Err(e) => *ERROR.write() = format!("route {}: {e}", output.label()),
        }
    });
}

// ─── Grouping ───────────────────────────────────────────────────────────

/// Section key: first segment, plus the second when it is an index
/// (`mixer/1`, `in/3`), so a section is one mixer / one channel.
fn section_of(path: &str) -> String {
    let mut it = path.split('/');
    let first = it.next().unwrap_or_default();
    let second = it.next();
    let deeper = it.next().is_some();
    match second {
        Some(s) if deeper && s.chars().all(|c| c.is_ascii_digit()) => format!("{first}/{s}"),
        _ => first.to_owned(),
    }
}

/// Row key: the parent path (`mixer/1/strip/16`).
fn row_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(parent, _)| parent)
}

fn leaf_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, leaf)| leaf)
}

/// Sections → rows → params, in first-seen (device) order.
fn group_params(
    params: &[ParamView],
    filter: &str,
) -> Vec<(String, Vec<(String, Vec<ParamView>)>)> {
    let f = filter.trim().to_lowercase();
    let mut sections: Vec<(String, Vec<(String, Vec<ParamView>)>)> = Vec::new();
    for p in params {
        if !f.is_empty()
            && !p.path.to_lowercase().contains(&f)
            && !p.label.to_lowercase().contains(&f)
        {
            continue;
        }
        let sec = section_of(&p.path);
        let row = row_of(&p.path).to_owned();
        let i = sections
            .iter()
            .position(|(s, _)| *s == sec)
            .unwrap_or_else(|| {
                sections.push((sec, Vec::new()));
                sections.len().saturating_sub(1)
            });
        let Some((_, rows)) = sections.get_mut(i) else {
            continue;
        };
        match rows.iter_mut().find(|(r, _)| *r == row) {
            Some((_, ps)) => ps.push(p.clone()),
            None => rows.push((row, vec![p.clone()])),
        }
    }
    sections
}

// ─── Components ─────────────────────────────────────────────────────────

// ─── Context ────────────────────────────────────────────────────────────
//
// The selected device is the app's *context*: Route and Mix both act on
// it. The rail shows it and switches it, so the list and the selected
// device are kept fresh from the app root, not from whichever page
// happens to be mounted.

/// The host's own audio layer (Core Audio / the `PipeWire` graph).
pub const KIND_SYSTEM: &str = "system-audio";
/// The Dante network, as one device.
pub const KIND_DANTE: &str = "dante";

/// The context switcher is open.
pub static CONTEXT_MENU: GlobalSignal<bool> = Signal::global(|| false);

/// Every configured device, for the context switcher.
pub fn contexts() -> Vec<DeviceSummary> {
    DEVICES.read().clone()
}

/// The device Route and Mix are about. `None` until the list has been
/// read (or where the engine has no device layer at all).
pub fn context() -> Option<DeviceSummary> {
    let selected = SELECTED.read().clone()?;
    DEVICES
        .read()
        .iter()
        .find(|d| device_key(d) == selected)
        .cloned()
}

/// A context's name at rail width: what the device IS — `Core Audio`,
/// `TF1`, `Galaxy32` — rather than what the config calls it. Named for
/// the thing and not as "System", because one rail is meant to hold
/// several hosts side by side (Core Audio on one machine, `PipeWire` on
/// another).
pub fn context_label(d: &DeviceSummary) -> String {
    match d.kind.as_str() {
        // "Dante network" doesn't fit the rail; the icon says network.
        KIND_DANTE => "Dante".to_owned(),
        _ if d.model.is_empty() => d.name.clone(),
        _ => d.model.clone(),
    }
}

pub const fn context_status(d: &DeviceSummary) -> Status {
    match d.state {
        DeviceLinkState::Online => Status::Ok,
        DeviceLinkState::Connecting | DeviceLinkState::Searching => Status::Busy,
        DeviceLinkState::Offline => Status::Bad,
        DeviceLinkState::Disabled | DeviceLinkState::NotFound => Status::Missing,
    }
}

/// `online`, `not found`, … plus where and why when there is more to say.
pub fn context_detail(d: &DeviceSummary) -> String {
    let mut out = state_label(d.state).to_owned();
    if !d.transport.is_empty() {
        out = format!("{out} · {}", d.transport);
    }
    if !d.error.is_empty() {
        out = format!("{out} · {}", d.error);
    }
    out
}

/// The context's key, for remembering it (`crate::session`).
pub fn selected_key() -> Option<String> {
    SELECTED.read().clone()
}

/// Select a device by key before its engine has listed it — restoring a
/// session. If it no longer exists the page says so, and the rail offers
/// the ones that do.
pub fn prefer(key: String) {
    if SELECTED.peek().as_deref() != Some(key.as_str()) {
        *SELECTED.write() = Some(key);
        *VIEW.write() = None;
    }
}

/// Make `d` the context.
pub fn select_context(handle: PatchbayHandle, d: &DeviceSummary) {
    *CONTEXT_MENU.write() = false;
    // A deliberate pick outranks wherever the last session was headed.
    *crate::session::WANT.write() = None;
    let key = device_key(d);
    if SELECTED.peek().as_deref() == Some(key.as_str()) {
        return;
    }
    *SELECTED.write() = Some(key);
    *VIEW.write() = None;
    OUT_GROUP.write().clear();
    SRC_GROUP.write().clear();
    *REPORT.write() = None;
    ERROR.write().clear();
    spawn(async move { load_selected(&handle).await });
}

/// Make the host's own audio the context — how another view hands off to
/// something that only exists there (a host mix).
pub fn select_system() {
    let system = DEVICES
        .peek()
        .iter()
        .find(|d| d.kind == KIND_SYSTEM)
        .map(device_key);
    if let Some(key) = system
        && SELECTED.peek().as_deref() != Some(key.as_str())
    {
        *SELECTED.write() = Some(key);
        *VIEW.write() = None;
    }
}

/// Keeps the device list and the context's view fresh: a cheap poll for
/// link state, a full re-read when events say so. Call once, from the
/// app root.
pub fn use_device_poll() {
    let handle = state::use_patchbay();
    use_future(move || {
        let handle = handle.clone();
        async move {
            refresh_list(&handle).await;
            load_selected(&handle).await;
            let mut seen_stale = *STALE.peek();
            let mut tick = 0_u32;
            loop {
                state::sleep_secs(1).await;
                tick = tick.wrapping_add(1);
                let stale = *STALE.peek();
                let unloaded = VIEW.peek().is_none() && loads_a_view();
                if tick.is_multiple_of(5) || stale != seen_stale {
                    refresh_list(&handle).await;
                }
                if stale != seen_stale || (unloaded && tick.is_multiple_of(5)) {
                    seen_stale = stale;
                    load_selected(&handle).await;
                }
            }
        }
    });
}

/// Whether the context is a device whose parameter view a page draws.
/// Dante is drawn from the subscription scan instead, and reading it as
/// a device costs a second ARC sweep of the network for nothing.
fn loads_a_view() -> bool {
    let Some(selected) = SELECTED.peek().clone() else {
        return false;
    };
    DEVICES
        .peek()
        .iter()
        .find(|d| device_key(d) == selected)
        .is_some_and(|d| d.kind != KIND_DANTE)
}

// ─── Pages ──────────────────────────────────────────────────────────────

/// What a device page shows: its console, or why there isn't one yet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DevicePage {
    Route,
    Mix,
}

/// A hardware device's router.
#[component]
pub fn DeviceRoute() -> Element {
    rsx! { DeviceShell { page: DevicePage::Route } }
}

/// A hardware device's console: strips, mixers, and everything else it
/// has that isn't patching.
#[component]
pub fn DeviceMix() -> Element {
    rsx! { DeviceShell { page: DevicePage::Mix } }
}

#[component]
fn DeviceShell(page: DevicePage) -> Element {
    let handle = state::use_patchbay();
    let summary = context();
    let error = ERROR.read().clone();
    let loading = *LOADING.read();
    let view = VIEW.read().clone();

    let reload = move |_| {
        let handle = handle.clone();
        spawn(async move {
            refresh_list(&handle).await;
            load_selected(&handle).await;
        });
    };
    let (title, sub) = match page {
        DevicePage::Route => ("Route", "this device's own patching"),
        DevicePage::Mix => ("Mix", "the device, as itself"),
    };

    rsx! {
        div { class: "devices-view",
            div { class: "view-head",
                span { class: "view-title", "{title}" }
                ContextTag {}
                span { class: "view-sub", "{sub}" }
                div { class: "view-head-actions",
                    // Only a console has disruptive parameters (clock, scene
                    // recall); a router has nothing this would unlock.
                    if page == DevicePage::Mix {
                        label { class: "label disruptive-toggle",
                            title: "Clock changes and scene recall interrupt audio, so they are locked until you say so",
                            input {
                                r#type: "checkbox",
                                checked: *ALLOW_DISRUPTIVE.read(),
                                onchange: move |e| *ALLOW_DISRUPTIVE.write() = e.checked(),
                            }
                            " disruptive writes"
                        }
                    }
                    button { class: "chip", onclick: reload,
                        if loading { "reading…" } else { "re-read" }
                    }
                }
            }
            ErrorBar { message: error, on_dismiss: move |()| ERROR.write().clear() }
            div { class: "devices-scroll",
                if let Some(v) = view {
                    match page {
                        DevicePage::Route => rsx! { RoutePane { view: v } },
                        DevicePage::Mix => rsx! {
                            Console { view: v.clone() }
                            Inspector { view: v }
                        },
                    }
                } else if loading {
                    div { class: "empty-state dim-note", "reading device…" }
                } else {
                    Offline { device: summary }
                }
            }
        }
    }
}

/// The device's router, or a plain statement that it doesn't have one
/// Patchbay can reach.
#[component]
fn RoutePane(view: DeviceView) -> Element {
    if view.outputs.is_empty() {
        let name = context_label(&view.summary);
        return rsx! {
            EmptyState { title: format!("{name} doesn't expose its patching"),
                p {
                    "This device's adapter reports no router, so there is nothing to patch "
                    "from here. Its levels are under Mix; every parameter it does report is "
                    "in the Inspector there."
                }
            }
        };
    }
    rsx! {
        div { class: "console",
            if view.summary.kind == "antelope-galaxy32" {
                p { class: "dim-note console-note", "{galaxy32::ROUTER_LAG}" }
            }
            RouterGrid { view }
        }
    }
}

/// The host's device table, docked under the system's Route page.
///
/// A drawer, shut by default: the page above it already lists these
/// devices live. This is where their sample rates and transports are.
#[component]
pub fn SystemDrawer() -> Element {
    let mut open = use_signal(|| false);
    let view = VIEW
        .read()
        .clone()
        .filter(|v| v.summary.kind == KIND_SYSTEM);
    let Some(view) = view else {
        return rsx! {};
    };
    let label = if view.summary.model.is_empty() {
        "System audio".to_owned()
    } else {
        view.summary.model.clone()
    };
    rsx! {
        div { class: "inspector docked",
            button {
                class: "inspector-toggle",
                onclick: move |_| {
                    let now = open();
                    open.set(!now);
                },
                span { class: "section-caret", if open() { "▾" } else { "▸" } }
                span { class: "section-label", "{label}" }
                span { class: "section-note", "sample rates · transports · defaults" }
            }
            if open() {
                core_audio::CoreAudioView { view }
            }
        }
    }
}

/// Which device this page is about — and the way to change it, right
/// where the title is. On a phone the subtitle is gone and the rail is a
/// tab bar, so this is what says "you are routing the TF".
#[component]
pub fn ContextTag() -> Element {
    let Some(d) = context() else {
        return rsx! {};
    };
    rsx! {
        button {
            class: "context-tag",
            title: "Working on {d.name} ({context_detail(&d)}) — click to switch device",
            onclick: move |_| *CONTEXT_MENU.write() = true,
            StatusDot { status: context_status(&d), title: context_detail(&d) }
            span { "{context_label(&d)}" }
            // Which machine — once there is more than one to be on.
            if crate::hosts::multi_host() {
                span { class: "context-tag-host", "{crate::hosts::live_name()}" }
            }
            span { class: "context-tag-caret", "▾" }
        }
    }
}

/// A device that is configured but not answering.
///
/// The adapter keeps trying, so this is a state to explain rather than
/// an error to dismiss — the console appears by itself when the device
/// comes back.
#[component]
fn Offline(device: Option<DeviceSummary>) -> Element {
    let Some(d) = device else {
        return rsx! {
            div { class: "empty-state dim-note", "Pick a device in the rail." }
        };
    };
    rsx! {
        crate::ui::EmptyState { title: format!("{} isn't answering", d.name),
            if !d.error.is_empty() {
                p { class: "dim-note", "{d.error}" }
            }
            p {
                "Patchbay keeps trying — this page fills in on its own when the device comes "
                "back. Check that it is powered on and reachable"
                if d.transport.is_empty() { "." } else { " at {d.transport}." }
            }
        }
    }
}

/// The console for this device — a real surface where we know the
/// device family, the parameter tree everywhere else.
///
/// Branches on `summary.kind`, which is the config's adapter kind, not
/// the model string: every Galaxy 32 answers to the same paths whatever
/// it calls itself.
#[component]
fn Console(view: DeviceView) -> Element {
    match view.summary.kind.as_str() {
        "yamaha-tf" => rsx! { yamaha_tf::YamahaTfConsole { view } },
        "antelope-galaxy32" => rsx! { galaxy32::Galaxy32Console { view } },
        _ => rsx! {},
    }
}

/// Everything the device reports, as it reports it.
///
/// Demoted from the whole tab to a drawer: it is the right tool for
/// reverse-engineering and for anything a console view doesn't cover,
/// and the wrong first thing to show someone who wants a fader.
#[component]
fn Inspector(view: DeviceView) -> Element {
    let mut open = use_signal(|| false);
    rsx! {
        div { class: "inspector",
            button {
                class: "inspector-toggle",
                onclick: move |_| {
                    let now = open();
                    open.set(!now);
                },
                span { class: "section-caret", if open() { "▾" } else { "▸" } }
                span { class: "section-label", "Inspector" }
                span { class: "section-note",
                    "{view.params.len()} parameters · {view.routes.len()} crosspoints"
                }
            }
            if open() {
                div { class: "devices-body",
                    ParamPanel { view: view.clone() }
                    div { class: "device-side",
                        SnapshotPanel { view }
                    }
                }
            }
        }
    }
}

#[component]
fn ParamPanel(view: DeviceView) -> Element {
    let filter = FILTER.read().clone();
    let open = OPEN_SECTIONS.read().clone();
    let sections = group_params(&view.params, &filter);
    let s = &view.summary;
    rsx! {
        div { class: "device-params",
            div { class: "device-id",
                strong { "{s.vendor} {s.model}" }
                span { class: "dim-note", " {s.id} · fw {s.firmware} · {s.transport}" }
            }
            input {
                class: "search",
                placeholder: "filter params (path or label)…",
                value: "{filter}",
                oninput: move |e| *FILTER.write() = e.value(),
            }
            for (sec, rows) in sections {
                {
                    let is_open = !filter.is_empty() || open.contains(&sec);
                    let key = sec.clone();
                    let count: usize = rows.iter().map(|(_, ps)| ps.len()).sum();
                    rsx! {
                        div { class: "param-section", key: "{sec}",
                            div {
                                class: "param-section-h",
                                onclick: move |_| {
                                    let mut o = OPEN_SECTIONS.write();
                                    if !o.remove(&key) {
                                        o.insert(key.clone());
                                    }
                                },
                                if is_open { "▾ " } else { "▸ " }
                                "{sec} "
                                span { class: "dev-count", "{rows.len()} row(s) · {count} param(s)" }
                            }
                            if is_open {
                                for (row, params) in rows {
                                    div { class: "param-row", key: "{row}",
                                        span { class: "param-row-h", title: "{row}", "{row}" }
                                        for p in params {
                                            ParamControl { key: "{p.path}", param: p }
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

/// One param as an inline control, by kind.
#[component]
fn ParamControl(param: ParamView) -> Element {
    let handle = state::use_patchbay();
    let p = param;
    let locked = !p.writable || (p.disruptive && !*ALLOW_DISRUPTIVE.read());
    let leaf = leaf_of(&p.path).to_owned();
    let shown = p.value.display(Some(&p.kind));
    let mut title = format!("{} — {}", p.path, p.label);
    if !p.writable {
        title.push_str(" (read-only)");
    }
    if p.disruptive {
        title.push_str(" (DISRUPTIVE: drops audio)");
    }
    let path = p.path.clone();
    let control = match (&p.kind, &p.value) {
        (DeviceParamKind::Level { min_db, max_db }, DeviceParamValue::Level(db)) => {
            let lo = if min_db.is_finite() { *min_db } else { -96.0 };
            let v = if db.is_finite() {
                db.clamp(lo, *max_db)
            } else {
                lo
            };
            rsx! {
                input {
                    r#type: "range", class: "fader",
                    min: "{lo}", max: "{max_db}", step: "0.5", value: "{v}",
                    disabled: locked,
                    onchange: move |e| {
                        if let Ok(x) = e.value().parse::<f64>() {
                            set_param(handle.clone(), path.clone(), DeviceParamValue::Level(x));
                        }
                    },
                }
                span { class: "param-val", "{shown}" }
            }
        }
        (DeviceParamKind::Pan, DeviceParamValue::Pan(x)) => rsx! {
            input {
                r#type: "range", class: "pan",
                min: "-1", max: "1", step: "0.05", value: "{x}",
                disabled: locked,
                onchange: move |e| {
                    if let Ok(x) = e.value().parse::<f64>() {
                        set_param(handle.clone(), path.clone(), DeviceParamValue::Pan(x));
                    }
                },
            }
            span { class: "param-val", "{shown}" }
        },
        (DeviceParamKind::Toggle, DeviceParamValue::Toggle(b)) => {
            let b = *b;
            rsx! {
                button {
                    class: if b { "chip on toggle" } else { "chip toggle" },
                    disabled: locked,
                    onclick: move |_| set_param(handle.clone(), path.clone(), DeviceParamValue::Toggle(!b)),
                    "{leaf}"
                }
            }
        }
        (DeviceParamKind::Enum { options }, DeviceParamValue::Enum(i)) => {
            let cur = *i;
            rsx! {
                select {
                    disabled: locked,
                    onchange: move |e| {
                        if let Ok(x) = e.value().parse::<u32>() {
                            set_param(handle.clone(), path.clone(), DeviceParamValue::Enum(x));
                        }
                    },
                    for (n, o) in options.iter().enumerate() {
                        {
                            let n = u32::try_from(n).unwrap_or(u32::MAX);
                            rsx! { option { value: "{n}", selected: n == cur, "{o}" } }
                        }
                    }
                }
            }
        }
        (DeviceParamKind::Int { min, max }, DeviceParamValue::Int(n)) => rsx! {
            input {
                r#type: "number", class: "param-int",
                min: "{min}", max: "{max}", value: "{n}",
                disabled: locked,
                onchange: move |e| {
                    if let Ok(x) = e.value().parse::<i64>() {
                        set_param(handle.clone(), path.clone(), DeviceParamValue::Int(x));
                    }
                },
            }
        },
        (_, DeviceParamValue::Text(t)) => rsx! {
            input {
                r#type: "text", class: "param-text", value: "{t}",
                disabled: locked,
                onchange: move |e| set_param(handle.clone(), path.clone(), DeviceParamValue::Text(e.value())),
            }
        },
        _ => rsx! { span { class: "param-val", "{shown}" } },
    };
    rsx! {
        span {
            class: if p.disruptive { "param disruptive" } else { "param" },
            title: "{title}",
            if !matches!(p.kind, DeviceParamKind::Toggle) {
                span { class: "param-leaf", "{leaf}" }
            }
            {control}
        }
    }
}

/// One output group × one source group crosspoint grid.
#[component]
fn RouterGrid(view: DeviceView) -> Element {
    let handle = state::use_patchbay();
    if view.outputs.is_empty() {
        return rsx! {
            div { class: "panel-section dim", "No router on this device (params only)." }
        };
    }
    let out_id = {
        let g = OUT_GROUP.read().clone();
        if view.outputs.iter().any(|o| o.id == g) {
            g
        } else {
            view.outputs
                .first()
                .map(|o| o.id.clone())
                .unwrap_or_default()
        }
    };
    let cells: Vec<_> = view
        .routes
        .iter()
        .filter(|r| r.output.group == out_id)
        .cloned()
        .collect();
    // Default source group: the one this output group uses most.
    let src_id = {
        let g = SRC_GROUP.read().clone();
        if view.inputs.iter().any(|i| i.id == g) {
            g
        } else {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for c in &cells {
                if let Some(s) = &c.source {
                    let n = counts.entry(s.group.as_str()).or_default();
                    *n = n.saturating_add(1);
                }
            }
            counts
                .into_iter()
                .max_by_key(|(_, n)| *n)
                .map(|(g, _)| g.to_owned())
                .or_else(|| view.inputs.first().map(|i| i.id.clone()))
                .unwrap_or_default()
        }
    };
    let src_channels = view
        .inputs
        .iter()
        .find(|i| i.id == src_id)
        .map_or(0, |i| i.channels);
    let out_name = view
        .outputs
        .iter()
        .find(|o| o.id == out_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    rsx! {
        div { class: "panel-section router",
            h3 { "Router" }
            div { class: "router-pick",
                select {
                    onchange: move |e| *OUT_GROUP.write() = e.value(),
                    for o in view.outputs.iter() {
                        option { value: "{o.id}", selected: o.id == out_id, "{o.name} ({o.id})" }
                    }
                }
                " ← "
                select {
                    onchange: move |e| *SRC_GROUP.write() = e.value(),
                    for i in view.inputs.iter() {
                        option { value: "{i.id}", selected: i.id == src_id, "{i.name} ({i.id})" }
                    }
                }
            }
            div { class: "router-scroll",
                table { class: "dante-grid",
                    thead {
                        tr {
                            th { class: "corner corner-dev", "{out_name} ↓ / src →" }
                            th { class: "tx-ch", span { class: "tx-ch-label", "none" } }
                            for n in 1..=src_channels {
                                th { class: "tx-ch", span { class: "tx-ch-label", "{n}" } }
                            }
                        }
                    }
                    tbody {
                        for c in cells {
                            {
                                let out = c.output.clone();
                                let cur = c.source;
                                let n = u32::from(out.channel).saturating_add(1);
                                let label = source_label(cur.as_ref());
                                let none_on = cur.is_none();
                                let h0 = handle.clone();
                                let out0 = out.clone();
                                rsx! {
                                    tr { key: "{out.label()}",
                                        th { class: "rx-ch", title: "{out.label()} ← {label}", "{n} ← {label}" }
                                        td {
                                            class: if none_on { "cell sub ok" } else { "cell" },
                                            onclick: move |_| if !none_on { set_route(h0.clone(), out0.clone(), None) },
                                            if none_on { "·" }
                                        }
                                        for ch in 0..src_channels {
                                            {
                                                let src = DeviceChannel::new(src_id.clone(), ch);
                                                let on = cur.as_ref() == Some(&src);
                                                let h = handle.clone();
                                                let o = out.clone();
                                                rsx! {
                                                    td {
                                                        class: if on { "cell sub ok" } else { "cell" },
                                                        title: "{o.label()} ← {src.label()}",
                                                        onclick: move |_| if !on { set_route(h.clone(), o.clone(), Some(src.clone())) },
                                                        if on { "✓" }
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
}

const fn status_label(s: DeviceRestoreStatus) -> &'static str {
    match s {
        DeviceRestoreStatus::Planned => "planned",
        DeviceRestoreStatus::Applied => "applied",
        DeviceRestoreStatus::Failed => "FAILED",
        DeviceRestoreStatus::SkippedDisruptive => "skipped: disruptive",
        DeviceRestoreStatus::SkippedReadOnly => "skipped: read-only",
        DeviceRestoreStatus::SkippedMissing => "skipped: missing",
    }
}

#[component]
fn SnapshotPanel(view: DeviceView) -> Element {
    let handle = state::use_patchbay();
    let id = view.summary.id;
    let snaps: Vec<DeviceSnapshotInfo> = SNAPSHOTS.read().clone();
    let report = REPORT.read().clone();

    let save = {
        let handle = handle.clone();
        let id = id.clone();
        move |_| {
            let name = SNAP_NAME.peek().trim().to_owned();
            if name.is_empty() {
                *ERROR.write() = "snapshot name is empty".into();
                return;
            }
            let include: Vec<String> = SNAP_INCLUDE
                .peek()
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            let handle = handle.clone();
            let id = id.clone();
            spawn(async move {
                match handle
                    .0
                    .save_device_snapshot(id, name, include, Vec::new())
                    .await
                {
                    Ok(_) => refresh_list(&handle).await,
                    Err(e) => *ERROR.write() = format!("save snapshot: {e}"),
                }
            });
        }
    };

    rsx! {
        div { class: "panel-section",
            h3 { "Snapshots" }
            div { class: "snap-save",
                input { class: "search", placeholder: "name", value: "{SNAP_NAME}",
                    oninput: move |e| *SNAP_NAME.write() = e.value() }
                input { class: "search", placeholder: "include prefixes (empty = all)", value: "{SNAP_INCLUDE}",
                    oninput: move |e| *SNAP_INCLUDE.write() = e.value() }
                button { class: "chip", onclick: save, "save" }
            }
            for s in snaps.into_iter().filter(|s| s.device == id) {
                {
                    let h1 = handle.clone();
                    let h2 = handle.clone();
                    let n1 = s.name.clone();
                    let n2 = s.name.clone();
                    let scope = if s.include.is_empty() { "all".to_owned() } else { s.include.join(", ") };
                    rsx! {
                        div { class: "snap-row", key: "{s.name}",
                            span { class: "snap-name", "{s.name}" }
                            span { class: "dim-note", " {s.params} param(s), {s.routes} route(s) · {scope}" }
                            button { class: "chip",
                                onclick: move |_| {
                                    let h = h1.clone();
                                    let n = n1.clone();
                                    let allow = *ALLOW_DISRUPTIVE.peek();
                                    spawn(async move {
                                        match h.0.diff_device_snapshot(n, Vec::new(), allow).await {
                                            Ok(r) => *REPORT.write() = Some(r),
                                            Err(e) => *ERROR.write() = format!("diff: {e}"),
                                        }
                                    });
                                },
                                "diff"
                            }
                            button { class: "chip danger",
                                onclick: move |_| {
                                    let h = h2.clone();
                                    let n = n2.clone();
                                    spawn(async move {
                                        match h.0.delete_device_snapshot(n).await {
                                            Ok(()) => refresh_list(&h).await,
                                            Err(e) => *ERROR.write() = format!("delete: {e}"),
                                        }
                                    });
                                },
                                "delete"
                            }
                        }
                    }
                }
            }
            if let Some(r) = report {
                {
                    let planned = r.count(DeviceRestoreStatus::Planned);
                    let name = r.snapshot.clone();
                    let h = handle;
                    rsx! {
                        div { class: "snap-report",
                            div {
                                strong { if r.dry_run { "Diff: " } else { "Restored: " } "{r.snapshot}" }
                                span { class: "dim-note", " {r.items.len()} differing · {r.unchanged} unchanged" }
                            }
                            if r.dry_run && planned > 0 {
                                button { class: "chip on",
                                    onclick: move |_| {
                                        let h = h.clone();
                                        let n = name.clone();
                                        let allow = *ALLOW_DISRUPTIVE.peek();
                                        spawn(async move {
                                            match h.0.restore_device_snapshot(n, Vec::new(), false, allow).await {
                                                Ok(r) => {
                                                    *REPORT.write() = Some(r);
                                                    load_selected(&h).await;
                                                }
                                                Err(e) => *ERROR.write() = format!("restore: {e}"),
                                            }
                                        });
                                    },
                                    "restore {planned} change(s)"
                                }
                            }
                            table { class: "snap-table",
                                for i in r.items.iter() {
                                    tr { key: "{i.path}",
                                        td { "{i.path}" }
                                        td { "{i.current}" }
                                        td { "→ {i.target}" }
                                        td { class: "dim-note", "{status_label(i.status)}" }
                                        td { class: "dante-error", "{i.error}" }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_and_rows_follow_paths() {
        assert_eq!(section_of("mixer/1/strip/16/level"), "mixer/1");
        assert_eq!(section_of("monitor/dim"), "monitor");
        assert_eq!(section_of("trim/line_in/3"), "trim");
        assert_eq!(section_of("in/3/send/aux/2/level"), "in/3");
        assert_eq!(row_of("mixer/1/strip/16/level"), "mixer/1/strip/16");
        assert_eq!(leaf_of("mixer/1/strip/16/level"), "level");
    }
}
