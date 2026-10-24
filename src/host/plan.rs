//! Pure planning helpers — graph in, decisions out. Every backend runs
//! these before touching the OS, so the rules are the same everywhere.

use std::collections::BTreeMap;

use crate::host::{Gain, HostError, HostEvent, HostLink, HostNode, HostSnapshot, PortDirection};

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
