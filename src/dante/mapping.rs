//! Pure mapping: network state → the generic device model, and param
//! paths → write targets.
//!
//! - Router **outputs**: one group per device with RX channels, id = the
//!   device name, `channels` = highest RX channel number.
//! - Router **inputs**: one group per device with TX channels, id = the
//!   device name.
//! - **Routes**: one crosspoint per RX channel; the source is the TX
//!   channel (matched by name, as the subscription names it) on the TX
//!   device. A subscription to a device/channel that isn't on the network
//!   stays visible in `…/subscription` but has no crosspoint source.
//! - **Params** (per device `<d>`): `<d>/name` (writable), `<d>/model`,
//!   `<d>/address`, `<d>/reachable`, `<d>/sample_rate`, `<d>/latency_us`
//!   (read-only), `<d>/tx/<n>/name`, `<d>/rx/<n>/name` (writable up to
//!   channel 255 — ARC's rename carries one byte), `<d>/rx/<n>/subscription`
//!   (`channel@device`, read-only), `<d>/rx/<n>/status` (read-only).

use crate::device::{
    ChannelRef, Crosspoint, DeviceInfo, DeviceSnapshot, Param, ParamKind, ParamValue, PortGroup,
};
use inferno_net::protocol::subscription_status_label;

use crate::dante::control::{ChannelSide, DanteDeviceState};

/// Longest name Dante accepts (device and channel).
const MAX_NAME: usize = 31;

/// What a writable param path addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamTarget {
    /// `<d>/name`.
    DeviceName,
    /// `<d>/{tx,rx}/<n>/name` (1-based `n`).
    ChannelName(ChannelSide, u16),
    /// A known read-only path.
    ReadOnly,
}

/// Split a param path into `(device, target)`. `None` = not a path
/// this adapter produces.
#[must_use]
pub fn parse_param_path(path: &str) -> Option<(&str, ParamTarget)> {
    let mut it = path.split('/');
    let device = it.next().filter(|d| !d.is_empty())?;
    let rest: Vec<&str> = it.collect();
    let target = match rest.as_slice() {
        ["name"] => ParamTarget::DeviceName,
        ["model" | "address" | "reachable" | "sample_rate" | "latency_us"] => ParamTarget::ReadOnly,
        [side @ ("tx" | "rx"), n, "name"] => {
            let n: u16 = n.parse().ok().filter(|n| *n > 0)?;
            let side = if *side == "tx" {
                ChannelSide::Tx
            } else {
                ChannelSide::Rx
            };
            ParamTarget::ChannelName(side, n)
        }
        ["rx", n, "subscription" | "status"] => {
            n.parse::<u16>().ok().filter(|n| *n > 0)?;
            ParamTarget::ReadOnly
        }
        _ => return None,
    };
    Some((device, target))
}

/// Dante device names: 1–31 characters, ASCII letters, digits and `-`,
/// not starting or ending with `-`.
///
/// # Errors
/// Why the name is refused.
pub fn validate_device_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!("device name must be 1–{MAX_NAME} characters"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("device name may contain only letters, digits and '-'".to_owned());
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("device name may not start or end with '-'".to_owned());
    }
    Ok(())
}

/// Dante channel names: 1–31 characters, no `=`, `.` or `@` (they
/// delimit subscriptions and mDNS names).
///
/// # Errors
/// Why the name is refused.
pub fn validate_channel_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!("channel name must be 1–{MAX_NAME} characters"));
    }
    if name.contains(['=', '.', '@']) {
        return Err("channel name may not contain '=', '.' or '@'".to_owned());
    }
    Ok(())
}

const fn text(path: String, label: String, value: String, writable: bool) -> Param {
    Param {
        path,
        label,
        kind: ParamKind::Text,
        value: ParamValue::Text(value),
        writable,
        disruptive: false,
    }
}

fn int(path: String, label: String, value: u32) -> Param {
    Param {
        path,
        label,
        kind: ParamKind::Int {
            min: 0,
            max: i64::from(u32::MAX),
        },
        value: ParamValue::Int(i64::from(value)),
        writable: false,
        disruptive: false,
    }
}

/// The TX channel a subscription points at, as a router input.
fn resolve_source(
    devices: &[DanteDeviceState],
    rx_device: &str,
    tx_device: Option<&str>,
    tx_channel: Option<&str>,
) -> Option<ChannelRef> {
    let channel = tx_channel.filter(|c| !c.is_empty())?;
    // An empty device name means "this device" (local loopback).
    let device = tx_device
        .filter(|d| !d.is_empty() && *d != ".")
        .unwrap_or(rx_device);
    let dev = devices.iter().find(|d| d.name() == device && d.reachable)?;
    let tx = dev.tx.iter().find(|c| c.name == channel).or_else(|| {
        // Some devices report the default numeric name.
        let n: u16 = channel.parse().ok()?;
        dev.tx.iter().find(|c| c.number == n)
    })?;
    Some(ChannelRef::new(device, tx.number.checked_sub(1)?))
}

