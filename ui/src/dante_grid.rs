//! Dante routing grid — the Dante Controller replacement view.
//!
//! Classic subscription matrix: TX channels across the top (grouped by
//! device, vertical labels), RX channels down the left (grouped by
//! device). Device groups collapse; a collapsed pair shows how many
//! subscriptions run between the two devices.
//!
//! Seeing and editing are separate problems. A 64×64 network only fits a
//! phone with cells far smaller than a fingertip, and a mis-tap here
//! re-routes live audio. So the grid zooms freely ([`CELL_PX`]), and with
//! *safe edit* on (the default for touch) a tap only SELECTS a
//! crosspoint: the row and column light up, the [`RouteBar`] names
//! exactly what is selected, arrows nudge it, and one full-size button
//! commits. The bar's pickers can also build a route without touching
//! the grid at all. With safe edit off a click toggles immediately and
//! shift-click routes a stereo pair, as it always has.

use std::collections::HashMap;
use std::rc::Rc;

use dioxus::prelude::*;
use patchbay_proto::{DanteDevice, DanteSubscription};

use crate::state::{self, DANTE_DEVICES, DANTE_ERROR, DANTE_LOADING, PatchbayHandle};

/// Devices with more channels than this start collapsed.
const AUTO_EXPAND_MAX: usize = 40;

/// Crosspoint sizes the zoom buttons step through, in px.
const ZOOM_STEPS: [u32; 7] = [10, 14, 18, 24, 30, 36, 44];
/// Where a mouse-driven window, a touch tablet and a phone start.
const CELL_DESKTOP: u32 = 24;
const CELL_TABLET: u32 = 30;
const CELL_PHONE: u32 = 18;
/// Below this a cell is a coloured square, not a glyph.
const CELL_GLYPH_MIN: u32 = 16;

/// `"tx/<dev>"` / `"rx/<dev>"` → expanded override.
static GRID_EXPANDED: GlobalSignal<HashMap<String, bool>> = Signal::global(HashMap::new);

/// Channel-name filter for the grid (matches TX and RX names).
static GRID_FILTER: GlobalSignal<String> = Signal::global(String::new);

/// Crosspoint size in px — the grid's zoom.
static CELL_PX: GlobalSignal<u32> = Signal::global(|| CELL_DESKTOP);
/// A tap selects and the route bar commits, rather than a tap committing.
static SAFE_EDIT: GlobalSignal<bool> = Signal::global(|| false);
/// [`CELL_PX`] and [`SAFE_EDIT`] have been given this device's defaults
/// (once — after that they are the user's).
static DEFAULTS_SET: GlobalSignal<bool> = Signal::global(|| false);
/// What the route bar is pointing at.
static SELECTION: GlobalSignal<Selection> = Signal::global(Selection::default);
/// The route bar is showing even with nothing selected ("route…").
static BAR_OPEN: GlobalSignal<bool> = Signal::global(|| false);
/// Connect / disconnect the next channel on both sides too.
static STEREO: GlobalSignal<bool> = Signal::global(|| false);
/// The tools menu is open. Only a phone has one: there the tools live
/// behind a button so the grid gets the screen; wider, they are a row.
static MENU_OPEN: GlobalSignal<bool> = Signal::global(|| false);

/// Another engine's network: nothing selected on the last one applies
/// (see `hosts::reset`). Zoom and safe edit are the user's and stay.
pub fn reset() {
    *SELECTION.write() = Selection::default();
    *BAR_OPEN.write() = false;
    *MENU_OPEN.write() = false;
    GRID_EXPANDED.write().clear();
}

/// Zoom and safe edit, for remembering them (`crate::session`).
pub fn prefs() -> (u32, bool) {
    (*CELL_PX.read(), *SAFE_EDIT.read())
}

/// Put back a remembered zoom and safe-edit switch. They are the user's
/// from here on, so the per-screen defaults no longer apply.
pub fn restore(cell: u32, safe_edit: bool) {
    *CELL_PX.write() = cell;
    *SAFE_EDIT.write() = safe_edit;
    *DEFAULTS_SET.write() = true;
}

/// One end or both of a route being built. RX channels are identified by
/// number and TX channels by name, because that is what ARC subscribes
/// with.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
struct Selection {
    rx: Option<(String, u32)>,
    tx: Option<(String, String)>,
}

impl Selection {
    const fn is_empty(&self) -> bool {
        self.rx.is_none() && self.tx.is_none()
    }
}

/// `"coarse|narrow"` as `"1|0"` — what kind of screen this is.
const SCREEN_JS: &str = r#"
return (matchMedia("(pointer: coarse)").matches ? "1" : "0") + "|" + (innerWidth <= 760 ? "1" : "0");
"#;

/// The scroll viewport's width, for "fit".
const WIDTH_JS: &str = r#"
const el = document.querySelector(".dante-scroll");
return el ? el.clientWidth : 0;
"#;

/// After a nudge: keep the selected crosspoint on screen. The cells
/// carry a scroll margin the size of the sticky headers.
const REVEAL_JS: &str = r#"
requestAnimationFrame(() => {
  const el = document.querySelector(".dante-grid .cell.sel") || document.querySelector(".dante-grid .rx-ch.sel");
  if (el) el.scrollIntoView({ block: "nearest", inline: "nearest" });
});
"#;

/// Width of the sticky RX label column at a zoom — mirrors `--label-w`
/// in `views.css`.
fn label_width(cell: u32) -> f64 {
    (f64::from(cell) * 6.5).clamp(84.0, 220.0)
}

/// Rough drawn width of a collapsed device's column: its name, the
/// `▸ 64ch` count and padding. Only "fit" uses it, so close is enough.
fn collapsed_width(name: &str) -> f64 {
    let chars = f64::from(u32::try_from(name.chars().count()).unwrap_or(u32::MAX));
    chars.mul_add(7.0, 64.0)
}

