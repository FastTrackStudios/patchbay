//! Pure layout math: filtered graph → positioned cards + port anchors.
//!
//! Nodes land in three columns (outputs-only | duplex | inputs-only),
//! stacked vertically. Consecutive same-prefix numbered ports
//! (`playback_1..128`) collapse into one group row unless expanded —
//! that's how a 128-channel Inferno node stays one small card instead
//! of a 3000-pixel wall.

use std::collections::HashMap;

use patchbay_proto::{GraphSnapshot, MediaKind, PortDirection, PwNode, PwPort};

pub const CARD_W: f64 = 280.0;
pub const ROW_H: f64 = 22.0;
pub const HEADER_H: f64 = 34.0;
pub const CARD_GAP: f64 = 18.0;
pub const COL_GAP: f64 = 200.0;
pub const MARGIN: f64 = 24.0;
/// Vertical space reserved for the column titles.
pub const COL_HEADER_H: f64 = 34.0;
/// Runs shorter than this never group.
pub const GROUP_MIN: usize = 5;

/// Column semantics: 0 = Inputs (capture devices/sources), 1 =
/// Applications (streams + app clients), 2 = Groups (bus sinks —
/// loopbacks like System Audio / Voice Chat / Games and patchbay
/// virtual sinks), 3 = Outputs (real hardware/network sinks). Driven
/// by media.class + bus-ness, not port shape.
pub fn column_of(node: &PwNode) -> usize {
    if node.media_class.contains("/Sink") {
        if node.virtual_sink || node.group.starts_with("loopback") {
            2
        } else {
            3
        }
    } else if node.media_class.contains("/Source") && !node.media_class.starts_with("Stream") {
        0
    } else {
        1
    }
}

/// Column titles per media tab.
pub fn column_titles(tab: MediaKind) -> [&'static str; 4] {
    match tab {
        MediaKind::Midi => ["MIDI Inputs", "Applications", "Groups", "MIDI Outputs"],
        MediaKind::Video => ["Cameras / Sources", "Applications", "Groups", "Outputs"],
        _ => ["Inputs", "Applications", "Groups", "Outputs"],
    }
}

/// Monitor ports (a sink's loop-out taps) get tagged + dimmed so
/// they're never mistaken for the sink's real playback inputs.
pub fn is_monitor(port_name: &str) -> bool {
    port_name.starts_with("monitor_") || port_name == "monitor"
}

/// One rendered row inside a card: a single port, a collapsed group,
/// or a condensed stereo pair.
pub struct PortRow {
    pub label: String,
    pub direction: PortDirection,
    pub kind: MediaKind,
    /// All port ids this row anchors (1 for a plain port row,
    /// `[left, right]` for a stereo pair).
    pub ports: Vec<u32>,
    /// Set when this row is a collapsible group (expansion key).
    pub group_key: Option<String>,
    /// A sink's monitor tap (rendered dimmed + tagged).
    pub monitor: bool,
    /// Condensed L/R stereo pair — one row, two thin cables.
    pub pair: bool,
    /// Toggle key for expanding a pair into its two channels (set on
    /// the pair row AND on its expanded singles, for collapsing back).
    pub pair_key: Option<String>,
    /// Channel number(s) from the raw port name's numeric suffix —
    /// shown as a dim chip so names never need numbers baked in.
    /// `(first, second)`; second is set for pair rows.
    pub chan: (Option<u64>, Option<u64>),
    /// y offset of the row center, relative to the card top.
    pub y: f64,
}

/// "28 - Guitar 1" → "Guitar 1", but ONLY when the leading number is
/// this port's actual channel — the UI shows the channel natively, so
/// a matching baked-in number is redundant; a MISmatched one is
/// information and stays.
pub fn strip_channel_prefix(label: &str, chan: Option<u64>) -> String {
    let Some(chan) = chan else {
        return label.to_string();
    };
    let digits = label.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return label.to_string();
    }
    let (num, rest) = label.split_at(digits);
    if num.parse::<u64>() != Ok(chan) {
        return label.to_string();
    }
    let stripped = rest
        .trim_start_matches([' ', '-', '–', '.', ':', '·'])
        .trim_start();
    if stripped.is_empty() {
        label.to_string()
    } else {
        stripped.to_string()
    }
}

/// Position rows as TWO side-by-side stacks — inputs down the left,
/// outputs down the right, independently — and return the card height
/// (header + the LONGER side, not the sum: half-height duplex cards).
fn assign_row_positions(rows: &mut [PortRow]) -> f64 {
    let (mut n_in, mut n_out) = (0usize, 0usize);
    for row in rows.iter_mut() {
        let idx = match row.direction {
            PortDirection::Input => {
                n_in += 1;
                n_in - 1
            }
            PortDirection::Output => {
                n_out += 1;
                n_out - 1
            }
        };
        row.y = HEADER_H + (idx as f64 + 0.5) * ROW_H;
    }
    HEADER_H + n_in.max(n_out) as f64 * ROW_H + 8.0
}

