//! The graph canvas: positioned node cards + an SVG cable layer.

use dioxus::prelude::*;
use patchbay_proto::{MediaKind, PortDirection};

use crate::layout::{self, CARD_W, COL_GAP, Filters, MARGIN, ROW_H, column_titles};
use crate::state::{
    self, ARMED_OUTPUTS, DRAG, Drag, DragSource, EXPANDED_GROUPS, GRAPH, HIDE_MONITORS,
    HIDE_UNCONNECTED, MEDIA_TAB, PAN, SEARCH, SELECTED_NODE, ZOOM,
};

#[component]
pub fn GraphCanvas() -> Element {
    // Fetch application icons for nodes we haven't looked up yet
    // (re-runs whenever the graph changes; already-known names are
    // skipped inside).
    let icon_handle = state::use_patchbay();
    use_effect(move || {
        let _ = GRAPH.read().nodes.len();
        state::request_missing_icons(icon_handle.clone());
    });

    let handle = state::use_patchbay();
    let graph = GRAPH.read();
    let aliases = state::ALIASES.read();
    let search = SEARCH.read();
    let expanded = EXPANDED_GROUPS.read();
    let collapsed_cols = *state::COLLAPSED_COLS.read();
    let filters = Filters {
        search: &search,
        tab: *MEDIA_TAB.read(),
        hide_unconnected: *HIDE_UNCONNECTED.read(),
        aliases: &aliases,
        hide_monitors: *HIDE_MONITORS.read(),
        collapsed: collapsed_cols,
    };
    let lay = layout::compute_layout(&graph, &filters, &expanded);

    // Hovered node → the set of nodes on its signal path (upstream
    // sources + downstream destinations, transitively). Cables off the
    // path dim.
    let hovered_node = *state::HOVERED_NODE.read();
    let path_nodes: Option<std::collections::HashSet<u32>> = hovered_node.map(|h| {
        // Two directed closures (not one undirected sweep — that
        // would flood into siblings sharing a sink).
        let mut down = std::collections::HashSet::from([h]);
        loop {
            let before = down.len();
            for l in &graph.links {
                if down.contains(&l.output_node) {
                    down.insert(l.input_node);
                }
            }
            if down.len() == before {
                break;
            }
        }
        let mut up = std::collections::HashSet::from([h]);
        loop {
            let before = up.len();
            for l in &graph.links {
                if up.contains(&l.input_node) {
                    up.insert(l.output_node);
                }
            }
            if up.len() == before {
                break;
            }
        }
        down.extend(up);
        down
    });

    // Ports rendered as condensed stereo pairs draw thin cables.
    let pair_ports: std::collections::HashSet<u32> = lay
        .cards
        .iter()
        .flat_map(|c| c.rows.iter())
        .filter(|r| r.pair)
        .flat_map(|r| r.ports.iter().copied())
        .collect();

    // Cables: only when both anchors are visible. Collapsed-group
    // fan-ins collapse to one drawn path per (from-anchor, to-anchor)
    // pair — the path remembers every link id it stands for, so
    // clicking it disconnects them all.
    struct Cable {
        ids: Vec<u32>,
        d: String,
        color: String,
        active: bool,
        thin: bool,
        out_node: u32,
        in_node: u32,
    }
    let mut by_path: std::collections::HashMap<(u64, u64), usize> =
        std::collections::HashMap::new();
    let mut cables: Vec<Cable> = Vec::new();
    for l in &graph.links {
        let (Some(&(x1, y1)), Some(&(x2, y2))) = (
            lay.anchors.get(&l.output_port),
            lay.anchors.get(&l.input_port),
        ) else {
            continue;
        };
        let key = (
            (x1 as u64) << 32 | (y1 as u64),
            (x2 as u64) << 32 | (y2 as u64),
        );
        if let Some(&i) = by_path.get(&key) {
            cables[i].ids.push(l.id);
            cables[i].active |= l.active;
            continue;
        }
        let port_name = graph
            .ports
            .iter()
            .find(|p| p.id == l.output_port)
            .map(|p| p.name.as_str())
            .unwrap_or("");
        let (node_name, node_label) = graph
            .nodes
            .iter()
            .find(|n| n.id == l.output_node)
            .map(|n| (n.name.as_str(), n.label.as_str()))
            .unwrap_or(("", ""));
        // Cables wear the color of where they come FROM.
        let color = state::port_color(node_name, node_label, port_name);
        let dx = ((x2 - x1) * 0.5).max(40.0);
        by_path.insert(key, cables.len());
        cables.push(Cable {
            ids: vec![l.id],
            d: format!(
                "M {x1} {y1} C {} {y1}, {} {y2}, {x2} {y2}",
                x1 + dx,
                x2 - dx
            ),
            color,
            active: l.active,
            thin: pair_ports.contains(&l.output_port) || pair_ports.contains(&l.input_port),
            out_node: l.output_node,
            in_node: l.input_node,
        });
    }

    // Ghost cable while dragging (only once the pointer has moved).
    let drag_ghost: Option<String> = DRAG.read().as_ref().and_then(|d| {
        let (sx, sy) = d.start;
        let (cx, cy) = d.current;
        if (cx - sx).abs() + (cy - sy).abs() < 6.0 {
            return None;
        }
        let dx = ((cx - sx) * 0.5).max(40.0);
        Some(format!(
            "M {sx} {sy} C {} {sy}, {} {cy}, {cx} {cy}",
            sx + dx,
            cx - dx
        ))
    });

    let world_w = lay.width;
    let world_h = lay.height.max(400.0);

    let zoom = *ZOOM.read();
    let (pan_x, pan_y) = *PAN.read();
    // Middle-drag pan state: last pointer position in client coords.
    let mut drag_last = use_signal(|| None::<(f64, f64)>);

    rsx! {
        div { class: "canvas-scroll",
            // Wheel = zoom toward the cursor; middle-drag = pan.
            onwheel: move |e: Event<WheelData>| {
                e.prevent_default();
                let dy = match e.delta() {
                    dioxus::html::geometry::WheelDelta::Pixels(v) => v.y,
                    dioxus::html::geometry::WheelDelta::Lines(v) => v.y * 40.0,
                    dioxus::html::geometry::WheelDelta::Pages(v) => v.y * 400.0,
                };
                let old = *ZOOM.peek();
                let new = (old * (1.0 - dy * 0.0015)).clamp(0.15, 3.0);
                // Keep the point under the cursor fixed while zooming.
                let cur = e.element_coordinates();
                let (px, py) = *PAN.peek();
                let scale = new / old;
                *PAN.write() = (
                    cur.x - (cur.x - px) * scale,
                    cur.y - (cur.y - py) * scale,
                );
                *ZOOM.write() = new;
            },
            onmousedown: move |e: Event<MouseData>| {
                if e.trigger_button() == Some(dioxus::html::input_data::MouseButton::Auxiliary) {
                    e.prevent_default();
                    let c = e.client_coordinates();
                    drag_last.set(Some((c.x, c.y)));
                }
            },
            onmousemove: move |e: Event<MouseData>| {
                let last = *drag_last.peek();
                if let Some((lx, ly)) = last {
                    let c = e.client_coordinates();
                    let (px, py) = *PAN.peek();
                    *PAN.write() = (px + (c.x - lx), py + (c.y - ly));
                    drag_last.set(Some((c.x, c.y)));
                }
                // Cable drag: track the pointer in world coordinates.
                if DRAG.peek().is_some() {
                    let c = e.element_coordinates();
                    let (px, py) = *PAN.peek();
                    let z = *ZOOM.peek();
                    let world = ((c.x - px) / z, (c.y - py) / z);
                    if let Some(d) = DRAG.write().as_mut() {
                        d.current = world;
                    }
                }
            },
            // Row/header mouseups complete a drag first (bubbling);
            // whatever reaches here just ends the gesture.
            onmouseup: move |_| {
                drag_last.set(None);
                *DRAG.write() = None;
            },
            onmouseleave: move |_| {
                drag_last.set(None);
                *DRAG.write() = None;
            },
            div { class: "canvas-tools",
                button { class: "chip", title: "zoom out",
                    onclick: move |_| { let z = *ZOOM.peek(); *ZOOM.write() = (z / 1.25).max(0.15); },
                    "−"
                }
                span { class: "zoom-pct", "{(zoom * 100.0) as i32}%" }
                button { class: "chip", title: "zoom in",
                    onclick: move |_| { let z = *ZOOM.peek(); *ZOOM.write() = (z * 1.25).min(3.0); },
                    "+"
                }
                button { class: "chip", title: "reset view",
                    onclick: move |_| { *ZOOM.write() = 1.0; *PAN.write() = (0.0, 0.0); },
                    "reset"
                }
            }
            div {
                class: "canvas-world",
                style: "width:{world_w}px;height:{world_h}px;\
                        transform: translate({pan_x}px, {pan_y}px) scale({zoom});\
                        transform-origin: 0 0;",
                for (i, title) in column_titles(*MEDIA_TAB.read()).iter().enumerate() {
                    div {
                        key: "{title}",
                        class: if collapsed_cols[i] { "col-header collapsed" } else { "col-header" },
                        style: "left:{MARGIN + i as f64 * (CARD_W + COL_GAP)}px;width:{CARD_W}px;",
                        title: if collapsed_cols[i] { "expand this column" } else { "collapse this column (headers only)" },
                        onclick: move |_| {
                            let mut cols = *state::COLLAPSED_COLS.peek();
                            cols[i] = !cols[i];
                            *state::COLLAPSED_COLS.write() = cols;
                        },
                        if collapsed_cols[i] { "▸ {title}" } else { "{title}" }
                    }
                }
                svg {
                    class: "cable-layer",
                    width: "{world_w}",
                    height: "{world_h}",
                    view_box: "0 0 {world_w} {world_h}",
                    for cable in cables {
                        {
                            let handle = handle.clone();
                            let ids = cable.ids.clone();
                            let n = ids.len();
                            let dimmed = path_nodes
                                .as_ref()
                                .is_some_and(|set| {
                                    !(set.contains(&cable.out_node)
                                        && set.contains(&cable.in_node))
                                });
                            let opacity = if dimmed {
                                "0.06"
                            } else if cable.active {
                                "0.95"
                            } else {
                                "0.4"
                            };
                            // Active links are being driven (signal is
                            // flowing) → animated marching dash. Paused
                            // links (source node idle/suspended) are
                            // solid and dim: connected, nothing through.
                            let cable_class = if !dimmed && cable.active {
                                "cable flowing"
                            } else {
                                "cable"
                            };
                            let state_note = if cable.active {
                                "signal flowing"
                            } else {
                                "connected — source idle"
                            };
                            rsx! {
                                path {
                                    key: "{cable.ids[0]}",
                                    class: "{cable_class}",
                                    d: "{cable.d}",
                                    fill: "none",
                                    stroke: "{cable.color}",
                                    stroke_width: if cable.thin { "1.2" } else { "2" },
                                    opacity: "{opacity}",
                                    pointer_events: "stroke",
                                    onclick: move |e: Event<MouseData>| {
                                        e.stop_propagation();
                                        state::disconnect_links(handle.clone(), ids.clone());
                                    },
                                    title { "{n} link(s), {state_note} — click to disconnect" }
                                }
                            }
                        }
                    }
                    if let Some(d) = drag_ghost {
                        path {
                            class: "cable-ghost",
                            d: "{d}",
                            fill: "none",
                            stroke: "#7cc4ff",
                            stroke_width: "2",
                            stroke_dasharray: "6 5",
                            opacity: "0.8",
                        }
                    }
                }
                for card in &lay.cards {
                    NodeCard {
                        key: "{card.key}",
                        node_id: card.node.id,
                        node_name: card.node.name.clone(),
                        node_label: state::node_label(&card.node.name, &card.node.label),
                        media_class: card.node.media_class.clone(),
                        node_state: card.node.state,
                        accent: state::node_color(&card.node.name, &card.node.label),
                        icon: state::ICONS
                            .read()
                            .get(&state::icon_candidate(&card.node))
                            .cloned()
                            .unwrap_or_default(),
                        x: card.x,
                        y: card.y,
                        h: card.h,
                        collapsed: card.collapsed,
                        rows: card
                            .rows
                            .iter()
                            .map(|r| RowProps {
                                anchor: match r.direction {
                                    PortDirection::Input => (card.x, card.y + r.y),
                                    PortDirection::Output => (card.x + CARD_W, card.y + r.y),
                                },
                                y: r.y,
                                aliased: aliases
                                    .contains_key(&format!("{}:{}", card.node.name, r.label)),
                                label: if r.pair {
                                    r.label.clone()
                                } else {
                                    // The chip shows the channel, so a
                                    // matching baked-in "28 - " prefix
                                    // in the alias is dropped.
                                    layout::strip_channel_prefix(
                                        &state::port_label(&card.node.name, &r.label),
                                        r.chan.0,
                                    )
                                },
                                chan_label: match r.chan {
                                    (Some(a), Some(b)) => format!("{a}·{b}"),
                                    (Some(a), None) => a.to_string(),
                                    _ => String::new(),
                                },
                                pair_key: r.pair_key.clone(),
                                raw_name: r.label.clone(),
                                monitor: r.monitor,
                                direction: r.direction,
                                dot: if r.group_key.is_some() {
                                    state::node_color(&card.node.name, &card.node.label)
                                } else if r.pair {
                                    state::pair_color(
                                        &card.node.name,
                                        &card.node.label,
                                        &r.label,
                                    )
                                } else {
                                    state::port_color(
                                        &card.node.name,
                                        &card.node.label,
                                        &r.label,
                                    )
                                },
                                pair: r.pair,
                                ports: r.ports.clone(),
                                group_key: r.group_key.clone(),
                                expanded: r
                                    .group_key
                                    .as_ref()
                                    .map(|k| expanded.get(k).copied().unwrap_or(false))
                                    .unwrap_or(false),
                            })
                            .collect::<Vec<_>>(),
                    }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
struct RowProps {
    label: String,
    raw_name: String,
    aliased: bool,
    monitor: bool,
    direction: PortDirection,
    /// Resolved dot/cable color for this row.
    dot: String,
    /// Condensed stereo pair (ports = [L, R]).
    pair: bool,
    /// Dim channel-number chip ("28" or "28·29"); empty = no chip.
    chan_label: String,
    /// Present on pair rows (expand to channels) and their expanded
    /// singles (collapse back).
    pair_key: Option<String>,
    /// World coordinates of this row's cable edge (drag start point).
    anchor: (f64, f64),
    /// Row-center y within the card (independent per side — inputs
    /// stack down the left half, outputs down the right).
    y: f64,
    ports: Vec<u32>,
    group_key: Option<String>,
    expanded: bool,
}

#[component]
fn NodeCard(
    node_id: u32,
    node_name: String,
    node_label: String,
    media_class: String,
    node_state: patchbay_proto::NodeState,
    accent: String,
    icon: String,
    x: f64,
    y: f64,
    h: f64,
    collapsed: bool,
    rows: Vec<RowProps>,
) -> Element {
    let armed = ARMED_OUTPUTS.read().clone();
    let inspected = *SELECTED_NODE.read() == Some(node_id);
    let handle = state::use_patchbay();
    // Duplex cards run inputs and outputs as SIDE-BY-SIDE half-width
    // lanes; single-direction cards keep the full width.
    let both = rows.iter().any(|r| r.direction == PortDirection::Input)
        && rows.iter().any(|r| r.direction == PortDirection::Output);
    let header_drag_name = node_name.clone();
    let header_drop_name = node_name.clone();
    let drop_handle = handle.clone();
    // Live activity dot: PipeWire's node state is the free, no-tap
    // answer to "is anything happening here" (running = actively
    // cycling; idle = connected but not driven; suspended = closed).
    let (state_cls, state_title) = match node_state {
        patchbay_proto::NodeState::Running => ("running", "running — actively processing"),
        patchbay_proto::NodeState::Idle => ("idle", "idle — connected, not being driven"),
        patchbay_proto::NodeState::Suspended => ("suspended", "suspended — closed / no clients"),
        patchbay_proto::NodeState::Unknown => ("unknown", "activity state unknown"),
    };

    rsx! {
        div {
            class: format!(
                "node-card{}{}",
                if inspected { " inspected" } else { "" },
                if both && !collapsed { " duplex" } else { "" },
            ),
            style: "left:{x}px;top:{y}px;width:{CARD_W}px;height:{h}px;",
            onmouseenter: move |_| *state::HOVERED_NODE.write() = Some(node_id),
            onmouseleave: move |_| {
                if *state::HOVERED_NODE.peek() == Some(node_id) {
                    *state::HOVERED_NODE.write() = None;
                }
            },
            div {
                class: "node-header",
                style: "border-left: 3px solid {accent};",
                title: "drag onto another node to bulk-connect 1:1",
                onclick: move |_| {
                    let cur = *SELECTED_NODE.peek();
                    *SELECTED_NODE.write() = if cur == Some(node_id) { None } else { Some(node_id) };
                },
                // Node drag: drop this header on another node's header
                // to bulk 1:1 connect (numeric-suffix pairing).
                onmousedown: move |e: Event<MouseData>| {
                    if e.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) {
                        let start = (x + CARD_W, y + 17.0);
                        *DRAG.write() = Some(Drag {
                            source: DragSource::Node(header_drag_name.clone()),
                            start,
                            current: start,
                        });
                    }
                },
                onmouseup: move |_| {
                    let dragged = DRAG.peek().clone();
                    if let Some(Drag { source: DragSource::Node(from), .. }) = dragged {
                        if from != header_drop_name {
                            state::connect_nodes_bulk(
                                drop_handle.clone(),
                                from,
                                header_drop_name.clone(),
                            );
                        }
                    }
                },
                if !icon.is_empty() {
                    img { class: "node-icon", src: "{icon}" }
                }
                div { class: "node-titles",
                    span { class: "node-title", "{node_label}" }
                    span { class: "node-class", "{media_class}" }
                }
                span {
                    class: "node-state {state_cls}",
                    title: "{state_title}",
                }
            }
            for (i, row) in rows.iter().enumerate().filter(|_| !collapsed) {
                {
                    let row = row.clone();
                    let drag_row = row.clone();
                    let drop_row = row.clone();
                    let drop_handle = handle.clone();
                    let handle = handle.clone();
                    let is_group = row.group_key.is_some();
                    let is_armed = row.direction == PortDirection::Output
                        && !row.ports.is_empty()
                        && !is_group
                        && row.ports == armed;
                    let connectable = row.direction == PortDirection::Input
                        && !armed.is_empty()
                        && !is_group
                        && !row.ports.is_empty();
                    let side = if row.direction == PortDirection::Input { "row-in" } else { "row-out" };
                    let classes = format!(
                        "port-row {side}{}{}{}{}{}{}",
                        if is_group { " group" } else { "" },
                        if is_armed { " armed" } else { "" },
                        if connectable { " connectable" } else { "" },
                        if row.aliased { " aliased" } else { "" },
                        if row.monitor { " mon" } else { "" },
                        if row.pair { " pair" } else { "" },
                    );
                    let tooltip = if is_group {
                        format!(
                            "{} ports — click to {}",
                            row.ports.len().max(1),
                            if row.expanded { "collapse" } else { "expand" }
                        )
                    } else if row.pair {
                        format!("stereo pair — click to {} both channels",
                            if row.direction == PortDirection::Output { "arm" } else { "connect" })
                    } else if row.monitor {
                        format!("{} — monitor tap (a copy of what this sink plays)", row.raw_name)
                    } else if row.aliased {
                        row.raw_name.clone()
                    } else {
                        String::new()
                    };
                    let dot = row.dot.clone();
                    // Pair expand/collapse toggle (tiny, separate from
                    // the row's arm/connect click).
                    let toggle = row.pair_key.clone().map(|pk| {
                        let is_pair = row.pair;
                        rsx! {
                            span {
                                class: "pair-toggle",
                                title: if is_pair { "split into individual channels" } else { "merge back into stereo pair" },
                                onclick: move |e: Event<MouseData>| {
                                    e.stop_propagation();
                                    let cur = EXPANDED_GROUPS
                                        .peek()
                                        .get(&pk)
                                        .copied()
                                        .unwrap_or(false);
                                    EXPANDED_GROUPS.write().insert(pk.clone(), !cur);
                                },
                                if is_pair { "±" } else { "=" }
                            }
                        }
                    });
                    let (lane_left, lane_width) = match (row.direction, both) {
                        (PortDirection::Input, true) => ("0%", "50%"),
                        (PortDirection::Output, true) => ("50%", "50%"),
                        _ => ("0%", "100%"),
                    };
                    let lane_top = row.y - ROW_H / 2.0;
                    rsx! {
                        div {
                            key: "{i}",
                            class: "{classes}",
                            style: "position:absolute;top:{lane_top}px;left:{lane_left};\
                                    width:{lane_width};height:{ROW_H}px;",
                            title: "{tooltip}",
                            // Drag from any port row (single, pair, or
                            // collapsed bank) to a row on the other side.
                            onmousedown: move |e: Event<MouseData>| {
                                if e.trigger_button()
                                    != Some(dioxus::html::input_data::MouseButton::Primary)
                                    || drag_row.ports.is_empty()
                                {
                                    return;
                                }
                                e.stop_propagation();
                                *DRAG.write() = Some(Drag {
                                    source: DragSource::Ports(
                                        drag_row.direction,
                                        drag_row.ports.clone(),
                                    ),
                                    start: drag_row.anchor,
                                    current: drag_row.anchor,
                                });
                            },
                            onmouseup: move |_| {
                                if !drop_row.ports.is_empty() {
                                    state::complete_drag_on_ports(
                                        drop_handle.clone(),
                                        drop_row.direction,
                                        &drop_row.ports,
                                    );
                                }
                            },
                            onclick: move |_| {
                                if let Some(key) = &row.group_key {
                                    let now = !row.expanded;
                                    EXPANDED_GROUPS.write().insert(key.clone(), now);
                                    return;
                                }
                                if row.ports.is_empty() {
                                    return;
                                }
                                match row.direction {
                                    PortDirection::Output => {
                                        let cur = ARMED_OUTPUTS.peek().clone();
                                        *ARMED_OUTPUTS.write() = if cur == row.ports {
                                            Vec::new()
                                        } else {
                                            row.ports.clone()
                                        };
                                    }
                                    PortDirection::Input => {
                                        state::connect_armed(handle.clone(), &row.ports);
                                    }
                                }
                            },
                            if row.direction == PortDirection::Input {
                                span {
                                    class: if row.pair { "port-dot pair-dot" } else { "port-dot" },
                                    style: "background:{dot};",
                                }
                                if !row.chan_label.is_empty() {
                                    span { class: "chan-num", "{row.chan_label}" }
                                }
                            }
                            span { class: "port-name",
                                if is_group {
                                    if row.expanded { "▾ {row.label}" } else { "▸ {row.label}" }
                                } else {
                                    "{row.label}"
                                }
                            }
                            {toggle}
                            if row.direction == PortDirection::Output {
                                if !row.chan_label.is_empty() {
                                    span { class: "chan-num", "{row.chan_label}" }
                                }
                                span {
                                    class: if row.pair { "port-dot pair-dot" } else { "port-dot" },
                                    style: "background:{dot};",
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