/// Largest zoom step at which `cols` crosspoint columns, plus `fixed` px
/// of columns that don't zoom (collapsed devices), fit in `width`; the
/// smallest step when none does.
fn fit_cell(width: f64, cols: usize, fixed: f64) -> u32 {
    let cols = f64::from(u32::try_from(cols).unwrap_or(u32::MAX));
    ZOOM_STEPS
        .iter()
        .rev()
        .copied()
        .find(|&c| label_width(c) + fixed + cols * f64::from(c) <= width)
        .unwrap_or(ZOOM_STEPS[0])
}

/// The next zoom step up or down from `px` (which needn't be a step).
fn zoom_step(px: u32, zoom_in: bool) -> u32 {
    if zoom_in {
        ZOOM_STEPS.iter().copied().find(|&s| s > px).unwrap_or(px)
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|&s| s < px)
            .unwrap_or(px)
    }
}

/// Move through `list` from `current`: clamped at the ends; from nothing,
/// forward starts at the first item and backward at the last.
fn nudge<T: PartialEq + Clone>(list: &[T], current: Option<&T>, forward: bool) -> Option<T> {
    let at = current.and_then(|c| list.iter().position(|x| x == c));
    let next = match (at, forward) {
        (Some(i), true) => i.saturating_add(1).min(list.len().saturating_sub(1)),
        (Some(i), false) => i.saturating_sub(1),
        (None, true) => 0,
        (None, false) => list.len().saturating_sub(1),
    };
    list.get(next).cloned()
}

fn is_expanded(axis: &str, dev: &DanteDevice, channel_count: usize) -> bool {
    GRID_EXPANDED
        .read()
        .get(&format!("{axis}/{}", dev.name))
        .copied()
        .unwrap_or(channel_count <= AUTO_EXPAND_MAX)
}

fn toggle_expanded(axis: &str, dev: &str, now: bool) {
    GRID_EXPANDED.write().insert(format!("{axis}/{dev}"), !now);
}

/// Subscription of `rx_dev`'s channel `rx_ch` if any: (`tx_device`,
/// `tx_channel_name`, healthy). Healthy ARC statuses: 1 (inferno-aoip's
/// "connected"), 9 (Dante dynamic/unicast), 14 (Dante static/multicast).
fn sub_of(rx_dev: &DanteDevice, rx_ch: u32) -> Option<(&str, &str, bool)> {
    rx_dev
        .subscriptions
        .iter()
        .find(|s| s.rx_channel == rx_ch)
        .map(|s| {
            (
                s.tx_device.as_str(),
                s.tx_channel.as_str(),
                matches!(s.status, 1 | 9 | 14),
            )
        })
}

/// Subscriptions running from `tx_dev` into `rx_dev` (for collapsed
/// device-pair cells).
fn pair_count(rx_dev: &DanteDevice, tx_dev: &str) -> usize {
    rx_dev
        .subscriptions
        .iter()
        .filter(|s| s.tx_device == tx_dev)
        .count()
}

/// What the route bar's button would do right now.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Action {
    /// Nothing actionable yet; say what is missing.
    Hint(&'static str),
    /// Subscribe the selected RX to the selected TX, replacing `replaces`
    /// (the source it has now, as `device : channel`).
    Connect { replaces: Option<String> },
    /// Clear the selected RX's subscription (`from`, as `device : channel`).
    Disconnect { from: String },
}

fn action_for(sel: &Selection, devices: &[DanteDevice]) -> Action {
    let Some((rx_dev, rx_ch)) = &sel.rx else {
        return Action::Hint(if sel.tx.is_some() {
            "Now pick the receiver: tap an RX name or a crosspoint."
        } else {
            "Pick a receiver and a source — tap the grid, or use the pickers."
        });
    };
    let current = devices
        .iter()
        .find(|d| &d.name == rx_dev)
        .and_then(|d| sub_of(d, *rx_ch))
        .map(|(d, c, _)| (d.to_owned(), c.to_owned()));
    let shown = |(d, c): &(String, String)| format!("{d} : {c}");
    match (&sel.tx, current) {
        (Some(tx), Some(cur)) if *tx == cur => Action::Disconnect { from: shown(&cur) },
        (Some(_), cur) => Action::Connect {
            replaces: cur.as_ref().map(shown),
        },
        (None, Some(cur)) => Action::Disconnect { from: shown(&cur) },
        (None, None) => {
            Action::Hint("Not subscribed. Pick a source: tap a TX name or a crosspoint.")
        }
    }
}

/// `(rx_channel, tx_channel_name)` pairs for a route: one, or with
/// `stereo` the next channel on both sides as well when both exist.
fn route_ops(
    rx_dev: &DanteDevice,
    rx_ch: u32,
    tx_dev: Option<&DanteDevice>,
    tx_ch: &str,
    stereo: bool,
) -> Vec<(u32, String)> {
    let mut ops = vec![(rx_ch, tx_ch.to_owned())];
    let next_rx = rx_ch.saturating_add(1);
    if !stereo || !rx_dev.rx.iter().any(|c| c.number == next_rx) {
        return ops;
    }
    match tx_dev {
        // Disconnecting: only the RX side matters.
        None => ops.push((next_rx, String::new())),
        Some(tx_dev) => {
            let next_tx = tx_dev
                .tx
                .iter()
                .position(|c| c.name == tx_ch)
                .and_then(|i| tx_dev.tx.get(i.saturating_add(1)));
            if let Some(next_tx) = next_tx {
                ops.push((next_rx, next_tx.name.clone()));
            }
        }
    }
    ops
}

