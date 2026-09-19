//! Mixes view — Loopback / OBS-style host audio mixes (macOS).
//!
//! Left: the saved mixes with their running / stopped / disabled state.
//! Right: the selected mix as a console — one vertical channel strip per
//! source (peak meter, dB fader, mute, channel map), an "add source"
//! picker (apps / input devices / system audio), then the outputs as
//! strips of their own (the first one clocks the mix).
//!
//! Level and mute go through the live RPCs (`set_mix_source` /
//! `set_mix_output`, coalesced while a fader drags); anything structural
//! (add / remove / map / enable) re-saves the whole config with
//! `save_mix`, exactly like `patchbay mix …` does. Meters poll
//! `mix_meters` at ~10 Hz only while this view is mounted.
//!
//! `patchbay-proto` only, so it runs the same in the desktop shell and
//! the wasm remote.

use std::collections::HashMap;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use patchbay_proto::{
    HostDevice, HostTargets, MixConfig, MixMeters, MixOutputConfig, MixSourceConfig,
    MixSourceStatus, MixView, default_map, source_kind,
};

use crate::state::{self, PatchbayHandle};
use crate::ui::level::{SILENT_DB, fall, fmt_db, peak_db};
use crate::ui::{Fader, Meter, MeterScale};

// ─── Poll cadences ──────────────────────────────────────────────────────

const METER_POLL_MS: u32 = 100;
/// Mix list / host targets refresh cadence, in meter polls.
const LIST_EVERY: u32 = 20;
const TARGETS_EVERY: u32 = 50;

// ─── State ──────────────────────────────────────────────────────────────

static MIXES: GlobalSignal<Vec<MixView>> = Signal::global(Vec::new);
static TARGETS: GlobalSignal<HostTargets> = Signal::global(HostTargets::default);
/// `host_targets` answered at least once (until then "unsupported" is
/// unknown, not false).
static TARGETS_LOADED: GlobalSignal<bool> = Signal::global(|| false);
static SELECTED: GlobalSignal<Option<String>> = Signal::global(|| None);
static ERROR: GlobalSignal<String> = Signal::global(String::new);
static NEW_NAME: GlobalSignal<String> = Signal::global(String::new);
/// Patchbay.driver status + its virtual devices.
static VDEVS: GlobalSignal<patchbay_proto::VirtualDevicesStatus> =
    Signal::global(patchbay_proto::VirtualDevicesStatus::default);
static VNEW_NAME: GlobalSignal<String> = Signal::global(String::new);
static VNEW_CHANNELS: GlobalSignal<u32> = Signal::global(|| 2);
/// `(uid, draft name)` of the device being renamed.
static VRENAME: GlobalSignal<Option<(String, String)>> = Signal::global(|| None);
/// Displayed meter levels (dBFS, with fall-off) per mix name.
static METERS: GlobalSignal<HashMap<String, MeterLevels>> = Signal::global(HashMap::new);
/// Open "add source" picker tab.
static PICKER: GlobalSignal<Option<PickerTab>> = Signal::global(|| None);
/// Apps tab: "only what it plays to" device uid (empty = stereo mixdown).
static PICKER_DEVICE: GlobalSignal<String> = Signal::global(String::new);
/// Apps tab: free bundle id for an app that isn't running yet.
static PICKER_BUNDLE: GlobalSignal<String> = Signal::global(String::new);
/// Mix whose delete button is armed (second click deletes).
static CONFIRM_DELETE: GlobalSignal<Option<String>> = Signal::global(|| None);
/// Live level writes in flight, per strip — see [`set_level`].
static LEVEL_TX: GlobalSignal<HashMap<LevelKey, Option<(f64, bool)>>> =
    Signal::global(HashMap::new);

/// `(mix name, is output, index)`.
type LevelKey = (String, bool, usize);

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerTab {
    Apps,
    Inputs,
    System,
}

#[derive(Clone, Default, PartialEq)]
struct MeterLevels {
    sources: Vec<f64>,
    outputs: Vec<f64>,
}

// ─── Pure helpers ───────────────────────────────────────────────────────

/// Stereo quick picks over a device's channels: `(label, map)`, where a
/// source map takes device channels N,N+1 into mix L/R and an output map
/// sends mix L/R to device channels N,N+1. Mono devices get one pick.
fn pair_picks(channels: u32, output: bool, word: &str) -> Vec<(String, String)> {
    let mut picks = Vec::new();
    if channels == 1 {
        let map = if output { "0:0" } else { "0:0,0:1" };
        picks.push((format!("{word} 1 (mono)"), map.to_owned()));
        return picks;
    }
    let mut a = 0_u32;
    while a.saturating_add(1) < channels {
        let b = a.saturating_add(1);
        let map = if output {
            format!("0:{a},1:{b}")
        } else {
            format!("{a}:0,{b}:1")
        };
        let label = format!("{word} {}–{}", a.saturating_add(1), b.saturating_add(1));
        picks.push((label, map));
        a = a.saturating_add(2);
    }
    picks
}

/// Mono picks for an input device (a mic on channel N → both sides).
fn mono_picks(channels: u32) -> Vec<(String, String)> {
    (0..channels)
        .map(|c| {
            (
                format!("in {} → L+R", c.saturating_add(1)),
                format!("{c}:0,{c}:1"),
            )
        })
        .collect()
}

fn find_device<'a>(t: &'a HostTargets, uid: &str) -> Option<&'a HostDevice> {
    t.devices.iter().find(|d| d.uid == uid)
}