pub struct CardLayout {
    pub node: PwNode,
    /// Unique render key — a MIDI device split by direction yields two
    /// cards from one node (`"<id>-out"` / `"<id>-in"`).
    pub key: String,
    pub x: f64,
    pub y: f64,
    pub h: f64,
    /// Header-only rendering (its column is collapsed); ports all
    /// anchor at the card edges.
    pub collapsed: bool,
    pub rows: Vec<PortRow>,
}

pub struct GraphLayout {
    pub cards: Vec<CardLayout>,
    /// port id → (x, y) cable anchor in world coordinates.
    pub anchors: HashMap<u32, (f64, f64)>,
    pub width: f64,
    pub height: f64,
}

/// Split `playback_97` → (`playback_`, 97). Ports without a numeric
/// suffix never group.
fn split_numeric(name: &str) -> Option<(&str, u64)> {
    let digits = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 || digits == name.len() {
        return None;
    }
    let (prefix, num) = name.split_at(name.len() - digits);
    num.parse().ok().map(|n| (prefix, n))
}

/// Is this display label the left or right half of a stereo pair?
/// Returns `(base, is_right)`. Matches `_FL`/`_FR` style PipeWire
/// channel suffixes and human `… L` / `… R` / `… Left` / `… Right`
/// aliases — always separated from the base by `_`, space, `-`, `.`
/// or `/` so `Vocal`/`GTR` never false-match.
fn lr_split(label: &str) -> Option<(&str, bool)> {
    for (suffix, right) in [
        ("FL", false),
        ("FR", true),
        ("Left", false),
        ("Right", true),
        ("left", false),
        ("right", true),
        ("L", false),
        ("R", true),
    ] {
        if let Some(base) = label.strip_suffix(suffix) {
            if base.ends_with(['_', ' ', '-', '.', '/']) {
                return Some((base, right));
            }
        }
    }
    None
}

/// Condense adjacent single-port L/R rows into one pair row (drawn as
/// two thin cables) — `playback_FL`+`playback_FR`, or aliased channels
/// like "Room Far L"+"Room Far R" on a 128-channel node. Pairing looks
/// at the DISPLAY label (alias first) with any baked-in channel-number
/// prefix stripped, so "28 - Guitar 1 L"+"29 - Guitar 1 R" still pair.
/// An expanded pair (`expanded["pair/…"]`) stays as its two channels,
/// each carrying the pair key so it can collapse back.
fn merge_lr_pairs(
    rows: Vec<PortRow>,
    node: &PwNode,
    dir_str: &str,
    expanded: &HashMap<String, bool>,
    aliases: &HashMap<String, String>,
) -> Vec<PortRow> {
    let display = |row: &PortRow| -> String {
        let d = aliases
            .get(&format!("{}:{}", node.name, row.label))
            .cloned()
            .unwrap_or_else(|| row.label.clone());
        strip_channel_prefix(&d, row.chan.0)
    };
    let mut out: Vec<PortRow> = Vec::new();
    for mut row in rows {
        let mergeable = row.group_key.is_none() && !row.pair && row.ports.len() == 1;
        if mergeable {
            if let Some(prev) = out.last() {
                if prev.group_key.is_none()
                    && !prev.pair
                    && prev.pair_key.is_none()
                    && prev.ports.len() == 1
                    && prev.monitor == row.monitor
                    && prev.kind == row.kind
                {
                    let (pl, rl) = (display(prev), display(&row));
                    let pair = match (lr_split(&pl), lr_split(&rl)) {
                        (Some((b1, false)), Some((b2, true))) if b1 == b2 => Some(b1.to_string()),
                        _ => None,
                    };
                    if let Some(b1) = pair {
                        let key = format!("pair/{}/{}/{}", node.name, dir_str, prev.label);
                        if expanded.get(&key).copied().unwrap_or(false) {
                            // Expanded: keep both channels, each able
                            // to collapse the pair back.
                            let mut prev = out.pop().expect("just peeked");
                            prev.pair_key = Some(key.clone());
                            row.pair_key = Some(key);
                            out.push(prev);
                            out.push(row);
                            continue;
                        }
                        let base = b1.trim_end_matches(['_', ' ', '-', '.', '/']);
                        let label = if base.is_empty() {
                            "L/R".to_string()
                        } else {
                            format!("{base} L/R")
                        };
                        let prev = out.pop().expect("just peeked");
                        out.push(PortRow {
                            label,
                            direction: row.direction,
                            kind: row.kind,
                            ports: vec![prev.ports[0], row.ports[0]],
                            group_key: None,
                            monitor: row.monitor,
                            pair: true,
                            pair_key: Some(key),
                            chan: (prev.chan.0, row.chan.0),
                            y: 0.0,
                        });
                        continue;
                    }
                }
            }
        }
        out.push(row);
    }
    out
}

