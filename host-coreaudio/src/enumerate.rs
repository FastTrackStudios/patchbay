//! HAL facts → graph nodes.
//!
//! Identity:
//! - devices: `coreaudio:device:<DeviceUID>` — stable across reboots;
//! - processes: `coreaudio:process:<pid>` — stable for the process's
//!   lifetime. Persist [`AppInfo::bundle_id`], never the pid.
//!
//! Direction is **graph** perspective (see [`patchbay_host::PortDirection`]):
//! a device's capture channels are graph *outputs*, its playback channels
//! graph *inputs*.

use std::collections::BTreeMap;

use patchbay_host::{AppInfo, HostError, HostNode, NodeDirection, NodeKind, PortCounts};

use crate::ffi::hal::{self, DeviceFacts, ProcessFacts, Transport};

/// Id prefix of every node this backend reports.
pub(crate) const DEVICE_PREFIX: &str = "coreaudio:device:";
const PROCESS_PREFIX: &str = "coreaudio:process:";

/// Channels a process tap delivers (stereo mixdown). Process objects have
/// no channel count of their own; this is what patchbay can take from one.
const APP_TAP_CHANNELS: u32 = 2;

/// Node id of a device.
pub(crate) fn device_id(uid: &str) -> String {
    format!("{DEVICE_PREFIX}{uid}")
}

fn device_node(d: DeviceFacts) -> HostNode {
    let ports = PortCounts {
        inputs: d.output_channels,
        outputs: d.input_channels,
    };
    let kind = match d.transport {
        Transport::Aggregate => NodeKind::Aggregate,
        Transport::Virtual => NodeKind::VirtualDevice,
        Transport::Other => NodeKind::HardwareDevice,
    };
    let mut props = BTreeMap::new();
    props.insert("coreaudio.uid".to_owned(), d.uid.clone());
    props.insert("coreaudio.object_id".to_owned(), d.object.to_string());
    props.insert("coreaudio.transport".to_owned(), d.transport_code);
    props.insert("coreaudio.alive".to_owned(), d.alive.to_string());
    props.insert(
        "coreaudio.running_somewhere".to_owned(),
        d.running_somewhere.to_string(),
    );
    if let Some(m) = d.manufacturer {
        props.insert("coreaudio.manufacturer".to_owned(), m);
    }
    HostNode {
        id: device_id(&d.uid),
        name: d.name,
        kind,
        direction: NodeDirection::from_counts(ports),
        ports,
        sample_rate: d.sample_rate,
        app: None,
        props,
    }
}

fn process_node(p: ProcessFacts) -> HostNode {
    let ports = PortCounts {
        inputs: 0,
        outputs: APP_TAP_CHANNELS,
    };
    let mut props = BTreeMap::new();
    props.insert("coreaudio.object_id".to_owned(), p.object.to_string());
    props.insert("coreaudio.running".to_owned(), p.running.to_string());
    props.insert(
        "coreaudio.running_output".to_owned(),
        p.running_output.to_string(),
    );
    props.insert(
        "coreaudio.running_input".to_owned(),
        p.running_input.to_string(),
    );
    HostNode {
        id: format!("{PROCESS_PREFIX}{}", p.pid),
        name: p.name.clone(),
        kind: NodeKind::AppStream,
        direction: NodeDirection::from_counts(ports),
        ports,
        sample_rate: None,
        app: Some(AppInfo {
            pid: p.pid,
            bundle_id: p.bundle_id,
            name: p.name,
        }),
        props,
    }
}

/// Every device and audio-client process as nodes, sorted by id. Objects
/// that vanish mid-enumeration are skipped.
pub(crate) fn nodes() -> Result<Vec<HostNode>, HostError> {
    let devices = hal::device_ids()?
        .into_iter()
        .filter_map(|id| hal::device(id).ok())
        .map(device_node);
    // Process objects need macOS 14.2+; an older HAL simply has no list.
    let processes = hal::process_ids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| hal::process(id).ok())
        .map(process_node);
    let mut nodes: Vec<HostNode> = devices.chain(processes).collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(nodes)
}

/// Node id of the default output (`true`) / input device.
pub(crate) fn default_device_id(output: bool) -> Option<String> {
    let object = hal::default_device(output).ok().flatten()?;
    hal::device(object).ok().map(|d| device_id(&d.uid))
}