fn device_name(t: &HostTargets, uid: &str) -> String {
    find_device(t, uid).map_or_else(|| uid.to_owned(), |d| d.name.clone())
}

fn app_name(t: &HostTargets, bundle_id: &str) -> String {
    t.apps
        .iter()
        .find(|a| a.bundle_id == bundle_id)
        .map(|a| a.name.clone())
        .or_else(|| bundle_id.rsplit('.').next().map(str::to_owned))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| bundle_id.to_owned())
}

/// Strip label: `REAPER`, `REAPER → Galaxy32`, `Galaxy32`, `System audio`.
fn source_label(t: &HostTargets, s: &MixSourceConfig) -> String {
    match s.kind.as_str() {
        source_kind::APP if s.device.is_empty() => app_name(t, &s.target),
        source_kind::APP => format!("{} → {}", app_name(t, &s.target), device_name(t, &s.device)),
        source_kind::SYSTEM => "System audio".to_owned(),
        _ => device_name(t, &s.target),
    }
}

fn kind_label(s: &MixSourceConfig) -> &'static str {
    match s.kind.as_str() {
        source_kind::APP if s.device.is_empty() => "app",
        source_kind::APP => "app on device",
        source_kind::INPUT => "input",
        source_kind::SYSTEM => "system",
        _ => "source",
    }
}

/// `(badge class, text)` for a mix.
fn mix_state(v: &MixView) -> (&'static str, &'static str) {
    if !v.config.is_enabled() {
        ("mix-badge off", "disabled")
    } else if v.running {
        ("mix-badge live", "running")
    } else {
        ("mix-badge bad", "stopped")
    }
}

/// Devices that can take a mix's output, the virtual ones first.
fn output_devices(t: &HostTargets) -> Vec<HostDevice> {
    let mut devs: Vec<HostDevice> = t
        .devices
        .iter()
        .filter(|d| d.output_channels > 0)
        .cloned()
        .collect();
    devs.sort_by_key(|d| (virtual_rank(&d.name), d.name.to_lowercase()));
    devs
}

/// 0 = Broadcast, 1 = Patchbay, 2 = everything else.
fn virtual_rank(name: &str) -> u8 {
    let n = name.to_lowercase();
    if n.contains("broadcast") {
        0
    } else if n.contains("patchbay") {
        1
    } else {
        2
    }
}

fn output_hint(name: &str) -> &'static str {
    match virtual_rank(name) {
        0 => " — virtual mic for Discord / FaceTime",
        1 => " — Patchbay virtual device",
        _ => "",
    }
}

// ─── RPC plumbing ───────────────────────────────────────────────────────

async fn refresh_mixes(handle: &PatchbayHandle) {
    match handle.0.list_mixes().await {
        Ok(list) => {
            // A fader mid-drag owns its value; the next poll catches up.
            if !LEVEL_TX.peek().is_empty() {
                return;
            }
            let selected = SELECTED.peek().clone();
            let keep = selected
                .as_ref()
                .is_some_and(|s| list.iter().any(|m| &m.config.name == s));
            if !keep {
                *SELECTED.write() = list.first().map(|m| m.config.name.clone());
            }
            if *MIXES.peek() != list {
                *MIXES.write() = list;
            }
        }
        Err(e) => *ERROR.write() = format!("list mixes: {e}"),
    }
}

async fn refresh_virtual(handle: &PatchbayHandle) {
    match handle.0.virtual_devices().await {
        Ok(v) => {
            if *VDEVS.peek() != v {
                *VDEVS.write() = v;
            }
        }
        Err(e) => *ERROR.write() = format!("virtual devices: {e}"),
    }
}

async fn refresh_targets(handle: &PatchbayHandle) {
    match handle.0.host_targets().await {
        Ok(t) => {
            if *TARGETS.peek() != t {
                *TARGETS.write() = t;
            }
            if !*TARGETS_LOADED.peek() {
                *TARGETS_LOADED.write() = true;
            }
        }
        Err(e) => *ERROR.write() = format!("host targets: {e}"),
    }
}

/// Fold one `mix_meters` answer into the displayed levels.
fn fold_meters(
    prev: &HashMap<String, MeterLevels>,
    meters: &[MixMeters],
) -> HashMap<String, MeterLevels> {
    let lane = |old: Option<&Vec<f64>>, new: &[Vec<f32>]| -> Vec<f64> {
        new.iter()
            .enumerate()
            .map(|(i, peaks)| fall(old.and_then(|o| o.get(i)).copied(), peak_db(peaks)))
            .collect()
    };
    meters
        .iter()
        .map(|m| {
            let old = prev.get(&m.name);
            let levels = MeterLevels {
                sources: lane(old.map(|o| &o.sources), &m.sources),
                outputs: lane(old.map(|o| &o.outputs), &m.outputs),
            };
            (m.name.clone(), levels)
        })
        .collect()
}

async fn poll_meters(handle: &PatchbayHandle) {
    match handle.0.mix_meters().await {
        Ok(m) => {
            let next = fold_meters(&METERS.peek(), &m);
            if *METERS.peek() != next {
                *METERS.write() = next;
            }
        }
        Err(e) => tracing::debug!("mix meters: {e:?}"),
    }
}

/// Save a whole config (structural edits) and fold the answer in.
fn save(handle: PatchbayHandle, cfg: MixConfig) {
    spawn_forever(async move {
        let name = cfg.name.clone();
        match handle.0.save_mix(cfg).await {
            Ok(v) => {
                let mut mixes = MIXES.write();
                match mixes.iter_mut().find(|m| m.config.name == v.config.name) {
                    Some(slot) => *slot = v,
                    None => mixes.push(v),
                }
                ERROR.write().clear();
            }
            Err(e) => *ERROR.write() = format!("save '{name}': {e}"),
        }
    });
}

