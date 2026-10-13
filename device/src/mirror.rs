//! Keeping a [`DeviceSnapshot`] in step with a device without re-reading
//! it: apply the adapter's own [`DeviceEvent`]s to a cached snapshot,
//! and turn a periodic fresh read into events by diffing it against the
//! cache. Adapters use these to serve [`crate::DeviceAdapter::current`]
//! from memory.

use std::collections::HashMap;

use crate::{ChannelRef, DeviceEvent, DeviceSnapshot, ParamValue};

/// What applying an event did to the mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorUpdate {
    /// The mirror took the change (or already had it).
    Applied,
    /// The event names a param or output the mirror doesn't have.
    Unknown,
    /// The device state changed wholesale: the mirror is stale and must
    /// be replaced by a fresh read.
    Stale,
    /// Nothing to apply (online/offline).
    Ignored,
}

/// Apply one event to a cached snapshot.
pub fn apply_event(snap: &mut DeviceSnapshot, ev: &DeviceEvent) -> MirrorUpdate {
    match ev {
        DeviceEvent::ParamChanged { path, value } => {
            match snap.params.iter_mut().find(|p| &p.path == path) {
                Some(p) => {
                    p.value = value.clone();
                    MirrorUpdate::Applied
                }
                None => MirrorUpdate::Unknown,
            }
        }
        DeviceEvent::RouteChanged(x) => {
            match snap.routes.iter_mut().find(|c| c.output == x.output) {
                Some(c) => {
                    c.source.clone_from(&x.source);
                    MirrorUpdate::Applied
                }
                None => MirrorUpdate::Unknown,
            }
        }
        DeviceEvent::SnapshotReplaced => MirrorUpdate::Stale,
        DeviceEvent::Online | DeviceEvent::Offline => MirrorUpdate::Ignored,
    }
}

/// The events that turn `old` into `new`: every param whose value
/// differs (or is new) and every output whose source differs (or is
/// new). Params or outputs that disappeared produce nothing.
#[must_use]
pub fn diff_events(old: &DeviceSnapshot, new: &DeviceSnapshot) -> Vec<DeviceEvent> {
    let old_params: HashMap<&str, &ParamValue> = old
        .params
        .iter()
        .map(|p| (p.path.as_str(), &p.value))
        .collect();
    let old_routes: HashMap<&ChannelRef, Option<&ChannelRef>> = old
        .routes
        .iter()
        .map(|c| (&c.output, c.source.as_ref()))
        .collect();
    let params = new
        .params
        .iter()
        .filter(|p| old_params.get(p.path.as_str()) != Some(&&p.value))
        .map(|p| DeviceEvent::ParamChanged {
            path: p.path.clone(),
            value: p.value.clone(),
        });
    let routes = new
        .routes
        .iter()
        .filter(|c| old_routes.get(&c.output) != Some(&c.source.as_ref()))
        .map(|c| DeviceEvent::RouteChanged(c.clone()));
    params.chain(routes).collect()
}
