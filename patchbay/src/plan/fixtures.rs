//! Graph builders for planner tests.
//!
//! A planner takes a `GraphStore` and returns commands, so a test needs
//! to build a graph by hand. These keep that to one line per node/port
//! instead of a screenful of struct literals.

use patchbay_proto::{MediaKind, NodeState, PortDirection, PwLink, PwNode, PwPort};

use crate::store::GraphStore;

/// An audio node with `name` as both `node.name` and label.
pub(crate) fn node(id: u32, name: &str) -> PwNode {
    PwNode {
        id,
        name: name.to_owned(),
        label: name.to_owned(),
        media_class: String::new(),
        media_kind: MediaKind::Audio,
        app_name: String::new(),
        latency: String::new(),
        icon_name: String::new(),
        group: String::new(),
        virtual_sink: false,
        state: NodeState::Running,
    }
}

/// An audio port on `node_id`.
pub(crate) fn port(id: u32, node_id: u32, name: &str, direction: PortDirection) -> PwPort {
    PwPort {
        id,
        node_id,
        name: name.to_owned(),
        direction,
        media_kind: MediaKind::Audio,
    }
}

/// An active link between two ports. Node ids are resolved by the
/// builder, so callers only give port ids.
pub(crate) fn link(id: u32, output_port: u32, input_port: u32) -> PwLink {
    PwLink {
        id,
        output_node: 0,
        output_port,
        input_node: 0,
        input_port,
        active: true,
    }
}

/// Assemble a store, filling in each link's node ids from its ports so
/// tests never have to state them twice.
pub(crate) fn store_with(nodes: &[PwNode], ports: &[PwPort], links: &[PwLink]) -> GraphStore {
    let mut store = GraphStore::default();
    for n in nodes {
        store.nodes.insert(n.id, n.clone());
    }
    for p in ports {
        store.ports.insert(p.id, p.clone());
    }
    for l in links {
        let mut l = l.clone();
        l.output_node = store.ports.get(&l.output_port).map_or(0, |p| p.node_id);
        l.input_node = store.ports.get(&l.input_port).map_or(0, |p| p.node_id);
        store.links.insert(l.id, l);
    }
    store
}

/// Insert a link into an EXISTING store, resolving its node ids from
/// that store's ports.
///
/// Prefer this over building a link separately: a link whose node ids
/// don't resolve is (correctly) skipped by the planners, so a fixture
/// that forgets them produces a silently empty plan rather than a
/// visible error.
pub(crate) fn insert_link(store: &mut GraphStore, id: u32, output_port: u32, input_port: u32) {
    let output_node = store
        .ports
        .get(&output_port)
        .unwrap_or_else(|| panic!("fixture: output port {output_port} not in store"))
        .node_id;
    let input_node = store
        .ports
        .get(&input_port)
        .unwrap_or_else(|| panic!("fixture: input port {input_port} not in store"))
        .node_id;
    store.links.insert(
        id,
        PwLink {
            id,
            output_node,
            output_port,
            input_node,
            input_port,
            active: true,
        },
    );
}

/// A `target → alias` map from `(target, alias)` pairs.
pub(crate) fn aliases(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(t, a)| ((*t).to_owned(), (*a).to_owned()))
        .collect()
}