/// Group one direction's ports into rows. `expanded` keys are
/// `"node.name/in|out/prefix<first>"`.
///
/// Alias interaction: a handful of named channels ("Guitar" on a
/// 128-port Inferno node) split OUT of their group so they're always
/// visible — the whole point of naming a channel is seeing it. But a
/// bank where ≥ GROUP_MIN channels are named (a full chanmap import)
/// stays grouped, or the card would explode back to 128 rows; the
/// aliases show when the group is expanded.
fn build_rows(
    node: &PwNode,
    ports: &[&PwPort],
    direction: PortDirection,
    expanded: &HashMap<String, bool>,
    aliases: &HashMap<String, String>,
) -> Vec<PortRow> {
    let aliased = |p: &PwPort| aliases.contains_key(&format!("{}:{}", node.name, p.name));
    let dir_str = match direction {
        PortDirection::Input => "in",
        PortDirection::Output => "out",
    };
    let mut rows = Vec::new();

    let single = |rows: &mut Vec<PortRow>, p: &PwPort| {
        rows.push(PortRow {
            label: p.name.clone(),
            direction,
            kind: p.media_kind,
            ports: vec![p.id],
            group_key: None,
            monitor: is_monitor(&p.name),
            pair: false,
            pair_key: None,
            chan: (split_numeric(&p.name).map(|(_, n)| n), None),
            y: 0.0,
        });
    };
    let group = |rows: &mut Vec<PortRow>, run: &[&PwPort]| {
        let (prefix, first) = split_numeric(&run[0].name).unwrap_or(("", 0));
        let last = split_numeric(&run[run.len() - 1].name)
            .map(|(_, n)| n)
            .unwrap_or(0);
        // `first` in the key keeps two segments of the same prefix
        // (split by a named channel) independently expandable.
        let key = format!("{}/{}/{}{}", node.name, dir_str, prefix, first);
        let is_expanded = expanded.get(&key).copied().unwrap_or(false);
        let monitor = is_monitor(&run[0].name);
        if is_expanded {
            rows.push(PortRow {
                label: format!("{}{}–{}", prefix, first, last),
                direction,
                kind: run[0].media_kind,
                ports: Vec::new(),
                group_key: Some(key),
                monitor,
                pair: false,
                pair_key: None,
                chan: (None, None),
                y: 0.0,
            });
            for p in run {
                single(rows, p);
            }
        } else {
            rows.push(PortRow {
                label: format!("{}{}–{}", prefix, first, last),
                direction,
                kind: run[0].media_kind,
                ports: run.iter().map(|p| p.id).collect(),
                group_key: Some(key),
                monitor,
                pair: false,
                pair_key: None,
                chan: (None, None),
                y: 0.0,
            });
        }
    };
    // A slice shorter than GROUP_MIN renders as singles.
    let segment = |rows: &mut Vec<PortRow>, seg: &[&PwPort]| {
        if seg.len() >= GROUP_MIN {
            group(rows, seg);
        } else {
            for p in seg {
                single(rows, p);
            }
        }
    };

    let mut i = 0;
    while i < ports.len() {
        // Extend a run of consecutive same-prefix numbered ports
        // (alias-blind — alias handling comes after).
        let run_end = match split_numeric(&ports[i].name) {
            None => i + 1,
            Some((prefix, mut num)) => {
                let mut j = i + 1;
                while j < ports.len() {
                    match split_numeric(&ports[j].name) {
                        Some((p, n)) if p == prefix && n == num + 1 => {
                            num = n;
                            j += 1;
                        }
                        _ => break,
                    }
                }
                j
            }
        };
        let run = &ports[i..run_end];
        i = run_end;

        if run.len() < GROUP_MIN {
            for p in run {
                single(&mut rows, p);
            }
            continue;
        }
        let aliased_count = run.iter().filter(|p| aliased(p)).count();
        if aliased_count == 0 || aliased_count >= GROUP_MIN {
            group(&mut rows, run);
            continue;
        }
        // A few named channels: split them out, group the gaps.
        let mut seg_start = 0;
        for k in 0..run.len() {
            if aliased(run[k]) {
                segment(&mut rows, &run[seg_start..k]);
                single(&mut rows, run[k]);
                seg_start = k + 1;
            }
        }
        segment(&mut rows, &run[seg_start..]);
    }
    merge_lr_pairs(rows, node, dir_str, expanded, aliases)
}

/// Does a port belong on the given media tab? `Other` (control/dsp
/// oddballs) rides with Audio so nothing is unreachable.
pub fn kind_on_tab(kind: MediaKind, tab: MediaKind) -> bool {
    match tab {
        MediaKind::Audio => matches!(kind, MediaKind::Audio | MediaKind::Other),
        other => kind == other,
    }
}

/// Which nodes/ports survive the current filters.
pub struct Filters<'a> {
    pub search: &'a str,
    /// Active media tab (Audio | Midi | Video).
    pub tab: MediaKind,
    pub hide_unconnected: bool,
    /// The full alias map (`node.name` and `node.name:port.name` keys):
    /// search matches what the user sees, and aliased ports stay out
    /// of collapsed groups.
    pub aliases: &'a HashMap<String, String>,
    /// Drop monitor ports entirely (cables through them disappear).
    pub hide_monitors: bool,
    /// Per-column collapse: collapsed columns render cards as headers
    /// only, with every cable converging on the card edge.
    pub collapsed: [bool; 4],
}