/// Edit the named mix's config and save it.
fn edit(handle: PatchbayHandle, name: &str, f: impl FnOnce(&mut MixConfig)) {
    let Some(mut cfg) = MIXES
        .peek()
        .iter()
        .find(|m| m.config.name == name)
        .map(|m| m.config.clone())
    else {
        return;
    };
    f(&mut cfg);
    save(handle, cfg);
}

/// Live gain / mute for one strip.
///
/// Shown immediately; the RPC is coalesced per strip — while one write
/// is in flight, further values only replace the pending one, so a fast
/// drag sends at the round-trip rate and always lands on the last value.
fn set_level(
    handle: PatchbayHandle,
    mix: String,
    output: bool,
    index: usize,
    gain_db: f64,
    muted: bool,
) {
    let Ok(wire_index) = u32::try_from(index) else {
        return;
    };
    {
        let mut mixes = MIXES.write();
        if let Some(m) = mixes.iter_mut().find(|m| m.config.name == mix) {
            if output {
                if let Some(o) = m.config.outputs.get_mut(index) {
                    o.gain_db = gain_db;
                    o.muted = muted;
                }
            } else if let Some(s) = m.config.sources.get_mut(index) {
                s.gain_db = gain_db;
                s.muted = muted;
            }
        }
    }
    let key: LevelKey = (mix.clone(), output, index);
    {
        let mut tx = LEVEL_TX.write();
        if let Some(pending) = tx.get_mut(&key) {
            *pending = Some((gain_db, muted));
            return;
        }
        tx.insert(key.clone(), None);
    }
    spawn_forever(async move {
        let mut next = Some((gain_db, muted));
        while let Some((g, m)) = next {
            let res = if output {
                handle.0.set_mix_output(mix.clone(), wire_index, g, m).await
            } else {
                handle.0.set_mix_source(mix.clone(), wire_index, g, m).await
            };
            if let Err(e) = res {
                let what = if output { "output" } else { "source" };
                *ERROR.write() = format!("{mix}: {what} {}: {e}", index.saturating_add(1));
            }
            let mut tx = LEVEL_TX.write();
            next = tx.get_mut(&key).and_then(Option::take);
            if next.is_none() {
                tx.remove(&key);
            }
        }
    });
}

// ─── Components ─────────────────────────────────────────────────────────

