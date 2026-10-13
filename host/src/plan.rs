//! Pure planning helpers — graph in, decisions out. Every backend runs
//! these before touching the OS, so the rules are the same everywhere.

use std::collections::BTreeMap;

use crate::{Gain, HostError, HostEvent, HostLink, HostNode, HostSnapshot, PortDirection};

/// Check `link` against `snapshot`: both ports exist, directions are
/// output → input, it isn't a self-loop and the gain is sane.
///
/// # Errors
/// [`HostError::NotFound`] for a missing port, [`HostError::InvalidSpec`]
/// for everything else.
pub fn validate_link(snapshot: &HostSnapshot, link: &HostLink) -> Result<(), HostError> {
    if link.from.direction != PortDirection::Output {
        return Err(HostError::InvalidSpec(format!(
            "link source {:?} is not an output port",
            link.from
        )));
    }
    if link.to.direction != PortDirection::Input {
        return Err(HostError::InvalidSpec(format!(
            "link destination {:?} is not an input port",
            link.to
        )));
    }
    if link.from.node == link.to.node {
        return Err(HostError::InvalidSpec(format!(
            "link loops node `{}` into itself",
            link.from.node
        )));
    }
    for port in [&link.from, &link.to] {
        if !snapshot.has_port(port) {
            return Err(HostError::NotFound(format!(
                "port {}:{} ({:?})",
                port.node, port.channel, port.direction
            )));
        }
    }
    Gain::validate("link", link.gain)?;
    Ok(())
}

/// Events turning node list `old` into `new` (matched by id): removals
/// first, then additions and changes, each in id order.
#[must_use]
pub fn diff_nodes(old: &[HostNode], new: &[HostNode]) -> Vec<HostEvent> {
    let old_by_id: BTreeMap<&str, &HostNode> = old.iter().map(|n| (n.id.as_str(), n)).collect();
    let new_by_id: BTreeMap<&str, &HostNode> = new.iter().map(|n| (n.id.as_str(), n)).collect();

    let removed = old_by_id
        .keys()
        .filter(|id| !new_by_id.contains_key(*id))
        .map(|id| HostEvent::NodeRemoved {
            id: (*id).to_owned(),
        });
    let added_or_changed = new_by_id
        .iter()
        .filter_map(|(id, node)| match old_by_id.get(id) {
            None => Some(HostEvent::NodeAdded((*node).clone())),
            Some(prev) if *prev != *node => Some(HostEvent::NodeChanged((*node).clone())),
            Some(_) => None,
        });
    removed.chain(added_or_changed).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostCapabilities, NodeDirection, NodeKind, PortCounts, PortRef};

    fn node(id: &str, inputs: u32, outputs: u32) -> HostNode {
        let ports = PortCounts { inputs, outputs };
        HostNode {
            id: id.to_owned(),
            name: id.to_owned(),
            kind: NodeKind::HardwareDevice,
            direction: NodeDirection::from_counts(ports),
            ports,
            sample_rate: Some(48_000.0),
            app: None,
            props: BTreeMap::new(),
        }
    }

    fn snap() -> HostSnapshot {
        HostSnapshot {
            backend: "test".to_owned(),
            capabilities: HostCapabilities::none(),
            nodes: vec![node("mic", 0, 2), node("spk", 2, 0), node("iface", 8, 8)],
            links: vec![],
        }
    }

    #[test]
    fn link_validation() {
        let s = snap();
        let ok = HostLink::new(PortRef::output("mic", 1), PortRef::input("spk", 0));
        assert!(validate_link(&s, &ok).is_ok());

        let backwards = HostLink::new(PortRef::input("spk", 0), PortRef::output("mic", 0));
        assert!(matches!(
            validate_link(&s, &backwards),
            Err(HostError::InvalidSpec(_))
        ));

        let out_of_range = HostLink::new(PortRef::output("mic", 2), PortRef::input("spk", 0));
        assert!(matches!(
            validate_link(&s, &out_of_range),
            Err(HostError::NotFound(_))
        ));

        let missing = HostLink::new(PortRef::output("nope", 0), PortRef::input("spk", 0));
        assert!(matches!(
            validate_link(&s, &missing),
            Err(HostError::NotFound(_))
        ));

        let self_loop = HostLink::new(PortRef::output("iface", 0), PortRef::input("iface", 0));
        assert!(matches!(
            validate_link(&s, &self_loop),
            Err(HostError::InvalidSpec(_))
        ));

        let mut loud = ok;
        loud.gain = 100.0;
        assert!(validate_link(&s, &loud).is_err());
    }

    #[test]
    fn node_diff() {
        let old = vec![node("a", 0, 2), node("b", 2, 0)];
        let mut b2 = node("b", 2, 0);
        b2.sample_rate = Some(44_100.0);
        let new = vec![b2.clone(), node("c", 1, 1)];
        assert_eq!(
            diff_nodes(&old, &new),
            vec![
                HostEvent::NodeRemoved { id: "a".to_owned() },
                HostEvent::NodeChanged(b2),
                HostEvent::NodeAdded(node("c", 1, 1)),
            ]
        );
        assert!(diff_nodes(&new, &new).is_empty());
    }
}