pub fn compute_layout(
    graph: &GraphSnapshot,
    filters: &Filters,
    expanded: &HashMap<String, bool>,
) -> GraphLayout {
    let search = filters.search.to_lowercase();
    let mut columns: [Vec<CardLayout>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];

    for node in &graph.nodes {
        // Prop-less ghosts (a global whose full info never arrived)
        // render as unnamed junk cards — skip until they resolve.
        if node.name.is_empty() && node.label.is_empty() {
            continue;
        }
        if !search.is_empty() {
            let alias = filters
                .aliases
                .get(&node.name)
                .map(String::as_str)
                .unwrap_or("");
            let hay = format!("{} {} {}", node.name, node.label, alias).to_lowercase();
            if !hay.contains(&search) {
                continue;
            }
        }
        let keep = |p: &&PwPort| {
            kind_on_tab(p.media_kind, filters.tab)
                && !(filters.hide_monitors && is_monitor(&p.name))
        };
        let mut ins: Vec<&PwPort> = graph
            .ports
            .iter()
            .filter(|p| p.node_id == node.id && p.direction == PortDirection::Input)
            .filter(keep)
            .collect();
        let mut outs: Vec<&PwPort> = graph
            .ports
            .iter()
            .filter(|p| p.node_id == node.id && p.direction == PortDirection::Output)
            .filter(keep)
            .collect();
        if ins.is_empty() && outs.is_empty() {
            continue; // metadata/factory nodes — nothing to patch
        }
        if filters.hide_unconnected {
            let touched = graph
                .links
                .iter()
                .any(|l| l.output_node == node.id || l.input_node == node.id);
            if !touched {
                continue;
            }
        }
        // Numeric-aware sort so playback_10 follows playback_9.
        let numeric_key = |p: &&PwPort| match split_numeric(&p.name) {
            Some((prefix, n)) => (prefix.to_string(), n),
            None => (p.name.clone(), 0),
        };
        ins.sort_by_key(numeric_key);
        outs.sort_by_key(numeric_key);

        let make_card = |rows: Vec<PortRow>, col: usize, key: String| {
            let collapsed = filters.collapsed[col];
            let mut rows = rows;
            let full_h = assign_row_positions(&mut rows);
            let h = if collapsed { HEADER_H + 6.0 } else { full_h };
            CardLayout {
                node: node.clone(),
                key,
                x: MARGIN + col as f64 * (CARD_W + COL_GAP),
                y: 0.0,
                h,
                collapsed,
                rows,
            }
        };

        // On the MIDI tab, pure-MIDI nodes (hardware bridges, midir
        // ports) are DEVICES: split them by direction — their outputs
        // (keyboards, control surfaces) go left, their inputs (synths,
        // light guides) right. Applications — anything that also
        // carries audio, or a stream — stay whole in the middle,
        // since their MIDI mostly routes within themselves.
        let is_app = graph
            .ports
            .iter()
            .any(|p| p.node_id == node.id && p.media_kind == MediaKind::Audio)
            || node.media_class.starts_with("Stream");
        if filters.tab == MediaKind::Midi && !is_app {
            if !outs.is_empty() {
                let rows = build_rows(
                    node,
                    &outs,
                    PortDirection::Output,
                    expanded,
                    filters.aliases,
                );
                columns[0].push(make_card(rows, 0, format!("{}-out", node.id)));
            }
            if !ins.is_empty() {
                let rows = build_rows(node, &ins, PortDirection::Input, expanded, filters.aliases);
                columns[3].push(make_card(rows, 3, format!("{}-in", node.id)));
            }
        } else {
            let mut rows = build_rows(node, &ins, PortDirection::Input, expanded, filters.aliases);
            rows.extend(build_rows(
                node,
                &outs,
                PortDirection::Output,
                expanded,
                filters.aliases,
            ));
            let col = column_of(node);
            columns[col].push(make_card(rows, col, node.id.to_string()));
        }
    }

    // Loopback passthroughs: a Stream/Output node sharing node.group
    // with an Audio/Sink is that sink's forwarder half — its output
    // rows belong ON the sink's card (Outputs column), not floating in
    // Applications ("System Audio → Inferno TX 97/98").
    if filters.tab == MediaKind::Audio {
        let moved: Vec<(usize, usize)> = {
            let sink_groups: HashMap<&str, usize> = columns[2]
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.node.group.is_empty())
                .map(|(i, c)| (c.node.group.as_str(), i))
                .collect();
            columns[1]
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.node.media_class.starts_with("Stream/Output") && !c.node.group.is_empty()
                })
                .filter_map(|(i, c)| sink_groups.get(c.node.group.as_str()).map(|&s| (i, s)))
                .collect()
        };
        for (stream_idx, sink_idx) in moved.into_iter().rev() {
            let stream = columns[1].remove(stream_idx);
            let sink = &mut columns[2][sink_idx];
            sink.rows.extend(stream.rows);
            let full_h = assign_row_positions(&mut sink.rows);
            if !sink.collapsed {
                sink.h = full_h;
            }
        }
    }

    // Stack each column; stable order by label keeps the layout calm
    // as ids churn.
    let mut cards = Vec::new();
    let mut height: f64 = 0.0;
    for col in &mut columns {
        col.sort_by(|a, b| {
            a.node
                .label
                .to_lowercase()
                .cmp(&b.node.label.to_lowercase())
        });
        let mut y = MARGIN + COL_HEADER_H;
        for mut card in col.drain(..) {
            card.y = y;
            y += card.h + CARD_GAP;
            cards.push(card);
        }
        height = height.max(y);
    }

    // Anchors: every port maps to its row's edge point (collapsed group
    // members all share the group row's anchor). A collapsed-column
    // card anchors ALL its ports at the header edge midpoints, so its
    // cables converge on the card.
    let mut anchors = HashMap::new();
    for card in &cards {
        for row in &card.rows {
            let (x, y) = if card.collapsed {
                let mid = card.y + card.h / 2.0;
                match row.direction {
                    PortDirection::Input => (card.x, mid),
                    PortDirection::Output => (card.x + CARD_W, mid),
                }
            } else {
                match row.direction {
                    PortDirection::Input => (card.x, card.y + row.y),
                    PortDirection::Output => (card.x + CARD_W, card.y + row.y),
                }
            };
            if !card.collapsed && row.pair && row.ports.len() == 2 {
                // Two thin cables want two distinct anchors — L just
                // above the row center, R just below.
                anchors.insert(row.ports[0], (x, y - 3.0));
                anchors.insert(row.ports[1], (x, y + 3.0));
            } else {
                for pid in &row.ports {
                    anchors.insert(*pid, (x, y));
                }
            }
        }
    }

    GraphLayout {
        cards,
        anchors,
        width: MARGIN * 2.0 + 4.0 * CARD_W + 3.0 * COL_GAP,
        height: height + MARGIN,
    }
}