#[component]
pub fn MixesView() -> Element {
    let handle = state::use_patchbay();

    // Targets + list on open, then meters at ~10 Hz (only while this view
    // is mounted — the task dies with it), list and targets slower.
    use_future({
        let handle = handle.clone();
        move || {
            let handle = handle.clone();
            async move {
                futures_util::join!(
                    refresh_targets(&handle),
                    refresh_mixes(&handle),
                    refresh_virtual(&handle)
                );
                let mut tick = 0_u32;
                loop {
                    state::sleep_ms(METER_POLL_MS).await;
                    tick = tick.wrapping_add(1);
                    if !TARGETS.peek().supported {
                        if tick.is_multiple_of(TARGETS_EVERY) {
                            refresh_targets(&handle).await;
                        }
                        continue;
                    }
                    poll_meters(&handle).await;
                    if tick.is_multiple_of(LIST_EVERY) {
                        refresh_mixes(&handle).await;
                    }
                    if tick.is_multiple_of(TARGETS_EVERY) {
                        refresh_targets(&handle).await;
                        refresh_virtual(&handle).await;
                    }
                }
            }
        }
    });

    let loaded = *TARGETS_LOADED.read();
    let supported = TARGETS.read().supported;
    let error = ERROR.read().clone();
    let mixes = MIXES.read().clone();
    let selected = SELECTED.read().clone();
    let current = selected
        .as_ref()
        .and_then(|s| mixes.iter().find(|m| &m.config.name == s))
        .cloned();

    if loaded && !supported {
        return rsx! {
            div { class: "mixes-view",
                div { class: "mix-empty",
                    h3 { "Mixes need macOS" }
                    p {
                        "A mix taps applications and audio devices through Core Audio "
                        "(Loopback / OBS style) — the Patchbay server this window is "
                        "connected to isn't running on macOS, so mixes can't run here."
                    }
                    p { class: "dim-note",
                        "Saved mixes stay in the config and start when the server runs on a Mac."
                    }
                }
            }
        };
    }

    let create = {
        move || {
            let name = NEW_NAME.peek().trim().to_owned();
            if name.is_empty() {
                *ERROR.write() = "mix name is empty".into();
                return;
            }
            if MIXES.peek().iter().any(|m| m.config.name == name) {
                *ERROR.write() = format!("a mix named '{name}' already exists");
                return;
            }
            NEW_NAME.write().clear();
            *SELECTED.write() = Some(name.clone());
            save(
                handle.clone(),
                MixConfig {
                    name,
                    channels: 2,
                    sources: Vec::new(),
                    outputs: Vec::new(),
                    enabled: None,
                },
            );
        }
    };
    let create_key = create.clone();

    rsx! {
        div { class: "mixes-view",
            div { class: "mix-list",
                h3 { "Mixes" }
                for m in mixes.iter() {
                    {
                        let name = m.config.name.clone();
                        let on = selected.as_deref() == Some(name.as_str());
                        let (badge, word) = mix_state(m);
                        let err = if m.config.is_enabled() && !m.running { m.error.clone() } else { String::new() };
                        let n_src = m.config.sources.len();
                        let n_out = m.config.outputs.len();
                        rsx! {
                            div {
                                key: "{name}",
                                class: if on { "mix-row on" } else { "mix-row" },
                                onclick: move |_| {
                                    *SELECTED.write() = Some(name.clone());
                                    *PICKER.write() = None;
                                    *CONFIRM_DELETE.write() = None;
                                },
                                div { class: "mix-row-top",
                                    span { class: "mix-row-name", "{m.config.name}" }
                                    span { class: "{badge}", "{word}" }
                                }
                                div { class: "dim-note", "{n_src} source(s) → {n_out} output(s)" }
                                if !err.is_empty() {
                                    div { class: "mix-error", "{err}" }
                                }
                            }
                        }
                    }
                }
                if mixes.is_empty() && loaded {
                    div { class: "dim-note", "No mixes yet." }
                }
                div { class: "mix-new",
                    input {
                        class: "search",
                        placeholder: "new mix name (e.g. Discord)",
                        value: "{NEW_NAME}",
                        oninput: move |e| *NEW_NAME.write() = e.value(),
                        onkeydown: move |e: Event<KeyboardData>| {
                            if e.key() == Key::Enter {
                                create_key();
                            }
                        },
                    }
                    button { class: "chip", onclick: move |_| create(), "New mix" }
                }
                VirtualDevicesPanel {}
            }
            div { class: "mix-main",
                if !error.is_empty() {
                    div { class: "mix-error-bar",
                        span { "{error}" }
                        button { class: "chip", onclick: move |_| ERROR.write().clear(), "dismiss" }
                    }
                }
                if let Some(v) = current {
                    MixEditor { view: v }
                } else if !loaded {
                    div { class: "mix-empty dim-note", "loading…" }
                } else {
                    div { class: "mix-empty",
                        h3 { "No mix selected" }
                        p { class: "dim-note",
                            "A mix sums sources — an app, an input device's channels, all system audio — "
                            "into outputs, e.g. the Broadcast virtual device Discord or FaceTime pick as their microphone. "
                            "Create one on the left."
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn MixEditor(view: MixView) -> Element {
    let handle = state::use_patchbay();
    let targets = TARGETS.read().clone();
    let cfg = view.config.clone();
    let name = cfg.name.clone();
    let enabled = cfg.is_enabled();
    let (badge, word) = mix_state(&view);
    let armed = CONFIRM_DELETE.read().as_deref() == Some(name.as_str());
    let picker = *PICKER.read();

    let toggle = {
        let handle = handle.clone();
        let name = name.clone();
        move |_| {
            edit(handle.clone(), &name, |c| {
                c.enabled = if enabled { Some(false) } else { None };
            });
        }
    };
    let delete = {
        let name = name.clone();
        move |_| {
            if CONFIRM_DELETE.peek().as_deref() != Some(name.as_str()) {
                *CONFIRM_DELETE.write() = Some(name.clone());
                return;
            }
            *CONFIRM_DELETE.write() = None;
            let handle = handle.clone();
            let name = name.clone();
            spawn_forever(async move {
                match handle.0.delete_mix(name.clone()).await {
                    Ok(()) => {
                        MIXES.write().retain(|m| m.config.name != name);
                        METERS.write().remove(&name);
                        let first = MIXES.peek().first().map(|m| m.config.name.clone());
                        *SELECTED.write() = first;
                        ERROR.write().clear();
                    }
                    Err(e) => *ERROR.write() = format!("delete '{name}': {e}"),
                }
            });
        }
    };

    rsx! {
        div { class: "mix-editor",
            div { class: "mix-head",
                span { class: "mix-title", "{name}" }
                span { class: "{badge}", "{word}" }
                span { class: "dim-note", "{cfg.channels} ch" }
                if enabled && !view.running && !view.error.is_empty() {
                    span { class: "mix-error", "{view.error}" }
                }
                div { class: "mix-head-actions",
                    button {
                        class: if enabled { "chip on" } else { "chip" },
                        title: if enabled { "running when possible — click to stop and keep it saved" } else { "stopped — click to start" },
                        onclick: toggle,
                        if enabled { "enabled" } else { "disabled" }
                    }
                    button {
                        class: if armed { "chip danger armed" } else { "chip danger" },
                        onclick: delete,
                        if armed { "click again to delete" } else { "delete" }
                    }
                }
            }

            div { class: "mix-section-h", "Sources" }
            div { class: "mix-strips",
                for (i, s) in cfg.sources.iter().enumerate() {
                    SourceStrip {
                        key: "{i}-{s.kind}-{s.target}-{s.device}",
                        mix: name.clone(),
                        index: i,
                        source: s.clone(),
                        status: view.sources.get(i).cloned(),
                        running: view.running,
                        targets: targets.clone(),
                    }
                }
                button {
                    class: if picker.is_some() { "mix-add on" } else { "mix-add" },
                    onclick: move |_| {
                        let open = PICKER.peek().is_some();
                        *PICKER.write() = if open { None } else { Some(PickerTab::Apps) };
                    },
                    span { class: "mix-add-plus", "+" }
                    "Add source"
                }
            }
            if let Some(tab) = picker {
                SourcePicker { mix: name.clone(), tab, targets: targets.clone() }
            }

            div { class: "mix-section-h", "Outputs" }
            div { class: "mix-strips",
                for (i, o) in cfg.outputs.iter().enumerate() {
                    OutputStrip {
                        key: "{i}-{o.device}",
                        mix: name.clone(),
                        index: i,
                        output: o.clone(),
                        clock: view.outputs.get(i).map_or(i == 0, |s| s.clock),
                        targets: targets.clone(),
                    }
                }
                AddOutput { mix: name, targets, taken: cfg.outputs.iter().map(|o| o.device.clone()).collect::<Vec<_>>() }
            }
        }
    }
}

/// One strip's meter: reads [`METERS`] itself, so the poll rate only
/// re-renders meters and never the strip around them.
#[component]
fn StripMeter(mix: String, output: bool, index: usize) -> Element {
    let db = METERS
        .read()
        .get(&mix)
        .and_then(|m| {
            if output {
                m.outputs.get(index)
            } else {
                m.sources.get(index)
            }
        })
        .copied()
        .unwrap_or(SILENT_DB);
    rsx! { Meter { db } }
}

#[component]
fn SourceStrip(
    mix: String,
    index: usize,
    source: MixSourceConfig,
    status: Option<MixSourceStatus>,
    running: bool,
    targets: HostTargets,
) -> Element {
    let handle = state::use_patchbay();
    let s = source;
    let label = source_label(&targets, &s);
    let kind = kind_label(&s);
    let (live_class, live_text, reason) = match &status {
        Some(st) if st.active => ("live-badge on", "live", String::new()),
        Some(st) => ("live-badge wait", "waiting", st.reason.clone()),
        None if running => ("live-badge wait", "waiting", String::new()),
        None => ("live-badge", "stopped", String::new()),
    };
    let title = match &status {
        Some(st) if !st.label.is_empty() => {
            format!("{} — {} ({} ch)", st.label, s.target, st.channels)
        }
        _ => format!(
            "{kind}: {}",
            if s.target.is_empty() {
                "all system audio"
            } else {
                &s.target
            }
        ),
    };
    // Quick picks: which device's channels the map picks from.
    let picks = match s.kind.as_str() {
        source_kind::APP if !s.device.is_empty() => find_device(&targets, &s.device)
            .map(|d| pair_picks(d.output_channels, false, "outs"))
            .unwrap_or_default(),
        source_kind::INPUT => find_device(&targets, &s.target)
            .map(|d| {
                let mut p = pair_picks(d.input_channels, false, "ins");
                if d.input_channels > 1 {
                    p.extend(mono_picks(d.input_channels));
                }
                p
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let gain = s.gain_db;
    let muted = s.muted;

    let on_level = {
        let handle = handle.clone();
        let mix = mix.clone();
        move |(g, m): (f64, bool)| set_level(handle.clone(), mix.clone(), false, index, g, m)
    };
    let mute = {
        let handle = handle.clone();
        let mix = mix.clone();
        move |_| set_level(handle.clone(), mix.clone(), false, index, gain, !muted)
    };
    let set_map = {
        let handle = handle.clone();
        let mix = mix.clone();
        move |map: String| {
            let map = if map.trim().is_empty() {
                default_map()
            } else {
                map.trim().to_owned()
            };
            edit(handle.clone(), &mix, |c| {
                if let Some(x) = c.sources.get_mut(index) {
                    x.map = map;
                }
            });
        }
    };
    let set_map_pick = set_map.clone();
    let remove = {
        let mix = mix.clone();
        move |_| {
            edit(handle.clone(), &mix, |c| {
                if index < c.sources.len() {
                    c.sources.remove(index);
                }
            });
        }
    };

    rsx! {
        div { class: if muted { "mix-strip muted" } else { "mix-strip" },
            div { class: "strip-head", title: "{title}",
                div { class: "strip-label", "{label}" }
                div { class: "strip-kind", "{kind}" }
                span { class: "{live_class}", title: "{reason}", "{live_text}" }
                if !reason.is_empty() {
                    div { class: "strip-reason", title: "{reason}", "{reason}" }
                }
            }
            div { class: "strip-body",
                MeterScale {}
                StripMeter { mix, output: false, index }
                Fader { gain_db: gain, muted, on_level }
            }
            div { class: "strip-db", "{fmt_db(gain)}" }
            button { class: if muted { "chip mute on" } else { "chip mute" }, onclick: mute,
                if muted { "muted" } else { "mute" }
            }
            input {
                class: "strip-map",
                title: "channel map: source channel → mix channel, 0-based (0:0,1:1 = stereo straight through)",
                value: "{s.map}",
                onchange: move |e| set_map(e.value()),
            }
            if !picks.is_empty() {
                select {
                    class: "strip-pick",
                    onchange: move |e| {
                        let v = e.value();
                        if !v.is_empty() {
                            set_map_pick(v);
                        }
                    },
                    option { value: "", selected: true, "pick channels…" }
                    for (l, m) in picks {
                        option { value: "{m}", "{l}" }
                    }
                }
            }
            button { class: "chip danger strip-remove", title: "remove this source", onclick: remove, "remove" }
        }
    }
}

#[component]
fn OutputStrip(
    mix: String,
    index: usize,
    output: MixOutputConfig,
    clock: bool,
    targets: HostTargets,
) -> Element {
    let handle = state::use_patchbay();
    let o = output;
    let dev = find_device(&targets, &o.device).cloned();
    let label = dev
        .as_ref()
        .map_or_else(|| o.device.clone(), |d| d.name.clone());
    let hint = output_hint(&label).trim_start_matches(" — ").to_owned();
    let picks = dev
        .as_ref()
        .filter(|d| d.output_channels > 2 || d.output_channels == 1)
        .map(|d| pair_picks(d.output_channels, true, "outs"))
        .unwrap_or_default();
    let gain = o.gain_db;
    let muted = o.muted;

    let on_level = {
        let handle = handle.clone();
        let mix = mix.clone();
        move |(g, m): (f64, bool)| set_level(handle.clone(), mix.clone(), true, index, g, m)
    };
    let mute = {
        let handle = handle.clone();
        let mix = mix.clone();
        move |_| set_level(handle.clone(), mix.clone(), true, index, gain, !muted)
    };
    let set_map = {
        let handle = handle.clone();
        let mix = mix.clone();
        move |map: String| {
            let map = if map.trim().is_empty() {
                default_map()
            } else {
                map.trim().to_owned()
            };
            edit(handle.clone(), &mix, |c| {
                if let Some(x) = c.outputs.get_mut(index) {
                    x.map = map;
                }
            });
        }
    };
    let set_map_pick = set_map.clone();
    let remove = {
        let mix = mix.clone();
        move |_| {
            edit(handle.clone(), &mix, |c| {
                if index < c.outputs.len() {
                    c.outputs.remove(index);
                }
            });
        }
    };

    rsx! {
        div { class: if muted { "mix-strip output muted" } else { "mix-strip output" },
            div { class: "strip-head", title: "{o.device}",
                div { class: "strip-label", "{label}" }
                div { class: "strip-kind",
                    if hint.is_empty() { "output" } else { "{hint}" }
                }
                if clock {
                    span { class: "live-badge clock", title: "this output's device clocks the mix", "clock" }
                }
                if dev.is_none() {
                    div { class: "strip-reason", "device not present" }
                }
            }
            div { class: "strip-body",
                MeterScale {}
                StripMeter { mix, output: true, index }
                Fader { gain_db: gain, muted, on_level }
            }
            div { class: "strip-db", "{fmt_db(gain)}" }
            button { class: if muted { "chip mute on" } else { "chip mute" }, onclick: mute,
                if muted { "muted" } else { "mute" }
            }
            input {
                class: "strip-map",
                title: "channel map: mix channel → output channel, 0-based (0:32,1:33 = outputs 33–34)",
                value: "{o.map}",
                onchange: move |e| set_map(e.value()),
            }
            if !picks.is_empty() {
                select {
                    class: "strip-pick",
                    onchange: move |e| {
                        let v = e.value();
                        if !v.is_empty() {
                            set_map_pick(v);
                        }
                    },
                    option { value: "", selected: true, "pick channels…" }
                    for (l, m) in picks {
                        option { value: "{m}", "{l}" }
                    }
                }
            }
            button { class: "chip danger strip-remove", title: "remove this output", onclick: remove, "remove" }
        }
    }
}

/// "+ Add output": a device dropdown (virtual devices first) plus a
/// one-click Broadcast shortcut.
#[component]
fn AddOutput(mix: String, targets: HostTargets, taken: Vec<String>) -> Element {
    let handle = state::use_patchbay();
    let devs: Vec<HostDevice> = output_devices(&targets)
        .into_iter()
        .filter(|d| !taken.contains(&d.uid))
        .collect();
    let broadcast = devs.iter().find(|d| virtual_rank(&d.name) == 0).cloned();
    let add = {
        move |uid: String| {
            edit(handle.clone(), &mix, |c| {
                c.outputs.push(MixOutputConfig {
                    device: uid,
                    map: default_map(),
                    gain_db: 0.0,
                    muted: false,
                });
            });
        }
    };
    let add_quick = add.clone();
    rsx! {
        div { class: "mix-add output",
            span { class: "mix-add-plus", "+" }
            "Add output"
            if devs.is_empty() {
                span { class: "dim-note", "no more output devices" }
            } else {
                select {
                    onchange: move |e| {
                        let v = e.value();
                        if !v.is_empty() {
                            add(v);
                        }
                    },
                    option { value: "", selected: true, "choose device…" }
                    for d in devs.iter() {
                        option { value: "{d.uid}",
                            "{d.name} ({d.output_channels} out){output_hint(&d.name)}"
                        }
                    }
                }
            }
            if let Some(b) = broadcast {
                button {
                    class: "chip on",
                    title: "the virtual device Discord / FaceTime / Zoom pick as their microphone",
                    onclick: move |_| add_quick(b.uid.clone()),
                    "→ {b.name} (virtual mic)"
                }
            }
        }
    }
}

#[component]
fn SourcePicker(mix: String, tab: PickerTab, targets: HostTargets) -> Element {
    let handle = state::use_patchbay();
    let device = PICKER_DEVICE.read().clone();
    let bundle = PICKER_BUNDLE.read().clone();

    let add = {
        move |src: MixSourceConfig| {
            *PICKER.write() = None;
            PICKER_BUNDLE.write().clear();
            edit(handle.clone(), &mix, |c| c.sources.push(src));
        }
    };

    let mut apps = targets.apps.clone();
    apps.sort_by_key(|a| (!a.playing, a.name.to_lowercase()));
    let inputs: Vec<HostDevice> = targets
        .devices
        .iter()
        .filter(|d| d.input_channels > 0)
        .cloned()
        .collect();
    let outs = output_devices(&targets);
    let app_source = {
        let device = device.clone();
        move |bundle_id: String| MixSourceConfig {
            kind: source_kind::APP.to_owned(),
            target: bundle_id,
            device: device.clone(),
            map: default_map(),
            gain_db: 0.0,
            muted: false,
        }
    };
    let tab_btn = |t: PickerTab, label: &'static str| {
        rsx! {
            button {
                class: if tab == t { "tab on" } else { "tab" },
                onclick: move |_| *PICKER.write() = Some(t),
                "{label}"
            }
        }
    };

    rsx! {
        div { class: "mix-picker",
            div { class: "mix-picker-tabs",
                {tab_btn(PickerTab::Apps, "Apps")}
                {tab_btn(PickerTab::Inputs, "Input devices")}
                {tab_btn(PickerTab::System, "System audio")}
                button { class: "chip picker-close", onclick: move |_| *PICKER.write() = None, "close" }
            }
            match tab {
                PickerTab::Apps => {
                    let add_row = add.clone();
                    let add_bundle = add;
                    let src_row = app_source.clone();
                    let src_bundle = app_source;
                    rsx! {
                        div { class: "picker-opts",
                            span { class: "label", "take" }
                            select {
                                onchange: move |e| *PICKER_DEVICE.write() = e.value(),
                                option { value: "", selected: device.is_empty(), "the app's stereo mixdown" }
                                for d in outs.iter() {
                                    option { value: "{d.uid}", selected: device == d.uid,
                                        "only what it plays to {d.name} ({d.output_channels} ch)"
                                    }
                                }
                            }
                        }
                        div { class: "picker-list",
                            if apps.is_empty() {
                                div { class: "dim-note", "No audio apps running." }
                            }
                            for a in apps {
                                {
                                    let add = add_row.clone();
                                    let src = src_row.clone();
                                    let id = a.bundle_id.clone();
                                    rsx! {
                                        button {
                                            key: "{a.bundle_id}",
                                            class: "picker-row",
                                            onclick: move |_| add(src(id.clone())),
                                            span { class: if a.playing { "play-dot on" } else { "play-dot" } }
                                            span { class: "picker-name", "{a.name}" }
                                            span { class: "dim-note", "{a.bundle_id}" }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "picker-opts",
                            input {
                                class: "search",
                                placeholder: "or a bundle id (app not running yet, e.g. com.cockos.reaper)",
                                value: "{bundle}",
                                oninput: move |e| *PICKER_BUNDLE.write() = e.value(),
                            }
                            button {
                                class: "chip",
                                disabled: bundle.trim().is_empty(),
                                onclick: move |_| {
                                    let id = PICKER_BUNDLE.peek().trim().to_owned();
                                    if !id.is_empty() {
                                        add_bundle(src_bundle(id));
                                    }
                                },
                                "add"
                            }
                        }
                    }
                }
                PickerTab::Inputs => rsx! {
                    div { class: "picker-list",
                        if inputs.is_empty() {
                            div { class: "dim-note", "No input devices." }
                        }
                        for d in inputs {
                            {
                                let add = add.clone();
                                let uid = d.uid.clone();
                                rsx! {
                                    button {
                                        key: "{d.uid}",
                                        class: "picker-row",
                                        onclick: move |_| add(MixSourceConfig {
                                            kind: source_kind::INPUT.to_owned(),
                                            target: uid.clone(),
                                            device: String::new(),
                                            map: default_map(),
                                            gain_db: 0.0,
                                            muted: false,
                                        }),
                                        span { class: "picker-name", "{d.name}" }
                                        span { class: "dim-note", "{d.input_channels} in" }
                                    }
                                }
                            }
                        }
                    }
                },
                PickerTab::System => rsx! {
                    div { class: "picker-opts",
                        span { class: "dim-note", "Everything the system plays, except Patchbay itself." }
                        button {
                            class: "chip on",
                            onclick: move |_| add(MixSourceConfig {
                                kind: source_kind::SYSTEM.to_owned(),
                                target: String::new(),
                                device: String::new(),
                                map: default_map(),
                                gain_db: 0.0,
                                muted: false,
                            }),
                            "Add system audio"
                        }
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchbay_proto::HostApp;

    fn targets() -> HostTargets {
        HostTargets {
            supported: true,
            apps: vec![HostApp {
                bundle_id: "com.cockos.reaper".into(),
                name: "REAPER".into(),
                playing: true,
                ..HostApp::default()
            }],
            devices: vec![
                HostDevice {
                    uid: "galaxy".into(),
                    name: "Galaxy32".into(),
                    input_channels: 64,
                    output_channels: 64,
                    ..HostDevice::default()
                },
                HostDevice {
                    uid: "Broadcast_UID".into(),
                    name: "Broadcast".into(),
                    input_channels: 2,
                    output_channels: 2,
                    ..HostDevice::default()
                },
            ],
        }
    }

    fn src(kind: &str, target: &str, device: &str) -> MixSourceConfig {
        MixSourceConfig {
            kind: kind.into(),
            target: target.into(),
            device: device.into(),
            map: default_map(),
            gain_db: 0.0,
            muted: false,
        }
    }

    #[test]
    fn channel_picks() {
        let p = pair_picks(64, false, "outs");
        assert_eq!(p.len(), 32);
        assert_eq!(p.get(16).map(|x| x.0.as_str()), Some("outs 33–34"));
        assert_eq!(p.get(16).map(|x| x.1.as_str()), Some("32:0,33:1"));
        let o = pair_picks(64, true, "outs");
        assert_eq!(o.get(16).map(|x| x.1.as_str()), Some("0:32,1:33"));
        assert_eq!(
            pair_picks(1, false, "ins").first().map(|x| x.1.as_str()),
            Some("0:0,0:1")
        );
        assert_eq!(mono_picks(2).get(1).map(|x| x.1.as_str()), Some("1:0,1:1"));
    }

    #[test]
    fn labels_resolve_through_targets() {
        let t = targets();
        assert_eq!(
            source_label(&t, &src("app", "com.cockos.reaper", "")),
            "REAPER"
        );
        assert_eq!(
            source_label(&t, &src("app", "com.cockos.reaper", "galaxy")),
            "REAPER → Galaxy32"
        );
        assert_eq!(
            source_label(&t, &src("app", "com.hnc.Discord", "")),
            "Discord"
        );
        assert_eq!(source_label(&t, &src("input", "galaxy", "")), "Galaxy32");
        assert_eq!(source_label(&t, &src("system", "", "")), "System audio");
        let outs = output_devices(&t);
        assert_eq!(outs.first().map(|d| d.name.as_str()), Some("Broadcast"));
    }
}

/// Run a virtual-device RPC, surface its error, then refresh.
fn vdev_op<F, Fut, E>(handle: PatchbayHandle, what: &'static str, op: F)
where
    F: FnOnce(PatchbayHandle) -> Fut + 'static,
    Fut: std::future::Future<Output = Result<(), E>> + 'static,
    E: std::fmt::Display,
{
    spawn_forever(async move {
        match op(handle.clone()).await {
            Ok(()) => ERROR.write().clear(),
            Err(e) => *ERROR.write() = format!("{what}: {e}"),
        }
        refresh_virtual(&handle).await;
        refresh_targets(&handle).await;
    });
}

fn vdev_create(handle: PatchbayHandle) {
    let name = VNEW_NAME.peek().trim().to_owned();
    if name.is_empty() {
        *ERROR.write() = "device name is empty".into();
        return;
    }
    let channels = *VNEW_CHANNELS.peek();
    VNEW_NAME.write().clear();
    vdev_op(handle, "create virtual device", move |h| async move {
        h.0.create_virtual_device(name, channels).await.map(|_| ())
    });
}

fn vdev_rename(handle: PatchbayHandle) {
    let Some((uid, name)) = VRENAME.peek().clone() else {
        return;
    };
    *VRENAME.write() = None;
    vdev_op(handle, "rename virtual device", move |h| async move {
        h.0.rename_virtual_device(uid, name).await
    });
}

fn vdev_remove(handle: PatchbayHandle, uid: String) {
    vdev_op(handle, "remove virtual device", move |h| async move {
        h.0.remove_virtual_device(uid).await
    });
}

/// Patchbay's virtual devices (loopbacks from `Patchbay.driver`): list,
/// create, rename, remove — all at runtime.
#[component]
fn VirtualDevicesPanel() -> Element {
    let handle = state::use_patchbay();
    let st = VDEVS.read().clone();
    let renaming = VRENAME.read().clone();
    rsx! {
        div { class: "vdev-panel",
            h3 { "Virtual devices" }
            if !st.driver_loaded {
                div { class: "dim-note",
                    "Patchbay.driver isn't loaded — install it with packaging/macos/install-driver.sh."
                }
            }
            for d in st.devices.iter() {
                {
                    let editing = renaming.as_ref().filter(|(u, _)| *u == d.uid).map(|(_, n)| n.clone());
                    let (h1, h2, h3) = (handle.clone(), handle.clone(), handle.clone());
                    let (uid_edit, uid_rm, name) = (d.uid.clone(), d.uid.clone(), d.name.clone());
                    rsx! {
                        div { key: "{d.uid}", class: "vdev-row",
                            if let Some(draft) = editing {
                                input {
                                    class: "search",
                                    value: "{draft}",
                                    oninput: move |e| {
                                        let v = e.value();
                                        if let Some((_, n)) = VRENAME.write().as_mut() {
                                            *n = v;
                                        }
                                    },
                                    onkeydown: move |e: Event<KeyboardData>| {
                                        if e.key() == Key::Enter {
                                            vdev_rename(h1.clone());
                                        } else if e.key() == Key::Escape {
                                            *VRENAME.write() = None;
                                        }
                                    },
                                }
                                button { class: "chip", onclick: move |_| vdev_rename(h2.clone()), "save" }
                            } else {
                                div { class: "vdev-name", "{d.name}" }
                                div { class: "vdev-actions",
                                    span { class: "dim-note", "{d.channels} ch" }
                                    button {
                                        class: "chip",
                                        title: "Rename (apps keep their selection)",
                                        onclick: move |_| *VRENAME.write() = Some((uid_edit.clone(), name.clone())),
                                        "rename"
                                    }
                                    button {
                                        class: "chip danger",
                                        title: "Remove this virtual device",
                                        onclick: move |_| vdev_remove(h3.clone(), uid_rm.clone()),
                                        "remove"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if st.driver_loaded {
                div { class: "mix-new",
                    input {
                        class: "search",
                        placeholder: "new device (e.g. Stream Mix)",
                        value: "{VNEW_NAME}",
                        oninput: move |e| *VNEW_NAME.write() = e.value(),
                    }
                    select {
                        value: "{VNEW_CHANNELS}",
                        onchange: move |e| *VNEW_CHANNELS.write() = e.value().parse().unwrap_or(2),
                        for n in [1_u32, 2, 4, 8, 16, 32, 64] {
                            option { value: "{n}", "{n} ch" }
                        }
                    }
                    button {
                        class: "chip",
                        onclick: move |_| vdev_create(handle.clone()),
                        "Create"
                    }
                }
            }
        }
    }
}
