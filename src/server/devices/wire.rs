//! `patchbay-device` model ↔ `patchbay-proto` wire types.
//!
//! The proto crate never depends on adapters, so every conversion
//! lives here, at the engine boundary.

use crate::device::{
    ChannelRef, Crosspoint, DeviceError, DeviceEvent, DeviceInfo, DeviceSnapshot, Param, ParamKind,
    ParamValue, PortGroup, Transport,
};
use crate::proto::{
    DeviceChannel, DeviceCrosspoint, DeviceEventKind, DeviceParamKind, DeviceParamValue,
    DevicePortGroup, DeviceSummary, DeviceView, ParamView, PatchbayError,
};

pub(crate) fn kind(k: &ParamKind) -> DeviceParamKind {
    match k {
        ParamKind::Level { min_db, max_db } => DeviceParamKind::Level {
            min_db: *min_db,
            max_db: *max_db,
        },
        ParamKind::Pan => DeviceParamKind::Pan,
        ParamKind::Toggle => DeviceParamKind::Toggle,
        ParamKind::Enum { options } => DeviceParamKind::Enum {
            options: options.clone(),
        },
        ParamKind::Int { min, max } => DeviceParamKind::Int {
            min: *min,
            max: *max,
        },
        ParamKind::Text => DeviceParamKind::Text,
    }
}

/// `-0.0` (attenuation 0 negated) → `0.0`, so nothing shows "-0 dB".
fn unsigned_zero(x: f64) -> f64 {
    if x.abs() < f64::MIN_POSITIVE { 0.0 } else { x }
}

pub(crate) fn value(v: &ParamValue) -> DeviceParamValue {
    match v {
        ParamValue::Level(x) => DeviceParamValue::Level(unsigned_zero(*x)),
        ParamValue::Pan(x) => DeviceParamValue::Pan(unsigned_zero(*x)),
        ParamValue::Toggle(b) => DeviceParamValue::Toggle(*b),
        ParamValue::Enum(i) => DeviceParamValue::Enum(*i),
        ParamValue::Int(n) => DeviceParamValue::Int(*n),
        ParamValue::Text(s) => DeviceParamValue::Text(s.clone()),
    }
}

pub(crate) fn value_from_wire(v: &DeviceParamValue) -> ParamValue {
    match v {
        DeviceParamValue::Level(x) => ParamValue::Level(*x),
        DeviceParamValue::Pan(x) => ParamValue::Pan(*x),
        DeviceParamValue::Toggle(b) => ParamValue::Toggle(*b),
        DeviceParamValue::Enum(i) => ParamValue::Enum(*i),
        DeviceParamValue::Int(n) => ParamValue::Int(*n),
        DeviceParamValue::Text(s) => ParamValue::Text(s.clone()),
    }
}

pub(crate) fn param(p: &Param) -> ParamView {
    ParamView {
        path: p.path.clone(),
        label: p.label.clone(),
        kind: kind(&p.kind),
        value: value(&p.value),
        writable: p.writable,
        disruptive: p.disruptive,
    }
}

pub(crate) fn channel(c: &ChannelRef) -> DeviceChannel {
    DeviceChannel::new(c.group.clone(), c.channel)
}

pub(crate) fn channel_from_wire(c: &DeviceChannel) -> ChannelRef {
    ChannelRef::new(c.group.clone(), c.channel)
}

pub(crate) fn crosspoint(c: &Crosspoint) -> DeviceCrosspoint {
    DeviceCrosspoint {
        output: channel(&c.output),
        source: c.source.as_ref().map(channel),
    }
}

fn group(g: &PortGroup) -> DevicePortGroup {
    DevicePortGroup {
        id: g.id.clone(),
        name: g.name.clone(),
        channels: g.channels,
    }
}

pub(crate) fn transport(t: &Transport) -> String {
    match t {
        Transport::Tcp {
            addr,
            via: Some(via),
        } => format!("tcp {addr} via {via}"),
        Transport::Tcp { addr, via: None } => format!("tcp {addr}"),
        Transport::Other { description } => description.clone(),
    }
}

/// Fill a summary's identity fields from adapter info.
pub(crate) fn apply_info(s: &mut DeviceSummary, info: &DeviceInfo) {
    s.id = info.id.to_string();
    s.vendor.clone_from(&info.vendor);
    s.model.clone_from(&info.model);
    s.serial = info.serial.clone().unwrap_or_default();
    s.firmware = info.firmware.clone().unwrap_or_default();
    s.transport = transport(&info.transport);
}

pub(crate) fn view(summary: DeviceSummary, snap: &DeviceSnapshot) -> DeviceView {
    DeviceView {
        summary,
        inputs: snap.inputs.iter().map(group).collect(),
        outputs: snap.outputs.iter().map(group).collect(),
        routes: snap.routes.iter().map(crosspoint).collect(),
        params: snap.params.iter().map(param).collect(),
    }
}

pub(crate) fn event(ev: &DeviceEvent) -> DeviceEventKind {
    match ev {
        DeviceEvent::ParamChanged { path, value: v } => DeviceEventKind::ParamChanged {
            path: path.clone(),
            value: value(v),
        },
        DeviceEvent::RouteChanged(c) => DeviceEventKind::RouteChanged {
            crosspoint: crosspoint(c),
        },
        DeviceEvent::Online => DeviceEventKind::Online,
        DeviceEvent::Offline => DeviceEventKind::Offline,
        DeviceEvent::SnapshotReplaced => DeviceEventKind::SnapshotReplaced,
    }
}

/// Stable agent-facing code for a device error.
pub(crate) const fn error_code(e: &DeviceError) -> &'static str {
    match e {
        DeviceError::Offline => "offline",
        DeviceError::Timeout(_) => "timeout",
        DeviceError::UnknownParam(_) => "unknown_param",
        DeviceError::UnknownPort(_) => "unknown_port",
        DeviceError::InvalidValue { .. } => "invalid_value",
        DeviceError::ReadOnly(_) => "read_only",
        DeviceError::DisruptiveWrite(_) => "disruptive_write",
        DeviceError::Unsupported(_) => "unsupported",
        DeviceError::Protocol(_) => "protocol",
        DeviceError::Transport(_) => "transport",
    }
}

pub(crate) fn error(e: &DeviceError) -> PatchbayError {
    PatchbayError::device(error_code(e), e.to_string())
}