#[cfg(test)]
mod reaper_layout {
    use super::*;

    /// REAPER joins the graph as a JACK client: `node.name = "REAPER"`,
    /// EMPTY `media.class` (kind `Other`), 128 audio `in*` + 128 audio
    /// `out*` ports. It must still lay out as a routable duplex card in
    /// the middle (Applications) column on the Audio tab — the "REAPER
    /// isn't a thing I can route to/from" regression.
    fn reaper_snapshot() -> GraphSnapshot {
        let node = PwNode {
            id: 1,
            name: "REAPER".into(),
            label: "REAPER".into(),
            media_class: String::new(), // JACK clients carry no media.class
            media_kind: MediaKind::Other,
            app_name: String::new(),
            latency: String::new(),
            icon_name: String::new(),
            group: String::new(),
            virtual_sink: false,
            state: patchbay_proto::NodeState::Running,
        };
        let mut ports = Vec::new();
        let mut pid = 100;
        for (dir, prefix) in [(PortDirection::Input, "in"), (PortDirection::Output, "out")] {
            for ch in 1..=128 {
                ports.push(PwPort {
                    id: pid,
                    node_id: 1,
                    name: format!("{prefix}{ch}"),
                    direction: dir,
                    media_kind: MediaKind::Audio,
                });
                pid += 1;
            }
        }
        GraphSnapshot {
            nodes: vec![node],
            ports,
            links: Vec::new(),
        }
    }

    #[test]
    fn reaper_is_a_routable_card() {
        let graph = reaper_snapshot();
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());

        let card = lay
            .cards
            .iter()
            .find(|c| c.node.name == "REAPER")
            .expect("REAPER must appear as a card on the Audio tab");

        // Empty media.class ⇒ Applications (middle) column.
        let middle_x = MARGIN + 1.0 * (CARD_W + COL_GAP);
        assert!(
            (card.x - middle_x).abs() < 1.0,
            "REAPER should land in the Applications column, got x={}",
            card.x
        );

        // Both directions are present, so it's routable to AND from.
        assert!(
            card.rows
                .iter()
                .any(|r| r.direction == PortDirection::Input),
            "REAPER must expose input rows (route audio INTO it)"
        );
        assert!(
            card.rows
                .iter()
                .any(|r| r.direction == PortDirection::Output),
            "REAPER must expose output rows (route audio OUT of it)"
        );

        // Every one of the 256 audio ports is anchored (reachable by a cable).
        assert_eq!(
            graph
                .ports
                .iter()
                .filter(|p| lay.anchors.contains_key(&p.id))
                .count(),
            256,
            "all REAPER audio ports must be cable-anchored"
        );
    }
}

#[cfg(test)]
mod stereo_pairs {
    use super::*;

    fn node() -> PwNode {
        PwNode {
            id: 1,
            name: "dev".into(),
            label: "dev".into(),
            media_class: "Audio/Sink".into(),
            media_kind: MediaKind::Audio,
            app_name: String::new(),
            latency: String::new(),
            icon_name: String::new(),
            group: String::new(),
            virtual_sink: false,
            state: patchbay_proto::NodeState::Running,
        }
    }

    fn port(id: u32, name: &str) -> PwPort {
        PwPort {
            id,
            node_id: 1,
            name: name.into(),
            direction: PortDirection::Input,
            media_kind: MediaKind::Audio,
        }
    }

    #[test]
    fn fl_fr_condense_into_one_pair_row_with_split_anchors() {
        let graph = GraphSnapshot {
            nodes: vec![node()],
            ports: vec![port(10, "playback_FL"), port(11, "playback_FR")],
            links: Vec::new(),
        };
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        let card = &lay.cards[0];
        assert_eq!(card.rows.len(), 1, "FL+FR must condense to one row");
        assert!(card.rows[0].pair);
        assert_eq!(card.rows[0].ports, vec![10, 11]);
        assert_eq!(card.rows[0].label, "playback L/R");
        // Distinct anchors so the pair draws as two thin cables.
        let a = lay.anchors[&10];
        let b = lay.anchors[&11];
        assert!(a.1 < b.1, "L anchors above R");
    }

