//! Out-of-band node-prop enrichment via `pw-dump`.
//!
//! Registry globals expose only a subset of node props (no
//! `node.group`, often no `application.*`), and binding node proxies
//! for the rest wedges the registry event stream (see
//! `engine::handle_node`). So full props come from a debounced
//! `pw-dump` shell-out — best-effort, completely decoupled from our
//! PipeWire connection, triggered by node-add bursts.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use parking_lot::RwLock;
use patchbay_proto::{GraphEvent, NodeState};

use crate::store::GraphStore;

/// Poll every node's live `info.state` (`running`/`idle`/`suspended`)
/// from one `pw-dump` run and emit [`GraphEvent::NodeStateChanged`] for
/// each node whose state moved. This is the free, no-tap answer to "is
/// anything going through this node" — PipeWire exposes no per-port
/// level, but it does expose which nodes are actively cycling. Runs on
/// a modest periodic cadence (see the service's poller thread); a
/// quiet graph emits nothing.
pub(crate) fn poll_node_states(store: &Arc<RwLock<GraphStore>>, events: &Sender<GraphEvent>) {
    let Ok(out) = std::process::Command::new("pw-dump").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Ok(dump) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return;
    };
    let Some(objects) = dump.as_array() else {
        return;
    };

    let mut changes = Vec::new();
    {
        let mut store_w = store.write();
        for obj in objects {
            if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
                continue;
            }
            let Some(id) = obj.get("id").and_then(|i| i.as_u64()).map(|i| i as u32) else {
                continue;
            };
            let Some(node) = store_w.nodes.get_mut(&id) else {
                continue;
            };
            let state = obj
                .pointer("/info/state")
                .and_then(|v| v.as_str())
                .map(NodeState::parse)
                .unwrap_or(NodeState::Unknown);
            if node.state != state {
                node.state = state;
                changes.push((id, state));
            }
        }
    }

    for (id, state) in changes {
        let _ = events.send(GraphEvent::NodeStateChanged { id, state });
    }
}

/// Merge full props from one `pw-dump` run into the store; emits an
/// (idempotent) `NodeAdded` update for every node that gained detail.
pub(crate) fn enrich_nodes(store: &Arc<RwLock<GraphStore>>, events: &Sender<GraphEvent>) {
    let Ok(out) = std::process::Command::new("pw-dump").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Ok(dump) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return;
    };
    let Some(objects) = dump.as_array() else {
        return;
    };

    let mut updates = Vec::new();
    for obj in objects {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(id) = obj.get("id").and_then(|i| i.as_u64()).map(|i| i as u32) else {
            continue;
        };
        let Some(props) = obj.pointer("/info/props") else {
            continue;
        };
        let get = |k: &str| {
            props
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        let mut store_w = store.write();
        let Some(node) = store_w.nodes.get_mut(&id) else {
            continue;
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
        fill(
            &mut node.icon_name,
            if get("application.icon-name").is_empty() {
                get("application.icon_name")
            } else {
                get("application.icon-name")
            },
        );
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
        if changed {
            updates.push(node.clone());
        }
    }

    for node in updates {
        let _ = events.send(GraphEvent::NodeAdded(node));
    }
}
