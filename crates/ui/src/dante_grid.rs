//! Dante routing grid — the Dante Controller replacement view.
//!
//! Classic subscription matrix: TX channels across the top (grouped by
//! device, vertical labels), RX channels down the left (grouped by
//! device). Click a cell to subscribe that RX channel to that TX
//! channel; click a subscribed cell to clear it. Device groups
//! collapse; a collapsed pair shows how many subscriptions run
//! between the two devices.

use std::collections::HashMap;

use dioxus::prelude::*;
use patchbay_proto::{DanteDevice, DanteSubscription};

use crate::state::{self, DANTE_DEVICES, DANTE_ERROR, DANTE_LOADING};

/// Devices with more channels than this start collapsed.
const AUTO_EXPAND_MAX: usize = 40;

/// `"tx/<dev>"` / `"rx/<dev>"` → expanded override.
static GRID_EXPANDED: GlobalSignal<HashMap<String, bool>> = Signal::global(HashMap::new);

/// Channel-name filter for the grid (matches TX and RX names).
static GRID_FILTER: GlobalSignal<String> = Signal::global(String::new);

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

/// Subscription of `rx_dev`'s channel `rx_ch` if any: (tx_device,
/// tx_channel_name, healthy). Healthy ARC statuses: 1 (inferno-aoip's
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

#[component]
pub fn DanteGrid() -> Element {
    let handle = state::use_patchbay();
    let devices = DANTE_DEVICES.read().clone();
    let loading = *DANTE_LOADING.read();
    let error = DANTE_ERROR.read().clone();

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

    let refresh = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            spawn(async move { state::refresh_dante(&handle).await });
        }
    };

    // Toggle (or move) a subscription. The mirror updates OPTIMISTICALLY
    // so the cell flips instantly; a delayed re-scan verifies against
    // the network (ARC round-trips are seconds).
    let click_cell = {
        let handle = handle.clone();
        // `ops` = (rx_channel, tx_channel_name) pairs — one for a plain
        // click, two for a shift-click stereo pair.
        move |rx_device: String, ops: Vec<(u32, String)>, tx_device: String, is_sub: bool| {
            let handle = handle.clone();
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
                            .dante_subscribe(
                                rx_device.clone(),
                                rx_channel,
                                tx_device.clone(),
                                tx_channel,
                            )
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
                            number: i as u32 + 1,
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

    rsx! {
        div { class: "dante-view",
            div { class: "dante-header",
                button { class: "chip", onclick: refresh,
                    if loading { "scanning…" } else { "rescan network" }
                }
                input {
                    class: "search",
                    placeholder: "filter channels…",
                    value: "{GRID_FILTER}",
                    oninput: move |e| *GRID_FILTER.write() = e.value(),
                }
                span { class: "label",
                    "{devices.len()} device(s) — click a cell to route TX → RX"
                }
                span { class: "label dante-legend",
                    span { class: "cell-demo ok", "✓" } " subscribed  "
                    span { class: "cell-demo warn", "!" } " unresolved"
                }
                if !error.is_empty() {
                    span { class: "dante-error", "{error}" }
                }
            }
            if devices.is_empty() && !loading {
                div { class: "panel-section dim", style: "padding:24px;",
                    "No Dante devices answered mDNS. Studio gear off? Inferno down? "
                    "Check the Services panel in the Patchbay view."
                }
            } else {
                div { class: "dante-scroll",
                    table { class: "dante-grid",
                        thead {
                            // Row 1: TX device headers.
                            tr {
                                th { class: "corner",
                                    span { class: "corner-tx", "TX →" }
                                    br {}
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
                                                class: if dev.unreachable { "dev-h unreachable" } else { "dev-h" },
                                                style: "border-top: 2px solid {accent};",
                                                colspan: "{cols}",
                                                onclick: move |_| toggle_expanded("tx", &name, expanded),
                                                "{dev.name} "
                                                span { class: "dev-count",
                                                    if expanded { "▾" } else { "▸ {dev.tx.len()}ch" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // Row 2: TX channel names (vertical).
                            tr {
                                th { class: "corner corner-slim" }
                                for dev in &tx_devices {
                                    if is_expanded("tx", dev, dev.tx.len()) {
                                        for ch in &dev.tx {
                                            th { class: "tx-ch", key: "{dev.name}:{ch.number}",
                                                title: "{dev.name} TX {ch.number}",
                                                span { class: "tx-ch-label", "{ch.name}" }
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
                                                tr { key: "{rx_dev.name}:{rx_ch.number}",
                                                    th { class: "rx-ch",
                                                        title: "{rx_dev.name} RX {rx_ch.number}",
                                                        "{rx_ch.name}"
                                                    }
                                                    for tx_dev in &tx_devices {
                                                        {
                                                            let tx_expanded = is_expanded("tx", tx_dev, tx_dev.tx.len());
                                                            let sub = sub_of(rx_dev, rx_ch.number);
                                                            if tx_expanded {
                                                                rsx! {
                                                                    for (ti, tx_ch) in tx_dev.tx.iter().enumerate() {
                                                                        {
                                                                            let is_sub = sub.is_some_and(|(d, c, _)|
                                                                                d == tx_dev.name && c == tx_ch.name);
                                                                            let healthy = sub.is_some_and(|(_, _, h)| h);
                                                                            let cls = match (is_sub, healthy) {
                                                                                (true, true) => "cell sub ok",
                                                                                (true, false) => "cell sub warn",
                                                                                _ => "cell",
                                                                            };
                                                                            let click = click_cell.clone();
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
                                                                                    class: "{cls}",
                                                                                    title: "{rx_dev.name}:{rx_ch.name} ← {tx_dev.name}:{tx_ch.name} (shift = stereo pair)",
                                                                                    onclick: move |e: Event<MouseData>| {
                                                                                        let mut ops = vec![(rxn, txc.clone())];
                                                                                        if e.modifiers().shift() && rx_has_next {
                                                                                            if let Some(ntx) = next_tx.clone() {
                                                                                                ops.push((rxn + 1, ntx));
                                                                                            }
                                                                                        }
                                                                                        click(rxd.clone(), ops, txd.clone(), is_sub);
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
                                                                    td { class: if routed { "cell sub ok" } else { "cell collapsed" },
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
    }
}
