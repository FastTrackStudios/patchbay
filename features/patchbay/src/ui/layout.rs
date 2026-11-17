//! Pure layout math: filtered graph → positioned cards + port anchors.
//!
//! Nodes land in three columns (outputs-only | duplex | inputs-only),
//! stacked vertically. Consecutive same-prefix numbered ports
//! (`playback_1..128`) collapse into one group row unless expanded —
//! that's how a 128-channel Inferno node stays one small card instead
//! of a 3000-pixel wall.

use std::collections::HashMap;

use crate::proto::{GraphSnapshot, MediaKind, PortDirection, PwNode, PwPort};

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

/// Column indices — see [`column_of`].
pub const COL_INPUTS: usize = 0;
pub const COL_APPS: usize = 1;
pub const COL_GROUPS: usize = 2;
pub const COL_OUTPUTS: usize = 3;

/// Column semantics: 0 = Inputs (capture devices/sources), 1 =
/// Applications (streams + app clients), 2 = Groups (bus sinks —
/// loopbacks like System Audio / Voice Chat / Games and patchbay
/// virtual sinks), 3 = Outputs (real hardware/network sinks). Driven
/// by media.class + bus-ness, not port shape.
pub fn column_of(node: &PwNode) -> usize {
    if node.media_class.contains("/Sink") {
        if node.virtual_sink || node.group.starts_with("loopback") {
            COL_GROUPS
        } else {
            COL_OUTPUTS
        }
    } else if node.media_class.contains("/Source") && !node.media_class.starts_with("Stream") {
        COL_INPUTS
    } else {
        COL_APPS
    }
}

/// Column titles per media tab.
pub const fn column_titles(tab: MediaKind) -> [&'static str; 4] {
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
/// a matching baked-in number is redundant; a `MISmatched` one is
/// information and stays.
pub fn strip_channel_prefix(label: &str, chan: Option<u64>) -> String {
    let Some(chan) = chan else {
        return label.to_string();
    };
    let digits = label.chars().take_while(char::is_ascii_digit).count();
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
        // Post-increment: the row takes the CURRENT count as its index.
        let idx = match row.direction {
            PortDirection::Input => {
                let i = n_in;
                n_in = n_in.saturating_add(1);
                i
            }
            PortDirection::Output => {
                let i = n_out;
                n_out = n_out.saturating_add(1);
                i
            }
        };
        row.y = HEADER_H + (row_offset(idx) + 0.5) * ROW_H;
    }
    HEADER_H + row_offset(n_in.max(n_out)) * ROW_H + 8.0
}

/// Left edge of column `i`, relative to the canvas margin.
#[must_use]
pub fn column_x(i: usize) -> f64 {
    row_offset(i) * (CARD_W + COL_GAP)
}

/// A row count as a pixel multiplier. Cards never approach 2^53 rows,
/// so the saturating conversion is a formality that keeps the cast
/// total.
fn row_offset(n: usize) -> f64 {
    u32::try_from(n).map_or_else(|_| f64::from(u32::MAX), f64::from)
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
/// suffix never group. The canonical definition lives in the wire crate
/// so the engine and every UI agree on channel identity.
use crate::proto::split_port_number as split_numeric;

/// Is this display label the left or right half of a stereo pair?
/// Returns `(base, is_right)`. Matches `_FL`/`_FR` style `PipeWire`
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
        // Pop the candidate up front rather than peeking and popping
        // later: the "peek, then pop and unwrap" shape was two
        // expect()s that only held because of the enclosing `if let`.
        // If this row can't pair, `prev` goes straight back on.
        let mergeable = row.group_key.is_none() && !row.pair && row.ports.len() == 1;
        let Some(prev) = (if mergeable { out.pop() } else { None }) else {
            out.push(row);
            continue;
        };
        let pairable = prev.group_key.is_none()
            && !prev.pair
            && prev.pair_key.is_none()
            && prev.ports.len() == 1
            && prev.monitor == row.monitor
            && prev.kind == row.kind;
        let base = if pairable {
            let (prev_label, row_label) = (display(&prev), display(&row));
            match (lr_split(&prev_label), lr_split(&row_label)) {
                (Some((b1, false)), Some((b2, true))) if b1 == b2 => Some(b1.to_owned()),
                _ => None,
            }
        } else {
            None
        };
        let (Some(base), Some(&prev_port), Some(&row_port)) =
            (base, prev.ports.first(), row.ports.first())
        else {
            out.push(prev);
            out.push(row);
            continue;
        };

        let key = format!("pair/{}/{}/{}", node.name, dir_str, prev.label);
        if expanded.get(&key).copied().unwrap_or(false) {
            // Expanded: keep both channels, each able to collapse the
            // pair back.
            let mut prev = prev;
            prev.pair_key = Some(key.clone());
            row.pair_key = Some(key);
            out.push(prev);
            out.push(row);
            continue;
        }
        let trimmed = base.trim_end_matches(['_', ' ', '-', '.', '/']);
        let label = if trimmed.is_empty() {
            "L/R".to_owned()
        } else {
            format!("{trimmed} L/R")
        };
        out.push(PortRow {
            label,
            direction: row.direction,
            kind: row.kind,
            ports: vec![prev_port, row_port],
            group_key: None,
            monitor: row.monitor,
            pair: true,
            pair_key: Some(key),
            chan: (prev.chan.0, row.chan.0),
            y: 0.0,
        });
    }
    out
}

