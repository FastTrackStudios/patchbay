//! Manager Server discovery (UDP multicast announces).
//!
//! The server multicasts one JSON object per service per interface every
//! 500 ms to `239.192.5.8:5008`. Ports are dynamic (seen: admin 2020,
//! control 2022 + 2023) — always discover, never hard-code.
//!
//! A single device may announce several control endpoints; on Manager
//! Server 1.8.19 one of them (loopback-only 2022) streams only AFX meters
//! and answers every `get_*` with `COMMAND_STATUS: FAIL`. Callers should
//! probe candidates in [`control_endpoints`] order and keep the first
//! that answers a read (see `Galaxy32Adapter::discover_and_connect`).

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::net::UdpSocket;

use crate::error::{AntelopeError, Result};

/// Multicast group of the announces.
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 192, 5, 8);
/// UDP port of the announces.
pub const DISCOVERY_PORT: u16 = 5008;
/// Service type of the per-device control endpoint.
pub const CONTROL_SERVICE: &str = "_antelope_control._tcp.local.";
/// Service type of the server admin endpoint.
pub const ADMIN_SERVICE: &str = "_antelope_admin._tcp.local.";

/// Device/server properties carried in an announce.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnnounceProperties {
    /// e.g. `Galaxy32`.
    #[serde(default)]
    pub device_name: Option<String>,
    /// e.g. `4202524000109`.
    #[serde(default)]
    pub serial_number: Option<String>,
    /// e.g. `8.24`.
    #[serde(default)]
    pub firmware_version: Option<String>,
    /// e.g. `11.4`.
    #[serde(default)]
    pub hardware_version: Option<String>,
    /// e.g. `Thunderbolt`.
    #[serde(default)]
    pub connection_type: Option<String>,
    /// Manager Server version, e.g. `1.8.19`.
    #[serde(default)]
    pub server_version: Option<String>,
    /// Everything else (`mode`, `vendor_id`, `product_id`, …).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One multicast announce.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announce {
    /// Address the service listens on.
    pub ip: String,
    /// TCP port (dynamic).
    pub port: u16,
    /// Service instance uuid (stable per endpoint across interfaces).
    #[serde(default)]
    pub uuid: String,
    /// mDNS-style instance name.
    #[serde(default)]
    pub name: String,
    /// [`CONTROL_SERVICE`] or [`ADMIN_SERVICE`].
    #[serde(rename = "type")]
    pub service_type: String,
    /// Device/server properties.
    #[serde(default)]
    pub properties: AnnounceProperties,
}

impl Announce {
    /// Whether this announces a device control endpoint.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.service_type == CONTROL_SERVICE
    }

    /// Whether this announces the server admin endpoint.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.service_type == ADMIN_SERVICE
    }

    /// `ip:port`, if `ip` parses.
    #[must_use]
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.ip
            .parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, self.port))
    }

    /// Device serial, if announced.
    #[must_use]
    pub fn serial(&self) -> Option<&str> {
        self.properties.serial_number.as_deref()
    }
}

/// A bound multicast listener.
#[derive(Debug)]
pub struct Listener {
    socket: UdpSocket,
}

impl Listener {
    /// Bind `0.0.0.0:5008` with `SO_REUSEADDR` + `SO_REUSEPORT` (the
    /// official panel listens too) and join the group on the default
    /// interface and on loopback (local servers announce on 127.0.0.1).
    ///
    /// # Errors
    /// Socket setup failure. Must be called inside a tokio runtime.
    pub fn bind() -> Result<Self> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        sock.set_reuse_address(true)?;
        sock.set_reuse_port(true)?;
        sock.bind(&SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            DISCOVERY_PORT,
        )))?;
        let mut joined = false;
        for iface in [Ipv4Addr::UNSPECIFIED, Ipv4Addr::LOCALHOST] {
            match sock.join_multicast_v4(&MULTICAST_GROUP, &iface) {
                Ok(()) => joined = true,
                Err(e) => {
                    tracing::debug!(%iface, error = %e, "antelope discovery: multicast join failed");
                }
            }
        }
        if !joined {
            return Err(std::io::Error::other(
                "could not join the Antelope multicast group on any interface",
            )
            .into());
        }
        sock.set_nonblocking(true)?;
        let socket = UdpSocket::from_std(std::net::UdpSocket::from(sock))?;
        Ok(Self { socket })
    }

    /// Wait for the next announce (unparseable datagrams are skipped).
    ///
    /// # Errors
    /// Socket failure.
    pub async fn recv(&self) -> Result<Announce> {
        let mut buf = vec![0u8; 65_536];
        loop {
            let (n, from) = self.socket.recv_from(&mut buf).await?;
            let Some(datagram) = buf.get(..n) else {
                continue;
            };
            match serde_json::from_slice::<Announce>(datagram) {
                Ok(a) => return Ok(a),
                Err(e) => {
                    tracing::debug!(%from, error = %e, "antelope discovery: skipping datagram");
                }
            }
        }
    }
}

/// Listen for `timeout` and return every distinct announce
/// (deduplicated by `ip`, `port`, `uuid`). Announces repeat every 500 ms,
/// so ~1.5 s is plenty.
///
/// # Errors
/// Socket setup / receive failure.
pub async fn discover(timeout: Duration) -> Result<Vec<Announce>> {
    let listener = Listener::bind()?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let collect = async {
        loop {
            match listener.recv().await {
                Ok(a) => {
                    if seen.insert((a.ip.clone(), a.port, a.uuid.clone())) {
                        out.push(a);
                    }
                }
                Err(e) => return Err::<(), AntelopeError>(e),
            }
        }
    };
    match tokio::time::timeout(timeout, collect).await {
        Ok(Err(e)) => Err(e),
        Ok(Ok(())) | Err(_) => Ok(out),
    }
}

/// Control endpoints for the device with `serial` (any device if `None`).
///
/// Best candidate first: loopback before LAN addresses, then endpoints
/// also announced on a non-loopback address (the panel-facing one on
/// 1.8.19), then higher port first.
#[must_use]
pub fn control_endpoints(announces: &[Announce], serial: Option<&str>) -> Vec<SocketAddr> {
    let matching: Vec<&Announce> = announces
        .iter()
        .filter(|a| a.is_control())
        .filter(|a| serial.is_none_or(|s| a.serial() == Some(s)))
        .collect();
    let lan_ports: HashSet<u16> = matching
        .iter()
        .filter(|a| a.socket_addr().is_some_and(|s| !s.ip().is_loopback()))
        .map(|a| a.port)
        .collect();
    let mut addrs: Vec<SocketAddr> = matching.iter().filter_map(|a| a.socket_addr()).collect();
    addrs.sort_by_key(|a| {
        (
            !a.ip().is_loopback(),
            !lan_ports.contains(&a.port()),
            std::cmp::Reverse(a.port()),
        )
    });
    addrs.dedup();
    addrs
}
