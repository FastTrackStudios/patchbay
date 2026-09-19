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

use crate::state::{self, PatchbayHandle};
use crate::ui::{Status, StatusDot};

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

fn bump_stale() {
    let mut s = STALE.write();
    *s = s.wrapping_add(1);
}

fn device_key(d: &DeviceSummary) -> String {
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

#[component]
pub fn DevicesView() -> Element {
    let handle = state::use_patchbay();

    // List on open, then keep it (and the selected device) fresh: a
    // cheap poll for link state, a full re-read when events say so.
    use_future({
        let handle = handle.clone();
        move || {
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
                    let unloaded = VIEW.peek().is_none() && SELECTED.peek().is_some();
                    if tick.is_multiple_of(5) || stale != seen_stale {
                        refresh_list(&handle).await;
                    }
                    if stale != seen_stale || (unloaded && tick.is_multiple_of(5)) {
                        seen_stale = stale;
                        load_selected(&handle).await;
                    }
                }
            }
        }
    });

    let devices = DEVICES.read().clone();
    let selected = SELECTED.read().clone();
    let error = ERROR.read().clone();
    let loading = *LOADING.read();
    let view = VIEW.read().clone();

    let reload = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            spawn(async move {
                refresh_list(&handle).await;
                load_selected(&handle).await;
            });
        }
    };

    rsx! {
        div { class: "devices-view",
            div { class: "dante-header",
                for d in devices.iter() {
                    {
                        let key = device_key(d);
                        let on = selected.as_deref() == Some(key.as_str());
                        let status = match d.state {
                            DeviceLinkState::Online => Status::Ok,
                            DeviceLinkState::Connecting | DeviceLinkState::Searching => Status::Busy,
                            DeviceLinkState::Offline => Status::Bad,
                            DeviceLinkState::Disabled | DeviceLinkState::NotFound => Status::Missing,
                        };
                        let title = format!("{} — {}{}{}", d.name, state_label(d.state), if d.transport.is_empty() { String::new() } else { format!(" — {}", d.transport) }, if d.error.is_empty() { String::new() } else { format!(" — {}", d.error) });
                        let label = if d.model.is_empty() { d.name.clone() } else { format!("{} ({})", d.model, d.name) };
                        let handle = handle.clone();
                        rsx! {
                            button {
                                class: if on { "chip on" } else { "chip" },
                                title: "{title}",
                                onclick: move |_| {
                                    *SELECTED.write() = Some(key.clone());
                                    *VIEW.write() = None;
                                    OUT_GROUP.write().clear();
                                    SRC_GROUP.write().clear();
                                    *REPORT.write() = None;
                                    let handle = handle.clone();
                                    spawn(async move { load_selected(&handle).await });
                                },
                                StatusDot { status, title: "{title}" }
                                " {label}"
                            }
                        }
                    }
                }
                button { class: "chip", onclick: reload,
                    if loading { "reading…" } else { "re-read" }
                }
                label { class: "label",
                    input {
                        r#type: "checkbox",
                        checked: *ALLOW_DISRUPTIVE.read(),
                        onchange: move |e| *ALLOW_DISRUPTIVE.write() = e.checked(),
                    }
                    " allow disruptive writes (clock / scene recall)"
                }
                if !error.is_empty() {
                    span { class: "dante-error", "{error}" }
                }
            }
            if devices.is_empty() {
                div { class: "panel-section dim", style: "padding:24px;",
                    "No devices configured. Add a `devices` section to the patchbay config, "
                    "e.g. devices ({{name galaxy32, kind antelope-galaxy32}})."
                }
            } else if let Some(v) = view {
                div { class: "devices-body",
                    ParamPanel { view: v.clone() }
                    div { class: "device-side",
                        RouterGrid { view: v.clone() }
                        SnapshotPanel { view: v }
                    }
                }
            } else {
                div { class: "panel-section dim", style: "padding:24px;",
                    if loading { "reading device…" } else { "device not loaded (offline?) — pick a device or re-read" }
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