    #[test]
    fn aliased_lr_channels_pair_up_inside_a_numbered_bank() {
        // Channels 5+6 of a big bank aliased "Room Far L"/"Room Far R":
        // they split out of the group AND condense into a stereo row.
        let ports: Vec<PwPort> = (1..=12).map(|n| port(n, &format!("capture_{n}"))).collect();
        let graph = GraphSnapshot {
            nodes: vec![node()],
            ports,
            links: Vec::new(),
        };
        let aliases: HashMap<String, String> = [
            ("dev:capture_5".to_string(), "Room Far L".to_string()),
            ("dev:capture_6".to_string(), "Room Far R".to_string()),
        ]
        .into();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        let rows = &lay.cards[0].rows;
        let pair = rows
            .iter()
            .find(|r| r.pair)
            .expect("aliased L/R channels must condense");
        assert_eq!(pair.label, "Room Far L/R");
        assert_eq!(pair.ports, vec![5, 6]);
    }

    #[test]
    fn collapsed_column_anchors_converge_on_the_card() {
        let graph = GraphSnapshot {
            nodes: vec![node()], // Audio/Sink → column 2
            ports: vec![port(10, "playback_FL"), port(11, "playback_FR")],
            links: Vec::new(),
        };
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false, false, false, true],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        let card = &lay.cards[0];
        assert!(card.collapsed);
        assert!(card.h < HEADER_H + ROW_H, "header-only height");
        // Both ports share the card-edge midpoint anchor.
        assert_eq!(lay.anchors[&10], lay.anchors[&11]);
        assert_eq!(lay.anchors[&10].0, card.x, "input side edge");
    }

    #[test]
    fn numbered_alias_prefixes_dont_defeat_pairing() {
        // REAPER-style: in28/in29 aliased "28 - Guitar 1 L"/"29 - Guitar 1 R"
        // must condense to "Guitar 1 L/R" with chans (28, 29).
        let ports = vec![port(28, "in28"), port(29, "in29")];
        let graph = GraphSnapshot {
            nodes: vec![node()],
            ports,
            links: Vec::new(),
        };
        let aliases: HashMap<String, String> = [
            ("dev:in28".to_string(), "28 - Guitar 1 L".to_string()),
            ("dev:in29".to_string(), "29 - Guitar 1 R".to_string()),
        ]
        .into();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        let rows = &lay.cards[0].rows;
        assert_eq!(rows.len(), 1, "must condense to one pair row");
        assert_eq!(rows[0].label, "Guitar 1 L/R");
        assert_eq!(rows[0].chan, (Some(28), Some(29)));

        // Expanding the pair splits it back into the two channels.
        let expanded: HashMap<String, bool> = [(rows[0].pair_key.clone().unwrap(), true)].into();
        let lay = compute_layout(&graph, &filters, &expanded);
        let rows = &lay.cards[0].rows;
        assert_eq!(rows.len(), 2, "expanded pair = two channel rows");
        assert!(rows.iter().all(|r| r.pair_key.is_some() && !r.pair));
    }

    #[test]
    fn strip_channel_prefix_only_when_it_matches() {
        assert_eq!(strip_channel_prefix("28 - Guitar 1", Some(28)), "Guitar 1");
        assert_eq!(strip_channel_prefix("28. Guitar", Some(28)), "Guitar");
        assert_eq!(strip_channel_prefix("28-Guitar", Some(28)), "Guitar");
        // Mismatched number is information — keep it.
        assert_eq!(strip_channel_prefix("29 - Guitar", Some(28)), "29 - Guitar");
        // A bare number stays (stripping would leave nothing).
        assert_eq!(strip_channel_prefix("28", Some(28)), "28");
        assert_eq!(strip_channel_prefix("Guitar", Some(28)), "Guitar");
        assert_eq!(strip_channel_prefix("28 - Guitar", None), "28 - Guitar");
    }

    #[test]
    fn loopback_forwarder_merges_into_its_sink_card() {
        // A loopback pair: sink "system_audio" + forwarder stream
        // "system_audio_to_inferno", same node.group. The stream card
        // must fold into the sink's card in the Outputs column.
        let mut sink = node();
        sink.id = 1;
        sink.name = "system_audio".into();
        sink.label = "System Audio".into();
        sink.group = "loopback-1".into();
        let mut stream = node();
        stream.id = 2;
        stream.name = "system_audio_to_inferno".into();
        stream.label = "System Audio → Inferno".into();
        stream.media_class = "Stream/Output/Audio".into();
        stream.group = "loopback-1".into();
        let mut ports = vec![port(10, "playback_FL"), port(11, "playback_FR")];
        for (id, name) in [(20, "output_FL"), (21, "output_FR")] {
            ports.push(PwPort {
                id,
                node_id: 2,
                name: name.into(),
                direction: PortDirection::Output,
                media_kind: MediaKind::Audio,
            });
        }
        let graph = GraphSnapshot {
            nodes: vec![sink, stream],
            ports,
            links: Vec::new(),
        };
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        assert_eq!(lay.cards.len(), 1, "forwarder card folds into the sink");
        let card = &lay.cards[0];
        assert_eq!(card.node.name, "system_audio");
        // Sink inputs AND the forwarder's outputs live on one card.
        assert!(
            card.rows
                .iter()
                .any(|r| r.direction == PortDirection::Input)
        );
        assert!(
            card.rows
                .iter()
                .any(|r| r.direction == PortDirection::Output)
        );
        // Forwarder output ports anchor on the sink card's right edge.
        assert_eq!(lay.anchors[&20].0, card.x + CARD_W);
    }

    #[test]
    fn duplex_cards_stack_sides_independently() {
        // 3 inputs + 1 output: height follows the LONGER side (3 rows),
        // and each side's rows start at the top of the card.
        let mut ports = vec![port(1, "in_a"), port(2, "in_b"), port(3, "in_c")];
        ports.push(PwPort {
            id: 4,
            node_id: 1,
            name: "out_a".into(),
            direction: PortDirection::Output,
            media_kind: MediaKind::Audio,
        });
        let graph = GraphSnapshot {
            nodes: vec![node()],
            ports,
            links: Vec::new(),
        };
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        let card = &lay.cards[0];
        assert_eq!(card.h, HEADER_H + 3.0 * ROW_H + 8.0, "height = longer side");
        let first_in = card
            .rows
            .iter()
            .find(|r| r.direction == PortDirection::Input)
            .unwrap();
        let first_out = card
            .rows
            .iter()
            .find(|r| r.direction == PortDirection::Output)
            .unwrap();
        assert_eq!(first_in.y, first_out.y, "both sides start at the card top");
    }

    #[test]
    fn unrelated_neighbors_never_pair() {
        let graph = GraphSnapshot {
            nodes: vec![node()],
            ports: vec![port(1, "Vocal"), port(2, "GTR"), port(3, "aux_L")],
            links: Vec::new(),
        };
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        assert!(lay.cards[0].rows.iter().all(|r| !r.pair));
        assert_eq!(lay.cards[0].rows.len(), 3);
    }
}

