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

use inferno_net::protocol::subscription_status_label;
use patchbay_device::{
    ChannelRef, Crosspoint, DeviceInfo, DeviceSnapshot, Param, ParamKind, ParamValue, PortGroup,
};

use crate::control::{ChannelSide, DanteDeviceState};

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

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;

    use patchbay_device::{DeviceId, Transport};

    use super::*;
    use crate::control::{Endpoint, RxChannel, TxChannel};

    pub(crate) fn device(
        name: &str,
        port: u16,
        tx: &[&str],
        rx: &[(&str, &str)],
    ) -> DanteDeviceState {
        DanteDeviceState {
            endpoint: Endpoint {
                name: name.to_owned(),
                addr: SocketAddr::from(([127, 0, 0, 1], port)),
            },
            arc_name: Some(name.to_owned()),
            model: Some("Test".to_owned()),
            sample_rate: Some(48_000),
            latency_us: Some(1000),
            tx: tx
                .iter()
                .zip(1_u16..)
                .map(|(n, number)| TxChannel {
                    number,
                    name: (*n).to_owned(),
                })
                .collect(),
            rx: rx
                .iter()
                .zip(1_u16..)
                .map(|((ch, dev), number)| RxChannel {
                    number,
                    name: format!("{number:02}"),
                    tx_channel: (!ch.is_empty()).then(|| (*ch).to_owned()),
                    tx_device: (!dev.is_empty()).then(|| (*dev).to_owned()),
                    status: if ch.is_empty() { 0 } else { 9 },
                })
                .collect(),
            reachable: true,
        }
    }

    pub(crate) fn info() -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::from_parts("dante", "network", "dante"),
            vendor: "Audinate".into(),
            model: "Dante network".into(),
            serial: None,
            firmware: None,
            transport: Transport::Other {
                description: "test".into(),
            },
            online: true,
        }
    }

    #[test]
    fn subscriptions_become_crosspoints() {
        let devices = vec![
            device("Console", 1, &["Main L", "Main R"], &[("", "")]),
            device(
                "Stage",
                2,
                &["Mic 1"],
                &[
                    ("Main L", "Console"),
                    ("Main R", "Console"),
                    ("Gone", "Offsite"),
                    ("Mic 1", ""),
                ],
            ),
        ];
        let s = snapshot(info(), &devices);
        assert_eq!(s.inputs.len(), 2);
        assert_eq!(s.input("Console").map(|g| g.channels), Some(2));
        assert_eq!(s.output("Stage").map(|g| g.channels), Some(4));
        let src = |dev: &str, ch: u16| s.source_of(&ChannelRef::new(dev, ch)).expect("route");
        assert_eq!(src("Stage", 0), Some(&ChannelRef::new("Console", 0)));
        assert_eq!(src("Stage", 1), Some(&ChannelRef::new("Console", 1)));
        // Unknown TX device: no crosspoint source, subscription still shown.
        assert_eq!(src("Stage", 2), None);
        assert_eq!(
            s.param("Stage/rx/3/subscription").map(|p| p.value.clone()),
            Some(ParamValue::Text("Gone@Offsite".into()))
        );
        // Empty device = local loopback.
        assert_eq!(src("Stage", 3), Some(&ChannelRef::new("Stage", 0)));
        assert_eq!(src("Console", 0), None);
        let p = s.param("Console/tx/1/name").expect("param");
        assert!(p.writable);
        assert_eq!(p.value, ParamValue::Text("Main L".into()));
        assert!(!s.param("Console/sample_rate").expect("sr").writable);
        assert_eq!(
            s.param("Stage/rx/1/status").map(|p| p.value.clone()),
            Some(ParamValue::Text("dynamic".into()))
        );
    }

    #[test]
    fn unreachable_devices_have_no_groups() {
        let mut d = device("Dead", 3, &["A"], &[("", "")]);
        d.reachable = false;
        let s = snapshot(info(), &[d]);
        assert!(s.inputs.is_empty() && s.outputs.is_empty() && s.routes.is_empty());
        assert_eq!(
            s.param("Dead/reachable").map(|p| p.value.clone()),
            Some(ParamValue::Toggle(false))
        );
        assert!(!s.param("Dead/name").expect("name").writable);
    }

    #[test]
    fn param_paths_parse() {
        assert_eq!(
            parse_param_path("Console/name"),
            Some(("Console", ParamTarget::DeviceName))
        );
        assert_eq!(
            parse_param_path("Console/tx/12/name"),
            Some(("Console", ParamTarget::ChannelName(ChannelSide::Tx, 12)))
        );
        assert_eq!(
            parse_param_path("Console/rx/3/subscription"),
            Some(("Console", ParamTarget::ReadOnly))
        );
        assert_eq!(parse_param_path("Console/tx/0/name"), None);
        assert_eq!(parse_param_path("Console/bogus"), None);
        assert_eq!(parse_param_path("/name"), None);
    }

    #[test]
    fn name_rules() {
        assert!(validate_device_name("Stage-Box-1").is_ok());
        assert!(validate_device_name("-bad").is_err());
        assert!(validate_device_name("has space").is_err());
        assert!(validate_device_name(&"x".repeat(32)).is_err());
        assert!(validate_channel_name("Kick In").is_ok());
        assert!(validate_channel_name("a@b").is_err());
        assert!(validate_channel_name("").is_err());
    }
}