/// Toggle (or move) subscriptions. The mirror updates OPTIMISTICALLY so
/// the cell flips instantly; a delayed re-scan verifies against the
/// network (ARC round-trips are seconds).
///
/// `ops` = `(rx_channel, tx_channel_name)` pairs — one for a plain route,
/// two for a stereo pair.
fn apply_route(
    handle: PatchbayHandle,
    rx_device: String,
    ops: Vec<(u32, String)>,
    tx_device: String,
    is_sub: bool,
) {
    {
        let mut devs = DANTE_DEVICES.write();
        if let Some(d) = devs.iter_mut().find(|d| d.name == rx_device) {
            for (rx_channel, tx_channel) in &ops {
                d.subscriptions.retain(|s| s.rx_channel != *rx_channel);
                if !is_sub {
                    d.subscriptions.push(DanteSubscription {
                        rx_channel: *rx_channel,
                        tx_channel: tx_channel.clone(),
                        tx_device: tx_device.clone(),
                        status: 1,
                    });
                }
            }
        }
    }
    spawn(async move {
        let mut failed = false;
        for (rx_channel, tx_channel) in ops {
            let res = if is_sub {
                handle
                    .0
                    .dante_unsubscribe(rx_device.clone(), rx_channel)
                    .await
            } else {
                handle
                    .0
                    .dante_subscribe(rx_device.clone(), rx_channel, tx_device.clone(), tx_channel)
                    .await
            };
            if let Err(e) = res {
                *DANTE_ERROR.write() = format!("subscription change failed: {e}");
                failed = true;
            }
        }
        if !failed {
            // Let the device settle, then fetch the truth.
            state::sleep_secs(2).await;
        }
        state::refresh_dante(&handle).await;
    });
}

/// Do what the route bar's button says, for the current selection.
fn commit(handle: PatchbayHandle, devices: &[DanteDevice], tx_all: &[DanteDevice]) {
    let sel = SELECTION.peek().clone();
    let stereo = *STEREO.peek();
    let Some((rx_name, rx_ch)) = sel.rx.clone() else {
        return;
    };
    let Some(rx_dev) = devices.iter().find(|d| d.name == rx_name) else {
        return;
    };
    match action_for(&sel, devices) {
        Action::Hint(_) => {}
        Action::Disconnect { .. } => {
            let ops = route_ops(rx_dev, rx_ch, None, "", stereo);
            apply_route(handle, rx_name, ops, String::new(), true);
        }
        Action::Connect { .. } => {
            let Some((tx_name, tx_ch)) = sel.tx else {
                return;
            };
            let tx_dev = tx_all.iter().find(|d| d.name == tx_name);
            // A source that is gone from the scan still subscribes by
            // name; it just can't offer a "next channel".
            let ops = route_ops(rx_dev, rx_ch, tx_dev, &tx_ch, stereo && tx_dev.is_some());
            apply_route(handle, rx_name, ops, tx_name, false);
        }
    }
}

/// How a crosspoint relates to the selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    /// In the selected column.
    Col,
    /// The selected crosspoint itself.
    Sel,
}

/// Static class strings, so 16 000 cells don't each allocate one.
const fn cell_class(is_sub: bool, healthy: bool, mark: Mark) -> &'static str {
    match (is_sub, healthy, mark) {
        (true, true, Mark::None) => "cell sub ok",
        (true, true, Mark::Col) => "cell sub ok in-col",
        (true, true, Mark::Sel) => "cell sub ok sel",
        (true, false, Mark::None) => "cell sub warn",
        (true, false, Mark::Col) => "cell sub warn in-col",
        (true, false, Mark::Sel) => "cell sub warn sel",
        (false, _, Mark::None) => "cell",
        (false, _, Mark::Col) => "cell in-col",
        (false, _, Mark::Sel) => "cell sel",
    }
}