#[cfg(test)]
mod live_probe {
    use super::*;
    use patchbay_proto::PatchbayServiceClient;

    /// Diagnose UI-vs-engine drift against a RUNNING patchbay app:
    /// `cargo test -p patchbay-ui live_layout -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs a running patchbay app on :4046"]
    async fn live_layout() {
        let link = vox_websocket::WsLink::connect("ws://127.0.0.1:4046/vox")
            .await
            .expect("ws connect (is patchbay running?)");
        let client: PatchbayServiceClient = vox_core::initiator_on(link)
            .establish()
            .await
            .expect("establish");
        let graph = client.graph().await.expect("graph");
        println!(
            "graph: {} nodes / {} ports / {} links",
            graph.nodes.len(),
            graph.ports.len(),
            graph.links.len()
        );
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        for col in 0..4 {
            let x = MARGIN + col as f64 * (CARD_W + COL_GAP);
            println!("── column {col} ({})", COLUMN_TITLES_DBG[col]);
            for c in lay.cards.iter().filter(|c| (c.x - x).abs() < 1.0) {
                println!(
                    "   y={:>6.0} h={:>5.0} {} [{}]",
                    c.y, c.h, c.node.label, c.node.name
                );
            }
        }
        assert!(
            lay.cards.iter().any(|c| c.node.name == "REAPER"),
            "REAPER missing from layout"
        );
    }

    const COLUMN_TITLES_DBG: [&str; 4] = ["Inputs", "Applications", "Groups", "Outputs"];

    /// Dump every node's classification inputs against the RUNNING app:
    /// `cargo test -p patchbay-ui live_columns -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs a running patchbay app on :4046"]
    async fn live_columns() {
        let link = vox_websocket::WsLink::connect("ws://127.0.0.1:4046/vox")
            .await
            .expect("ws connect (is patchbay running?)");
        let client: PatchbayServiceClient = vox_core::initiator_on(link)
            .establish()
            .await
            .expect("establish");
        let graph = client.graph().await.expect("graph");
        for n in &graph.nodes {
            let ports = graph.ports.iter().filter(|p| p.node_id == n.id).count();
            if ports == 0 {
                continue;
            }
            println!(
                "col={} label={:35} name={:35} class={:22} group={:18} virt={} app={:?}",
                column_of(n),
                n.label,
                n.name,
                n.media_class,
                n.group,
                n.virtual_sink,
                n.app_name,
            );
        }
    }