fn max_number(numbers: impl Iterator<Item = u16>) -> u16 {
    numbers.max().unwrap_or(0)
}

/// Build the generic snapshot of the whole network.
#[must_use]
pub fn snapshot(info: DeviceInfo, devices: &[DanteDeviceState]) -> DeviceSnapshot {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut routes = Vec::new();
    let mut params = Vec::new();
    for d in devices {
        device_params(d, &mut params);
        if !d.reachable {
            continue;
        }
        channels(
            devices,
            d,
            &mut inputs,
            &mut outputs,
            &mut routes,
            &mut params,
        );
    }
    DeviceSnapshot {
        info,
        inputs,
        outputs,
        routes,
        params,
    }
}

/// Identity / status params of one device.
fn device_params(d: &DanteDeviceState, params: &mut Vec<Param>) {
    let name = d.name();
    {
        params.push(text(
            format!("{name}/name"),
            format!("{name} · device name"),
            name.to_owned(),
            d.reachable,
        ));
        params.push(text(
            format!("{name}/address"),
            format!("{name} · ARC address"),
            d.endpoint.addr.to_string(),
            false,
        ));
        params.push(Param {
            path: format!("{name}/reachable"),
            label: format!("{name} · reachable"),
            kind: ParamKind::Toggle,
            value: ParamValue::Toggle(d.reachable),
            writable: false,
            disruptive: false,
        });
        if let Some(m) = &d.model {
            params.push(text(
                format!("{name}/model"),
                format!("{name} · model"),
                m.clone(),
                false,
            ));
        }
        if let Some(sr) = d.sample_rate {
            params.push(int(
                format!("{name}/sample_rate"),
                format!("{name} · sample rate (Hz)"),
                sr,
            ));
        }
        if let Some(l) = d.latency_us {
            params.push(int(
                format!("{name}/latency_us"),
                format!("{name} · latency (µs)"),
                l,
            ));
        }
    }
}

/// Port groups, crosspoints and channel params of one reachable device.
fn channels(
    devices: &[DanteDeviceState],
    d: &DanteDeviceState,
    inputs: &mut Vec<PortGroup>,
    outputs: &mut Vec<PortGroup>,
    routes: &mut Vec<Crosspoint>,
    params: &mut Vec<Param>,
) {
    let name = d.name();
    {
        let tx_count = max_number(d.tx.iter().map(|c| c.number));
        if tx_count > 0 {
            inputs.push(PortGroup {
                id: name.to_owned(),
                name: format!("{name} TX"),
                channels: tx_count,
            });
        }
        for c in &d.tx {
            params.push(text(
                format!("{name}/tx/{}/name", c.number),
                format!("{name} · TX {}", c.number),
                c.name.clone(),
                c.number <= 255,
            ));
        }
        let rx_count = max_number(d.rx.iter().map(|c| c.number));
        if rx_count > 0 {
            outputs.push(PortGroup {
                id: name.to_owned(),
                name: format!("{name} RX"),
                channels: rx_count,
            });
        }
        for idx in 0..rx_count {
            let number = idx.saturating_add(1);
            let source = d.rx.iter().find(|c| c.number == number).and_then(|c| {
                resolve_source(
                    devices,
                    name,
                    c.tx_device.as_deref(),
                    c.tx_channel.as_deref(),
                )
            });
            routes.push(Crosspoint {
                output: ChannelRef::new(name, idx),
                source,
            });
        }
        for c in &d.rx {
            let n = c.number;
            params.push(text(
                format!("{name}/rx/{n}/name"),
                format!("{name} · RX {n}"),
                c.name.clone(),
                n <= 255,
            ));
            let sub = if c.is_subscribed() {
                format!(
                    "{}@{}",
                    c.tx_channel.as_deref().unwrap_or_default(),
                    c.tx_device.as_deref().unwrap_or_default()
                )
            } else {
                String::new()
            };
            params.push(text(
                format!("{name}/rx/{n}/subscription"),
                format!("{name} · RX {n} subscription"),
                sub,
                false,
            ));
            params.push(text(
                format!("{name}/rx/{n}/status"),
                format!("{name} · RX {n} status"),
                subscription_status_label(c.status).to_owned(),
                false,
            ));
        }
    }
}
