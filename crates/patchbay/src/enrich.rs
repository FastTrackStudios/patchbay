//! Out-of-band node-prop enrichment via `pw-dump`.
//!
//! Registry globals expose only a subset of node props (no
//! `node.group`, often no `application.*`), and binding node proxies
//! for the rest wedges the registry event stream (see
//! `engine::handle_node`). So full props come from a debounced
//! `pw-dump` shell-out — best-effort, completely decoupled from our
//! `PipeWire` connection, triggered by node-add bursts.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use parking_lot::RwLock;
use patchbay_proto::{GraphEvent, NodeState, PwNode};
use serde_json::Value;

use crate::store::GraphStore;

/// Run `pw-dump` and hand back its node objects. `None` for every
/// best-effort failure (tool absent, non-zero exit, unparseable JSON) —
/// enrichment is decoration, never a hard dependency.
fn dump_nodes() -> Option<Vec<Value>> {
    let out = std::process::Command::new("pw-dump").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let dump = serde_json::from_slice::<Value>(&out.stdout).ok()?;
    let objects = dump.as_array()?;
    Some(
        objects
            .iter()
            .filter(|o| o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Node"))
            .cloned()
            .collect(),
    )
}

/// The `id` field of a `pw-dump` object, as a `PipeWire` global id.
fn object_id(obj: &Value) -> Option<u32> {
    u32::try_from(obj.get("id").and_then(Value::as_u64)?).ok()
}

/// One `pw-dump` run feeding BOTH passes: live `info.state` deltas and
/// the full-prop merge. These used to be two separate shell-outs on
/// overlapping schedules; the dump is the expensive part, so the
/// pollers share it.
///
/// Emits [`GraphEvent::NodeStateChanged`] for every node whose state
/// moved, and an (idempotent) [`GraphEvent::NodeAdded`] for every node
/// that gained detail. A quiet graph emits nothing.
pub(crate) fn refresh(store: &Arc<RwLock<GraphStore>>, events: &Sender<GraphEvent>, props: bool) {
    let Some(nodes) = dump_nodes() else { return };

    let mut state_changes = Vec::new();
    let mut prop_updates = Vec::new();
    {
        // ONE write lock for the whole dump, not one per node.
        let mut store_w = store.write();
        for obj in &nodes {
            let Some(id) = object_id(obj) else { continue };
            let Some(node) = store_w.nodes.get_mut(&id) else {
                continue;
            };
            let state = obj
                .pointer("/info/state")
                .and_then(Value::as_str)
                .map_or(NodeState::Unknown, NodeState::parse);
            if node.state != state {
                node.state = state;
                state_changes.push((id, state));
            }
            if props
                && let Some(p) = obj.pointer("/info/props")
                && merge_props(node, p)
            {
                prop_updates.push(node.clone());
            }
        }
    }

    for (id, state) in state_changes {
        drop(events.send(GraphEvent::NodeStateChanged { id, state }));
    }
    for node in prop_updates {
        drop(events.send(GraphEvent::NodeAdded(node)));
    }
}

/// Poll every node's live `info.state` (`running`/`idle`/`suspended`).
/// This is the free, no-tap answer to "is anything going through this
/// node" — `PipeWire` exposes no per-port level, but it does expose
/// which nodes are actively cycling.
pub(crate) fn poll_node_states(store: &Arc<RwLock<GraphStore>>, events: &Sender<GraphEvent>) {
    refresh(store, events, false);
}

/// Merge full props from one `pw-dump` run into the store.
pub(crate) fn enrich_nodes(store: &Arc<RwLock<GraphStore>>, events: &Sender<GraphEvent>) {
    refresh(store, events, true);
}

/// Fold a `pw-dump` `info.props` object into `node`; returns whether
/// anything actually changed (so the caller only re-emits real deltas).
fn merge_props(node: &mut PwNode, props: &Value) -> bool {
    let get = |k: &str| {
        props
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut changed = false;
    let mut fill = |field: &mut String, value: String| {
        if !value.is_empty() && *field != value {
            *field = value;
            changed = true;
        }
    };
    fill(&mut node.group, get("node.group"));
    fill(&mut node.app_name, get("application.name"));
    let icon = {
        let dashed = get("application.icon-name");
        if dashed.is_empty() {
            get("application.icon_name")
        } else {
            dashed
        }
    };
    fill(&mut node.icon_name, icon);
    // Registry-subset nodes can even lack name/description.
    fill(&mut node.name, get("node.name"));
    if node.label.is_empty() {
        let label = [get("node.nick"), get("node.description"), get("node.name")]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        fill(&mut node.label, label);
    }
    if get("patchbay.virtual") == "1" && !node.virtual_sink {
        node.virtual_sink = true;
        changed = true;
    }
    changed
}