    /// Diagnose why pairs/icons aren't visible against the RUNNING app:
    /// `cargo test -p patchbay-ui live_pairs -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs a running patchbay app on :4046"]
    async fn live_pairs() {
        let link = vox_websocket::WsLink::connect("ws://127.0.0.1:4046/vox")
            .await
            .expect("ws connect (is patchbay running?)");
        let client: PatchbayServiceClient = vox_core::initiator_on(link)
            .establish()
            .await
            .expect("establish");
        let graph = client.graph().await.expect("graph");
        let aliases: HashMap<String, String> = client
            .aliases()
            .await
            .expect("aliases")
            .into_iter()
            .map(|a| (a.target, a.alias))
            .collect();
        println!("aliases: {}", aliases.len());
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        for c in &lay.cards {
            let pairs: Vec<&str> = c
                .rows
                .iter()
                .filter(|r| r.pair)
                .map(|r| r.label.as_str())
                .collect();
            let ports = graph
                .ports
                .iter()
                .filter(|p| p.node_id == c.node.id)
                .count();
            println!(
                "card {:30} rows={:3} ports={:3} pairs={:?} icon_name={:?} app={:?}",
                c.node.label,
                c.rows.len(),
                ports,
                pairs,
                c.node.icon_name,
                c.node.app_name
            );
        }
        // What would the icon lookups be, and do they resolve host-side?
        let candidates: Vec<String> = graph
            .nodes
            .iter()
            .map(crate::state::icon_candidate)
            .filter(|s| !s.is_empty())
            .collect();
        println!("icon candidates: {candidates:?}");
        let icons = client.icons(candidates).await.expect("icons");
        println!(
            "resolved icons: {:?}",
            icons
                .iter()
                .map(|i| i.icon_name.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// Ground truth against the LIVE PipeWire graph via an in-process
    /// backend (no separate app needed):
    /// `cargo test -p patchbay-ui inproc_layout -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs live PipeWire"]
    async fn inproc_layout() {
        let backend = patchbay::PatchbayBackend::new();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        use patchbay::proto::PatchbayService as _;
        let graph = backend.graph().await.expect("snapshot");
        println!(
            "graph: {} nodes / {} ports / {} links",
            graph.nodes.len(),
            graph.ports.len(),
            graph.links.len()
        );
        if let Some(r) = graph.nodes.iter().find(|n| n.name == "REAPER") {
            let ins = graph
                .ports
                .iter()
                .filter(|p| p.node_id == r.id && p.direction == PortDirection::Input)
                .count();
            let outs = graph
                .ports
                .iter()
                .filter(|p| p.node_id == r.id && p.direction == PortDirection::Output)
                .count();
            println!(
                "mirror REAPER: id={} class={:?} kind={:?} ins={ins} outs={outs}",
                r.id, r.media_class, r.media_kind
            );
        } else {
            println!("mirror REAPER: ABSENT");
        }
        let aliases = HashMap::new();
        let filters = Filters {
            search: "",
            tab: MediaKind::Audio,
            hide_unconnected: false,
            aliases: &aliases,
            hide_monitors: false,
            collapsed: [false; 4],
        };
        let lay = compute_layout(&graph, &filters, &HashMap::new());
        match lay.cards.iter().find(|c| c.node.name == "REAPER") {
            Some(c) => {
                let col = ((c.x - MARGIN) / (CARD_W + COL_GAP)).round() as usize;
                println!(
                    "layout REAPER: column {col} ({}), {} rows, x={}",
                    COLUMN_TITLES_DBG.get(col).unwrap_or(&"?"),
                    c.rows.len(),
                    c.x
                );
            }
            None => println!("layout REAPER: NOT IN LAYOUT (nodes present but filtered out)"),
        }
    }
}

#[cfg(test)]
mod category_colors {
    /// Print which real-world channel names miss categorization:
    /// `cargo test -p patchbay-ui category_coverage -- --nocapture`
    #[test]
    fn category_coverage() {
        let names = [
            "Kick In",
            "Kick Out",
            "Snare Top",
            "Snare Bottom",
            "Tom 1",
            "Hi Hat",
            "Ride",
            "OH L",
            "Room L",
            "Room Far L",
            "Electronic Kit L",
            "Drum Pad L",
            "Bass DI",
            "Bass Amp",
            "Bass Synth L",
            "Guitar 1 L",
            "Guitar 1 DI",
            "Keys 1 L",
            "Lead Mic L",
            "Engineer Vocal",
            "Drummer Mic",
            "Wireless Mic 1",
            "Click",
            "Guide",
            "Talkback L",
            "Speakers L",
            "Engineer Mix L",
            "Drums Mix L",
            "Bass Mix L",
            "Guitar 1 Mix L",
            "Keys 1 Mix L",
            "Broadcast Mix L",
            "Voice Chat L",
            "DAW L",
            "Spare 1",
            "Acoustic 1",
            "Synth Lead",
            "Pad",
            "Vocal 1 Mix L",
        ];
        let mut misses = Vec::new();
        for n in names {
            match crate::state::category_color(n) {
                Some(c) => println!("{n:24} → {c}"),
                None => misses.push(n),
            }
        }
        assert!(misses.is_empty(), "uncategorized channel names: {misses:?}");

        // Spot-check the scheme: drums red-family, bass yellow,
        // guitars sky, acoustic cyan, keys green, synths violet.
        let c = |n: &str| crate::state::category_color(n).unwrap();
        assert_eq!(c("Guitar 1 L"), c("Guitar 2 Mix R"));
        assert_eq!(c("OH L"), c("Ride"), "overheads and cymbals share amber");
        assert_eq!(c("Bass Synth L"), c("Bass DI"), "bass family yellow");
        assert_eq!(c("Engineer Vocal"), c("Vocal 1 Mix L"));
    }
}