/// Group one direction's ports into rows. `expanded` keys are
/// `"node.name/in|out/prefix<first>"`.
///
/// Alias interaction: a handful of named channels ("Guitar" on a
/// 128-port Inferno node) split OUT of their group so they're always
/// visible — the whole point of naming a channel is seeing it. But a
/// bank where ≥ `GROUP_MIN` channels are named (a full chanmap import)
/// stays grouped, or the card would explode back to 128 rows; the
/// aliases show when the group is expanded.
#[allow(clippy::too_many_lines)]
fn build_rows(
    node: &PwNode,
    ports: &[&PwPort],
    direction: PortDirection,
    expanded: &HashMap<String, bool>,
    aliases: &HashMap<String, String>,
) -> Vec<PortRow> {
    let is_aliased = |p: &PwPort| aliases.contains_key(&format!("{}:{}", node.name, p.name));
    let dir_str = match direction {
        PortDirection::Input => "in",
        PortDirection::Output => "out",
    };
    let mut out_rows = Vec::new();

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
        // Callers only ever pass a non-empty run; `first()`/`last()`
        // say so without indexing.
        let (Some(head), Some(tail)) = (run.first(), run.last()) else {
            return;
        };
        let (prefix, first) = split_numeric(&head.name).unwrap_or(("", 0));
        let last = split_numeric(&tail.name).map_or(0, |(_, n)| n);
        // `first` in the key keeps two segments of the same prefix
        // (split by a named channel) independently expandable.
        let key = format!("{}/{}/{}{}", node.name, dir_str, prefix, first);
        let is_expanded = expanded.get(&key).copied().unwrap_or(false);
        let label = format!("{prefix}{first}–{last}");
        let monitor = is_monitor(&head.name);
        rows.push(PortRow {
            label,
            direction,
            kind: head.media_kind,
            // Expanded: the group row is a header, its channels follow
            // as singles. Collapsed: the row stands for every port.
            ports: if is_expanded {
                Vec::new()
            } else {
                run.iter().map(|p| p.id).collect()
            },
            group_key: Some(key),
            monitor,
            pair: false,
            pair_key: None,
            chan: (None, None),
            y: 0.0,
        });
        if is_expanded {
            for p in run {
                single(rows, p);
            }
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
    while let Some(head) = ports.get(i) {
        // Extend a run of consecutive same-prefix numbered ports
        // (alias-blind — alias handling comes after).
        let run_end = match split_numeric(&head.name) {
            None => i.saturating_add(1),
            Some((prefix, mut num)) => {
                let mut j = i.saturating_add(1);
                while let Some(next) = ports.get(j) {
                    match split_numeric(&next.name) {
                        Some((p, n)) if p == prefix && n == num.saturating_add(1) => {
                            num = n;
                            j = j.saturating_add(1);
                        }
                        _ => break,
                    }
                }
                j
            }
        };
        let Some(run) = ports.get(i..run_end) else {
            break;
        };
        i = run_end;

        if run.len() < GROUP_MIN {
            for p in run {
                single(&mut out_rows, p);
            }
            continue;
        }
        let aliased_count = run.iter().filter(|p| is_aliased(p)).count();
        if aliased_count == 0 || aliased_count >= GROUP_MIN {
            group(&mut out_rows, run);
            continue;
        }
        // A few named channels: split them out, group the gaps.
        let mut seg_start = 0;
        for (k, port) in run.iter().enumerate() {
            if is_aliased(port) {
                if let Some(seg) = run.get(seg_start..k) {
                    segment(&mut out_rows, seg);
                }
                single(&mut out_rows, port);
                seg_start = k.saturating_add(1);
            }
        }
        if let Some(seg) = run.get(seg_start..) {
            segment(&mut out_rows, seg);
        }
    }
    merge_lr_pairs(out_rows, node, dir_str, expanded, aliases)
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

// The remaining body is one loop over nodes; the separable phases are
// already extracted (`fold_loopback_forwarders`, `stack_columns`,
// `port_anchors`).
#[allow(clippy::too_many_lines)]
pub fn compute_layout(
    graph: &GraphSnapshot,
    filters: &Filters,
    expanded: &HashMap<String, bool>,
) -> GraphLayout {
    let search = filters.search.to_lowercase();
    let mut columns: [Vec<CardLayout>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];

    // Index ports by owning node ONCE. This used to be three full scans
    // of `graph.ports` per node (ins, outs, and the MIDI is_app probe),
    // i.e. O(nodes x ports) on every render — ~600k iterations on a
    // 100-node / 2000-port graph, which is exactly the shape a PipeWire
    // reconnect produces.
    let mut ports_by_node: HashMap<u32, Vec<&PwPort>> = HashMap::new();
    for port in &graph.ports {
        ports_by_node.entry(port.node_id).or_default().push(port);
    }
    // Same story for `hide_unconnected`, which scanned every link per node.
    let linked_nodes: std::collections::HashSet<u32> = graph
        .links
        .iter()
        .flat_map(|l| [l.output_node, l.input_node])
        .collect();
    let no_ports: Vec<&PwPort> = Vec::new();

    for node in &graph.nodes {
        // Prop-less ghosts (a global whose full info never arrived)
        // render as unnamed junk cards — skip until they resolve.
        if node.name.is_empty() && node.label.is_empty() {
            continue;
        }
        if !search.is_empty() {
            let alias = filters.aliases.get(&node.name).map_or("", String::as_str);
            let hay = format!("{} {} {}", node.name, node.label, alias).to_lowercase();
            if !hay.contains(&search) {
                continue;
            }
        }
        let node_ports = ports_by_node.get(&node.id).unwrap_or(&no_ports);
        let keep = |p: &&&PwPort| {
            kind_on_tab(p.media_kind, filters.tab)
                && !(filters.hide_monitors && is_monitor(&p.name))
        };
        let mut ins: Vec<&PwPort> = node_ports
            .iter()
            .filter(|p| p.direction == PortDirection::Input)
            .filter(keep)
            .copied()
            .collect();
        let mut outs: Vec<&PwPort> = node_ports
            .iter()
            .filter(|p| p.direction == PortDirection::Output)
            .filter(keep)
            .copied()
            .collect();
        if ins.is_empty() && outs.is_empty() {
            continue; // metadata/factory nodes — nothing to patch
        }
        if filters.hide_unconnected && !linked_nodes.contains(&node.id) {
            continue;
        }
        // Numeric-aware sort so playback_10 follows playback_9.
        // `sort_by_cached_key`: the key allocates, and `sort_by_key`
        // would rebuild it O(n log n) times per card.
        let numeric_key = |p: &&PwPort| match split_numeric(&p.name) {
            Some((prefix, n)) => (prefix.to_owned(), n),
            None => (p.name.clone(), 0),
        };
        ins.sort_by_cached_key(numeric_key);
        outs.sort_by_cached_key(numeric_key);

        let make_card = |rows: Vec<PortRow>, col: usize, key: String| {
            let collapsed = filters.collapsed.get(col).copied().unwrap_or(false);
            let mut rows = rows;
            let full_h = assign_row_positions(&mut rows);
            let h = if collapsed { HEADER_H + 6.0 } else { full_h };
            CardLayout {
                node: node.clone(),
                key,
                x: MARGIN + row_offset(col) * (CARD_W + COL_GAP),
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
        let is_app = node_ports.iter().any(|p| p.media_kind == MediaKind::Audio)
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
                if let Some(column) = columns.get_mut(COL_INPUTS) {
                    column.push(make_card(rows, COL_INPUTS, format!("{}-out", node.id)));
                }
            }
            if !ins.is_empty() {
                let rows = build_rows(node, &ins, PortDirection::Input, expanded, filters.aliases);
                if let Some(column) = columns.get_mut(COL_OUTPUTS) {
                    column.push(make_card(rows, COL_OUTPUTS, format!("{}-in", node.id)));
                }
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
            if let Some(column) = columns.get_mut(col) {
                column.push(make_card(rows, col, node.id.to_string()));
            }
        }
    }

    // Loopback passthroughs: a Stream/Output node sharing node.group
    // with an Audio/Sink is that sink's forwarder half — its output
    // rows belong ON the sink's card (Outputs column), not floating in
    // Applications ("System Audio → Inferno TX 97/98").
    if filters.tab == MediaKind::Audio {
        fold_loopback_forwarders(&mut columns);
    }

    let (cards, height) = stack_columns(&mut columns);

    let anchors = port_anchors(&cards);

    GraphLayout {
        cards,
        anchors,
        width: MARGIN * 2.0 + 4.0 * CARD_W + 3.0 * COL_GAP,
        height: height + MARGIN,
    }
}

/// Loopback passthroughs: a `Stream/Output` node sharing `node.group`
/// with an `Audio/Sink` is that sink's forwarder half — its output rows
/// belong ON the sink's card (Groups column), not floating in
/// Applications ("System Audio → Inferno TX 97/98").
fn fold_loopback_forwarders(columns: &mut [Vec<CardLayout>; 4]) {
    let moved: Vec<(usize, usize)> = {
        let sink_groups: HashMap<&str, usize> = columns
            .get(COL_GROUPS)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, c)| !c.node.group.is_empty())
            .map(|(i, c)| (c.node.group.as_str(), i))
            .collect();
        columns
            .get(COL_APPS)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, c)| {
                c.node.media_class.starts_with("Stream/Output") && !c.node.group.is_empty()
            })
            .filter_map(|(i, c)| sink_groups.get(c.node.group.as_str()).map(|&s| (i, s)))
            .collect()
    };
    // Reverse so earlier removals don't shift later indices.
    for (stream_idx, sink_idx) in moved.into_iter().rev() {
        let Some(stream) = columns
            .get_mut(COL_APPS)
            .filter(|c| stream_idx < c.len())
            .map(|c| c.remove(stream_idx))
        else {
            continue;
        };
        let Some(sink) = columns
            .get_mut(COL_GROUPS)
            .and_then(|c| c.get_mut(sink_idx))
        else {
            continue;
        };
        sink.rows.extend(stream.rows);
        let full_h = assign_row_positions(&mut sink.rows);
        if !sink.collapsed {
            sink.h = full_h;
        }
    }
}

/// Stack each column top-to-bottom, returning the flattened cards and
/// the tallest column's height. Stable order by label keeps the layout
/// calm as `PipeWire` ids churn.
fn stack_columns(columns: &mut [Vec<CardLayout>; 4]) -> (Vec<CardLayout>, f64) {
    let mut cards = Vec::new();
    let mut height: f64 = 0.0;
    for col in columns.iter_mut() {
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
    (cards, height)
}

/// Cable attachment point for every port: its row's edge (collapsed
/// group members all share the group row's anchor). A collapsed-column
/// card anchors ALL its ports at the header edge midpoints, so its
/// cables converge on the card.
fn port_anchors(cards: &[CardLayout]) -> HashMap<u32, (f64, f64)> {
    let mut anchors = HashMap::new();
    for card in cards {
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
            if let (false, true, [left, right]) = (card.collapsed, row.pair, row.ports.as_slice()) {
                // Two thin cables want two distinct anchors — L just
                // above the row center, R just below.
                anchors.insert(*left, (x, y - 3.0));
                anchors.insert(*right, (x, y + 3.0));
            } else {
                for pid in &row.ports {
                    anchors.insert(*pid, (x, y));
                }
            }
        }
    }
    anchors
}