#[component]
pub fn DanteGrid() -> Element {
    let handle = state::use_patchbay();
    let devices = DANTE_DEVICES.read().clone();
    let loading = *DANTE_LOADING.read();
    let error = DANTE_ERROR.read().clone();
    let cell_px = *CELL_PX.read();
    let safe_edit = *SAFE_EDIT.read();
    let selection = SELECTION.read().clone();
    let bar_open = *BAR_OPEN.read() || !selection.is_empty();
    let menu_open = *MENU_OPEN.read();

    // First open: scan automatically.
    use_future({
        let handle = handle.clone();
        move || {
            let handle = handle.clone();
            async move {
                if DANTE_DEVICES.peek().is_empty() {
                    state::refresh_dante(&handle).await;
                }
            }
        }
    });

    // Once per session: a phone starts zoomed out, and anything touched
    // starts in safe edit. After that both belong to the user.
    use_future(|| async {
        if *DEFAULTS_SET.peek() {
            return;
        }
        if let Ok(screen) = document::eval(SCREEN_JS).join::<String>().await
            // A remembered zoom may have been restored while this waited.
            && !*DEFAULTS_SET.peek()
        {
            let (coarse, narrow) = (screen.starts_with('1'), screen.ends_with('1'));
            *CELL_PX.write() = match (coarse, narrow) {
                (_, true) => CELL_PHONE,
                (true, false) => CELL_TABLET,
                (false, false) => CELL_DESKTOP,
            };
            *SAFE_EDIT.write() = coarse;
        }
        *DEFAULTS_SET.write() = true;
    });

    let refresh = {
        let handle = handle.clone();
        move |_: Event<MouseData>| {
            let handle = handle.clone();
            spawn(async move { state::refresh_dante(&handle).await });
        }
    };

    // TX devices that only exist as subscription targets (a console
    // that didn't answer mDNS — different VLAN scope, or just not
    // browsable). Without these columns, live routes to them would be
    // invisible; subscribing still works because ARC subscribe talks
    // to the RX device with the TX side as names.
    let phantoms: Vec<DanteDevice> = {
        let known: std::collections::HashSet<&str> =
            devices.iter().map(|d| d.name.as_str()).collect();
        let mut by_dev: HashMap<String, Vec<String>> = HashMap::new();
        for d in &devices {
            for s in &d.subscriptions {
                if !s.tx_device.is_empty() && !known.contains(s.tx_device.as_str()) {
                    let chans = by_dev.entry(s.tx_device.clone()).or_default();
                    if !chans.contains(&s.tx_channel) {
                        chans.push(s.tx_channel.clone());
                    }
                }
            }
        }
        let mut phantoms: Vec<DanteDevice> = by_dev
            .into_iter()
            .map(|(name, mut chans)| {
                chans.sort();
                DanteDevice {
                    name,
                    ip: String::new(),
                    arc_port: 0,
                    tx: chans
                        .into_iter()
                        .enumerate()
                        .map(|(i, name)| patchbay_proto::DanteChannel {
                            number: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
                            name,
                        })
                        .collect(),
                    rx: Vec::new(),
                    subscriptions: Vec::new(),
                    unreachable: true,
                }
            })
            .collect();
        phantoms.sort_by(|a, b| a.name.cmp(&b.name));
        phantoms
    };

    // Channel filter: matching channels stay, empty devices drop out.
    let filter = GRID_FILTER.read().to_lowercase();
    let keep = |name: &str| filter.is_empty() || name.to_lowercase().contains(&filter);
    let mut tx_devices: Vec<DanteDevice> = devices
        .iter()
        .filter(|d| !d.tx.is_empty())
        .cloned()
        .chain(phantoms)
        .collect();
    let mut rx_devices: Vec<DanteDevice> = devices
        .iter()
        .filter(|d| !d.rx.is_empty())
        .cloned()
        .collect();
    // Unfiltered, for the route bar's pickers and for "next channel".
    let tx_all = Rc::new(tx_devices.clone());
    let rx_all = Rc::new(rx_devices.clone());
    if !filter.is_empty() {
        for d in &mut tx_devices {
            d.tx.retain(|c| keep(&c.name));
        }
        tx_devices.retain(|d| !d.tx.is_empty());
        for d in &mut rx_devices {
            d.rx.retain(|c| keep(&c.name));
        }
        rx_devices.retain(|d| !d.rx.is_empty());
    }

    // What is actually drawn, in order — what the arrows walk through,
    // and what "fit" has to fit.
    let tx_cols: Rc<Vec<(String, String)>> = Rc::new(
        tx_devices
            .iter()
            .filter(|d| is_expanded("tx", d, d.tx.len()))
            .flat_map(|d| d.tx.iter().map(|c| (d.name.clone(), c.name.clone())))
            .collect(),
    );
    let rx_rows: Rc<Vec<(String, u32)>> = Rc::new(
        rx_devices
            .iter()
            .filter(|d| is_expanded("rx", d, d.rx.len()))
            .flat_map(|d| d.rx.iter().map(|c| (d.name.clone(), c.number)))
            .collect(),
    );
    let drawn_cols: usize = tx_devices
        .iter()
        .filter(|d| is_expanded("tx", d, d.tx.len()))
        .map(|d| d.tx.len().max(1))
        .sum();
    let collapsed_px: f64 = tx_devices
        .iter()
        .filter(|d| !is_expanded("tx", d, d.tx.len()))
        .map(|d| collapsed_width(&d.name))
        .sum();

    let fit = move |_| {
        spawn(async move {
            if let Ok(width) = document::eval(WIDTH_JS).join::<f64>().await
                && width > 0.0
            {
                *CELL_PX.write() = fit_cell(width, drawn_cols, collapsed_px);
            }
        });
    };

    // Arrow keys walk the selection, Enter commits, Escape lets go.
    let devices_rc = Rc::new(devices.clone());
    let on_key = {
        let handle = handle.clone();
        let devices = devices_rc.clone();
        let tx_all = tx_all.clone();
        let tx_cols = tx_cols.clone();
        let rx_rows = rx_rows.clone();
        move |e: Event<KeyboardData>| {
            let moved = match e.key() {
                Key::ArrowLeft => move_tx(&tx_cols, false),
                Key::ArrowRight => move_tx(&tx_cols, true),
                Key::ArrowUp => move_rx(&rx_rows, false),
                Key::ArrowDown => move_rx(&rx_rows, true),
                Key::Enter => {
                    commit(handle.clone(), &devices, &tx_all);
                    false
                }
                Key::Escape => {
                    *SELECTION.write() = Selection::default();
                    *BAR_OPEN.write() = false;
                    false
                }
                _ => return,
            };
            e.prevent_default();
            if moved {
                document::eval(REVEAL_JS);
            }
        }
    };

    let view_class = match (cell_px < CELL_GLYPH_MIN, safe_edit) {
        (true, true) => "dante-view tiny safe",
        (true, false) => "dante-view tiny",
        (false, true) => "dante-view safe",
        (false, false) => "dante-view",
    };

    rsx! {
        div { class: "{view_class}", style: "--cell: {cell_px}px;",
            // Head and tools share a box so the phone's menu can hang off it.
            div { class: "dante-top",
                div { class: "view-head",
                    span { class: "view-title", "Route" }
                    crate::devices::ContextTag {}
                    span { class: "view-sub",
                        if safe_edit {
                            "Dante subscriptions — tap a crosspoint, then connect"
                        } else {
                            "Dante subscriptions — click a cell to route TX → RX"
                        }
                    }
                    div { class: "view-head-actions",
                        span { class: "label dante-legend",
                            span { class: "cell-demo ok", "✓" } " subscribed  "
                            span { class: "cell-demo warn", "!" } " unresolved"
                        }
                        div { class: "zoom-group", title: "Grid zoom",
                            button {
                                class: "chip",
                                "aria-label": "zoom out",
                                disabled: cell_px <= ZOOM_STEPS[0],
                                onclick: move |_| {
                                    let px = *CELL_PX.peek();
                                    *CELL_PX.write() = zoom_step(px, false);
                                },
                                "−"
                            }
                            button { class: "chip", title: "Fit every column in the window", onclick: fit, "fit" }
                            button {
                                class: "chip",
                                "aria-label": "zoom in",
                                disabled: zoom_step(cell_px, true) == cell_px,
                                onclick: move |_| {
                                    let px = *CELL_PX.peek();
                                    *CELL_PX.write() = zoom_step(px, true);
                                },
                                "+"
                            }
                        }
                        // Lit while a filter is hiding channels, so a short
                        // grid is never a mystery with the menu shut.
                        button {
                            class: if menu_open || !filter.is_empty() { "chip on menu-toggle" } else { "chip menu-toggle" },
                            "aria-label": "grid tools",
                            "aria-expanded": "{menu_open}",
                            onclick: move |_| {
                                let open = *MENU_OPEN.peek();
                                *MENU_OPEN.write() = !open;
                            },
                            "⋯"
                        }
                    }
                }
                if menu_open {
                    button {
                        class: "dante-tools-scrim",
                        "aria-label": "close menu",
                        onclick: move |_| *MENU_OPEN.write() = false,
                    }
                }
                div { class: if menu_open { "dante-tools open" } else { "dante-tools" },
                    button {
                        class: "chip",
                        onclick: move |e| {
                            *MENU_OPEN.write() = false;
                            refresh(e);
                        },
                        if loading { "scanning…" } else { "rescan network" }
                    }
                    input {
                        class: "search",
                        placeholder: "filter channels…",
                        value: "{GRID_FILTER}",
                        oninput: move |e| *GRID_FILTER.write() = e.value(),
                    }
                    button {
                        class: if safe_edit { "chip on" } else { "chip" },
                        title: "On: a tap selects a crosspoint and the bar below connects it. Off: a click routes immediately (shift = stereo pair).",
                        onclick: move |_| {
                            let on = *SAFE_EDIT.peek();
                            *SAFE_EDIT.write() = !on;
                        },
                        if safe_edit { "safe edit: on" } else { "safe edit: off" }
                    }
                    button {
                        class: if bar_open { "chip on" } else { "chip" },
                        title: "Build a route from lists instead of the grid",
                        onclick: move |_| {
                            *MENU_OPEN.write() = false;
                            if *BAR_OPEN.peek() || !SELECTION.peek().is_empty() {
                                *BAR_OPEN.write() = false;
                                *SELECTION.write() = Selection::default();
                            } else {
                                *BAR_OPEN.write() = true;
                            }
                        },
                        "route from lists…"
                    }
                    span { class: "label", "{devices.len()} device(s)" }
                }
            }
            crate::ui::ErrorBar { message: error, on_dismiss: move |()| DANTE_ERROR.write().clear() }
            if devices.is_empty() && !loading {
                crate::ui::EmptyState { title: "No Dante devices answered".to_owned(),
                    p {
                        "Nothing replied to mDNS on this network. Studio gear powered off, or "
                        "the Dante stack not running?"
                    }
                    p { class: "dim-note",
                        "Local Network permission has to be granted for discovery to see "
                        "anything — Settings says whether it is. On Linux the Inferno units "
                        "are in Settings too."
                    }
                }
            } else {
                div { class: "dante-scroll", tabindex: "0", onkeydown: on_key,
                    table { class: "dante-grid",
                        thead {
                            // Row 1: TX device headers.
                            tr {
                                th { class: "corner",
                                    span { class: "corner-tx", "TX → " }
                                    span { class: "corner-rx", "RX ↓" }
                                }
                                for dev in &tx_devices {
                                    {
                                        let expanded = is_expanded("tx", dev, dev.tx.len());
                                        let name = dev.name.clone();
                                        let cols = if expanded { dev.tx.len().max(1) } else { 1 };
                                        let accent = state::auto_color(&dev.name);
                                        rsx! {
                                            th {
                                                class: match (dev.unreachable, expanded) {
                                                    (true, true) => "dev-h unreachable",
                                                    (true, false) => "dev-h unreachable collapsed",
                                                    (false, true) => "dev-h",
                                                    (false, false) => "dev-h collapsed",
                                                },
                                                style: "border-top: 2px solid {accent};",
                                                colspan: "{cols}",
                                                onclick: move |_| toggle_expanded("tx", &name, expanded),
                                                // Sticks to the left edge while its columns scroll by,
                                                // so a wide device never loses its name.
                                                span { class: "dev-h-name",
                                                    "{dev.name} "
                                                    span { class: "dev-count",
                                                        if expanded { "▾" } else { "▸ {dev.tx.len()}ch" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // Row 2: TX channel names (vertical). Tap one to pick the source.
                            tr {
                                th { class: "corner corner-slim" }
                                for dev in &tx_devices {
                                    if is_expanded("tx", dev, dev.tx.len()) {
                                        for ch in &dev.tx {
                                            {
                                                let on = selection.tx.as_ref().is_some_and(|(d, c)| d == &dev.name && c == &ch.name);
                                                let pick = (dev.name.clone(), ch.name.clone());
                                                rsx! {
                                                    th {
                                                        class: if on { "tx-ch sel" } else { "tx-ch" },
                                                        key: "{dev.name}:{ch.number}",
                                                        title: "{dev.name} TX {ch.number} — tap to pick as the source",
                                                        onclick: move |_| SELECTION.write().tx = Some(pick.clone()),
                                                        span { class: "tx-ch-label", "{ch.name}" }
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        th { class: "tx-ch collapsed" }
                                    }
                                }
                            }
                        }
                        tbody {
                            for rx_dev in &rx_devices {
                                {
                                    let rx_expanded = is_expanded("rx", rx_dev, rx_dev.rx.len());
                                    let rx_name = rx_dev.name.clone();
                                    let rx_accent = state::auto_color(&rx_dev.name);
                                    let sel_rx_here = selection.rx.as_ref().filter(|(d, _)| d == &rx_dev.name).map(|(_, n)| *n);
                                    rsx! {
                                        // RX device header row (aggregates when collapsed).
                                        tr { class: "rx-dev-row", key: "dev-{rx_dev.name}",
                                            th {
                                                class: if rx_dev.unreachable { "rx-dev unreachable" } else { "rx-dev" },
                                                style: "border-left: 3px solid {rx_accent};",
                                                onclick: move |_| toggle_expanded("rx", &rx_name, rx_expanded),
                                                "{rx_dev.name} "
                                                span { class: "dev-count",
                                                    if rx_expanded { "▾" } else { "▸ {rx_dev.rx.len()}ch" }
                                                }
                                            }
                                            for tx_dev in &tx_devices {
                                                {
                                                    let tx_expanded = is_expanded("tx", tx_dev, tx_dev.tx.len());
                                                    let n = pair_count(rx_dev, &tx_dev.name);
                                                    let cols = if tx_expanded { tx_dev.tx.len().max(1) } else { 1 };
                                                    rsx! {
                                                        td { class: "pair-cell", colspan: "{cols}",
                                                            if n > 0 { span { class: "pair-badge", "{n}" } }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        // RX channel rows.
                                        if rx_expanded {
                                            for rx_ch in &rx_dev.rx {
                                                {
                                                    let row_sel = sel_rx_here == Some(rx_ch.number);
                                                    let pick_rx = (rx_dev.name.clone(), rx_ch.number);
                                                    rsx! {
                                                        tr {
                                                            key: "{rx_dev.name}:{rx_ch.number}",
                                                            class: if row_sel { "sel-row" } else { "" },
                                                            th {
                                                                class: if row_sel { "rx-ch sel" } else { "rx-ch" },
                                                                title: "{rx_dev.name} RX {rx_ch.number} — tap to pick as the receiver",
                                                                onclick: move |_| SELECTION.write().rx = Some(pick_rx.clone()),
                                                                "{rx_ch.name}"
                                                            }
                                                            for tx_dev in &tx_devices {
                                                                {
                                                                    let tx_expanded = is_expanded("tx", tx_dev, tx_dev.tx.len());
                                                                    let sub = sub_of(rx_dev, rx_ch.number);
                                                                    // The selected column's name, if it is in this device.
                                                                    let sel_tx_here = selection.tx.as_ref().filter(|(d, _)| d == &tx_dev.name).map(|(_, c)| c.as_str());
                                                                    if tx_expanded {
                                                                        rsx! {
                                                                            for (ti, tx_ch) in tx_dev.tx.iter().enumerate() {
                                                                                {
                                                                                    let is_sub = sub.is_some_and(|(d, c, _)|
                                                                                        d == tx_dev.name && c == tx_ch.name);
                                                                                    let healthy = sub.is_some_and(|(_, _, h)| h);
                                                                                    let mark = match (sel_tx_here == Some(tx_ch.name.as_str()), row_sel) {
                                                                                        (true, true) => Mark::Sel,
                                                                                        (true, false) => Mark::Col,
                                                                                        _ => Mark::None,
                                                                                    };
                                                                                    let handle = handle.clone();
                                                                                    let rxd = rx_dev.name.clone();
                                                                                    let txd = tx_dev.name.clone();
                                                                                    let txc = tx_ch.name.clone();
                                                                                    let rxn = rx_ch.number;
                                                                                    // Shift-click routes this AND the next
                                                                                    // channel on both sides (stereo pair).
                                                                                    let next_tx = tx_dev.tx.get(ti + 1).map(|c| c.name.clone());
                                                                                    let rx_has_next = rx_dev.rx.iter().any(|c| c.number == rxn + 1);
                                                                                    rsx! {
                                                                                        td {
                                                                                            key: "{tx_dev.name}:{tx_ch.number}",
                                                                                            class: cell_class(is_sub, healthy, mark),
                                                                                            title: "{rx_dev.name}:{rx_ch.name} ← {tx_dev.name}:{tx_ch.name}",
                                                                                            onclick: move |e: Event<MouseData>| {
                                                                                                if *SAFE_EDIT.peek() {
                                                                                                    *SELECTION.write() = Selection {
                                                                                                        rx: Some((rxd.clone(), rxn)),
                                                                                                        tx: Some((txd.clone(), txc.clone())),
                                                                                                    };
                                                                                                    return;
                                                                                                }
                                                                                                let mut ops = vec![(rxn, txc.clone())];
                                                                                                if e.modifiers().shift() && rx_has_next {
                                                                                                    if let Some(ntx) = next_tx.clone() {
                                                                                                        ops.push((rxn + 1, ntx));
                                                                                                    }
                                                                                                }
                                                                                                apply_route(handle.clone(), rxd.clone(), ops, txd.clone(), is_sub);
                                                                                            },
                                                                                            if is_sub {
                                                                                                if healthy { "✓" } else { "!" }
                                                                                            }
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    } else {
                                                                        let routed = sub.is_some_and(|(d, _, _)| d == tx_dev.name);
                                                                        rsx! {
                                                                            td { class: if routed { "cell sub ok collapsed" } else { "cell collapsed" },
                                                                                if routed { "✓" }
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
                    }
                }
                if bar_open {
                    RouteBar {
                        devices: devices_rc,
                        tx_all,
                        rx_all,
                        tx_cols,
                        rx_rows,
                    }
                }
            }
        }
    }
}

/// Step the selected source along the drawn columns. `true` if it moved.
fn move_tx(cols: &[(String, String)], forward: bool) -> bool {
    let next = nudge(cols, SELECTION.peek().tx.as_ref(), forward);
    let moved = next.is_some() && next != SELECTION.peek().tx;
    if moved {
        SELECTION.write().tx = next;
    }
    moved
}

/// Step the selected receiver along the drawn rows. `true` if it moved.
fn move_rx(rows: &[(String, u32)], forward: bool) -> bool {
    let next = nudge(rows, SELECTION.peek().rx.as_ref(), forward);
    let moved = next.is_some() && next != SELECTION.peek().rx;
    if moved {
        SELECTION.write().rx = next;
    }
    moved
}

/// The route being built, and the one button that commits it.
///
/// Everything the grid can do, at full size: pickers for both ends (so a
/// route never *needs* a 14px target), arrows to correct a near miss,
/// the stereo-pair switch that shift-click is on a desktop, and a plain
/// statement of what the button will do — including what it replaces.
#[component]
fn RouteBar(
    devices: Rc<Vec<DanteDevice>>,
    tx_all: Rc<Vec<DanteDevice>>,
    rx_all: Rc<Vec<DanteDevice>>,
    tx_cols: Rc<Vec<(String, String)>>,
    rx_rows: Rc<Vec<(String, u32)>>,
) -> Element {
    let handle = state::use_patchbay();
    let sel = SELECTION.read().clone();
    let stereo = *STEREO.read();
    let action = action_for(&sel, &devices);

    let rx_dev_name = sel.rx.as_ref().map(|(d, _)| d.clone()).unwrap_or_default();
    let rx_ch_num = sel.rx.as_ref().map(|(_, n)| *n);
    let tx_dev_name = sel.tx.as_ref().map(|(d, _)| d.clone()).unwrap_or_default();
    let tx_ch_name = sel.tx.as_ref().map(|(_, c)| c.clone()).unwrap_or_default();
    let rx_channels = rx_all
        .iter()
        .find(|d| d.name == rx_dev_name)
        .map(|d| d.rx.clone())
        .unwrap_or_default();
    let tx_channels = tx_all
        .iter()
        .find(|d| d.name == tx_dev_name)
        .map(|d| d.tx.clone())
        .unwrap_or_default();

    let arrow = |label: &'static str, name: &'static str, tx_axis: bool, forward: bool| {
        let tx_cols = tx_cols.clone();
        let rx_rows = rx_rows.clone();
        rsx! {
            button {
                class: "chip nudge",
                "aria-label": "{name}",
                title: "{name}",
                onclick: move |_| {
                    let moved = if tx_axis { move_tx(&tx_cols, forward) } else { move_rx(&rx_rows, forward) };
                    if moved {
                        document::eval(REVEAL_JS);
                    }
                },
                "{label}"
            }
        }
    };

    let (verb, verb_class, detail) = match &action {
        Action::Hint(text) => ("connect", "chip primary", (*text).to_owned()),
        Action::Connect {
            replaces: Some(old),
        } => ("connect", "chip primary", format!("replaces {old}")),
        Action::Connect { replaces: None } => ("connect", "chip primary", String::new()),
        Action::Disconnect { from } => ("disconnect", "chip danger armed", format!("from {from}")),
    };
    let actionable = !matches!(action, Action::Hint(_));

    let pick_rx_dev = {
        let rx_all = rx_all.clone();
        move |e: Event<FormData>| {
            let name = e.value();
            let first = rx_all
                .iter()
                .find(|d| d.name == name)
                .and_then(|d| d.rx.first())
                .map(|c| c.number);
            SELECTION.write().rx = first.map(|n| (name, n));
        }
    };
    let pick_tx_dev = {
        let tx_all = tx_all.clone();
        move |e: Event<FormData>| {
            let name = e.value();
            let first = tx_all
                .iter()
                .find(|d| d.name == name)
                .and_then(|d| d.tx.first())
                .map(|c| c.name.clone());
            SELECTION.write().tx = first.map(|c| (name, c));
        }
    };
    let rx_dev_for_ch = rx_dev_name.clone();
    let tx_dev_for_ch = tx_dev_name.clone();
    let commit_devices = devices;
    let commit_tx = tx_all.clone();

    rsx! {
        div { class: "route-bar",
            div { class: "route-ends",
                div { class: "route-end",
                    span { class: "route-tag rx", "RX" }
                    select { "aria-label": "receiving device", onchange: pick_rx_dev,
                        option { value: "", selected: rx_dev_name.is_empty(), disabled: true, "device…" }
                        for d in rx_all.iter() {
                            option { key: "{d.name}", value: "{d.name}", selected: d.name == rx_dev_name, "{d.name}" }
                        }
                    }
                    select {
                        "aria-label": "receiving channel",
                        disabled: rx_channels.is_empty(),
                        onchange: move |e: Event<FormData>| {
                            if let Ok(n) = e.value().parse::<u32>() {
                                SELECTION.write().rx = Some((rx_dev_for_ch.clone(), n));
                            }
                        },
                        for c in rx_channels.iter() {
                            option { key: "{c.number}", value: "{c.number}", selected: Some(c.number) == rx_ch_num, "{c.name}" }
                        }
                    }
                }
                span { class: "route-arrow", "←" }
                div { class: "route-end",
                    span { class: "route-tag tx", "TX" }
                    select { "aria-label": "source device", onchange: pick_tx_dev,
                        option { value: "", selected: tx_dev_name.is_empty(), disabled: true, "device…" }
                        for d in tx_all.iter() {
                            option { key: "{d.name}", value: "{d.name}", selected: d.name == tx_dev_name, "{d.name}" }
                        }
                    }
                    select {
                        "aria-label": "source channel",
                        disabled: tx_channels.is_empty(),
                        onchange: move |e: Event<FormData>| {
                            SELECTION.write().tx = Some((tx_dev_for_ch.clone(), e.value()));
                        },
                        for c in tx_channels.iter() {
                            option { key: "{c.number}", value: "{c.name}", selected: c.name == tx_ch_name, "{c.name}" }
                        }
                    }
                }
            }
            div { class: "route-actions",
                div { class: "nudge-pad", title: "Move the selection one channel",
                    {arrow("◀", "previous source", true, false)}
                    {arrow("▲", "previous receiver", false, false)}
                    {arrow("▼", "next receiver", false, true)}
                    {arrow("▶", "next source", true, true)}
                }
                button {
                    class: if stereo { "chip on" } else { "chip" },
                    title: "Also route the next channel on both sides (what shift-click does)",
                    onclick: move |_| {
                        let on = *STEREO.peek();
                        *STEREO.write() = !on;
                    },
                    "stereo pair"
                }
                span { class: "route-detail", "{detail}" }
                button {
                    class: "{verb_class} route-commit",
                    disabled: !actionable,
                    onclick: move |_| commit(handle.clone(), &commit_devices, &commit_tx),
                    "{verb}"
                }
                button {
                    class: "chip",
                    "aria-label": "close",
                    onclick: move |_| {
                        *SELECTION.write() = Selection::default();
                        *BAR_OPEN.write() = false;
                    },
                    "✕"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchbay_proto::DanteChannel;

    fn chans(names: &[&str]) -> Vec<DanteChannel> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| DanteChannel {
                number: u32::try_from(i).unwrap_or(0).saturating_add(1),
                name: (*n).to_owned(),
            })
            .collect()
    }

    fn device(name: &str, tx: &[&str], rx: &[&str], subs: &[(u32, &str, &str)]) -> DanteDevice {
        DanteDevice {
            name: name.to_owned(),
            ip: String::new(),
            arc_port: 0,
            tx: chans(tx),
            rx: chans(rx),
            subscriptions: subs
                .iter()
                .map(|(rx, dev, ch)| DanteSubscription {
                    rx_channel: *rx,
                    tx_channel: (*ch).to_owned(),
                    tx_device: (*dev).to_owned(),
                    status: 9,
                })
                .collect(),
            unreachable: false,
        }
    }

    fn sel(rx: Option<(&str, u32)>, tx: Option<(&str, &str)>) -> Selection {
        Selection {
            rx: rx.map(|(d, n)| (d.to_owned(), n)),
            tx: tx.map(|(d, c)| (d.to_owned(), c.to_owned())),
        }
    }

    #[test]
    fn the_button_says_what_it_will_do_including_what_it_replaces() {
        let devices = vec![
            device("desk", &["L", "R", "Aux"], &[], &[]),
            device("amp", &[], &["in1", "in2"], &[(1, "desk", "L")]),
        ];
        // The crosspoint that is already made: the button takes it away.
        assert_eq!(
            action_for(&sel(Some(("amp", 1)), Some(("desk", "L"))), &devices),
            Action::Disconnect {
                from: "desk : L".into()
            }
        );
        // A different source for a receiver that has one: say what goes.
        assert_eq!(
            action_for(&sel(Some(("amp", 1)), Some(("desk", "Aux"))), &devices),
            Action::Connect {
                replaces: Some("desk : L".into())
            }
        );
        assert_eq!(
            action_for(&sel(Some(("amp", 2)), Some(("desk", "R"))), &devices),
            Action::Connect { replaces: None }
        );
        // A receiver alone can still be cleared…
        assert_eq!(
            action_for(&sel(Some(("amp", 1)), None), &devices),
            Action::Disconnect {
                from: "desk : L".into()
            }
        );
        // …and half a route is never actionable.
        assert!(matches!(
            action_for(&sel(Some(("amp", 2)), None), &devices),
            Action::Hint(_)
        ));
        assert!(matches!(
            action_for(&sel(None, Some(("desk", "L"))), &devices),
            Action::Hint(_)
        ));
    }

    #[test]
    fn a_stereo_pair_is_the_next_channel_on_both_sides_or_not_at_all() {
        let desk = device("desk", &["L", "R", "Aux"], &[], &[]);
        let amp = device("amp", &[], &["in1", "in2"], &[]);
        assert_eq!(
            route_ops(&amp, 1, Some(&desk), "L", true),
            vec![(1, "L".to_owned()), (2, "R".to_owned())]
        );
        assert_eq!(
            route_ops(&amp, 1, Some(&desk), "L", false),
            vec![(1, "L".to_owned())]
        );
        // No next receiver: just the one.
        assert_eq!(
            route_ops(&amp, 2, Some(&desk), "L", true),
            vec![(2, "L".to_owned())]
        );
        // No next source: just the one.
        assert_eq!(
            route_ops(&amp, 1, Some(&desk), "Aux", true),
            vec![(1, "Aux".to_owned())]
        );
        // Disconnecting a pair only needs the receivers.
        assert_eq!(
            route_ops(&amp, 1, None, "", true),
            vec![(1, String::new()), (2, String::new())]
        );
    }

    #[test]
    fn arrows_walk_what_is_drawn_and_stop_at_the_edges() {
        let cols = vec![1, 2, 3];
        assert_eq!(nudge(&cols, Some(&2), true), Some(3));
        assert_eq!(nudge(&cols, Some(&3), true), Some(3));
        assert_eq!(nudge(&cols, Some(&1), false), Some(1));
        // From nothing: forward starts at the start, back at the end.
        assert_eq!(nudge(&cols, None, true), Some(1));
        assert_eq!(nudge(&cols, None, false), Some(3));
        // A selection that was filtered away restarts rather than sticking.
        assert_eq!(nudge(&cols, Some(&9), true), Some(1));
        assert_eq!(nudge::<u32>(&[], None, true), None);
    }

    #[test]
    fn zoom_steps_and_fit() {
        assert_eq!(zoom_step(24, true), 30);
        assert_eq!(zoom_step(24, false), 18);
        assert_eq!(zoom_step(10, false), 10);
        assert_eq!(zoom_step(44, true), 44);
        // Off-step values (from a future default) still move sensibly.
        assert_eq!(zoom_step(20, true), 24);
        // 16 columns on a 375px phone: 14px cells (91 + 224 = 315).
        assert_eq!(fit_cell(375.0, 16, 0.0), 14);
        // Collapsed devices take room that doesn't zoom: one step smaller.
        assert_eq!(fit_cell(375.0, 16, 120.0), 10);
        // A handful of columns doesn't balloon past the largest step.
        assert_eq!(fit_cell(1400.0, 4, 0.0), 44);
        // More than can ever fit: the smallest step, and scroll.
        assert_eq!(fit_cell(375.0, 128, 0.0), 10);
    }
}
