//! Find TF consoles on the local network.
//!
//! RCP has no multicast announce, so discovery is a bounded TCP probe
//! on port 49280, confirmed with `devinfo productname` — a **read-only**
//! query. A console counts when the product name starts with `TF`.
//! Nothing else is ever sent.
//!
//! The probe runs in stages and stops at the first stage that finds a
//! console, so a console the OS already knows about is found in one
//! round trip instead of a ~750-host sweep:
//!
//! 1. ARP neighbours whose MAC carries a Yamaha OUI,
//! 2. every other resolved ARP neighbour on a scannable network,
//! 3. the full sweep of the machine's private IPv4 subnets (`/24` or
//!    smaller, loopback / tunnel / AWDL / bridge interfaces skipped),
//!    minus the addresses already probed.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;

use crate::client::RCP_PORT;
use crate::rcp::reply::Line;

/// Scan limits.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// RCP port to probe.
    pub port: u16,
    /// Hard cap on the number of addresses probed per scan.
    pub max_hosts: usize,
    /// Probes in flight at once.
    pub concurrency: usize,
    /// TCP connect timeout per host.
    pub connect_timeout: Duration,
    /// How long an open port gets to answer `devinfo productname`.
    pub reply_timeout: Duration,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            port: RCP_PORT,
            max_hosts: 1024,
            // Each probe holds a socket; launchd gives GUI apps a
            // 256-fd soft limit, so stay well under it.
            concurrency: 64,
            connect_timeout: Duration::from_millis(600),
            reply_timeout: Duration::from_millis(1500),
        }
    }
}

/// A console that answered the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundConsole {
    /// RCP endpoint.
    pub addr: SocketAddr,
    /// `devinfo productname` (`TF1`, `TF5`, …).
    pub product: String,
    /// The console's MAC, when the neighbour table had it — a stable
    /// identity across DHCP address changes (see [`find_by_mac`]).
    pub mac: Option<[u8; 6]>,
}

/// One local IPv4 network: interface name, own address, prefix length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNet {
    /// Interface name (`en8`).
    pub interface: String,
    /// This machine's address on it.
    pub ip: Ipv4Addr,
    /// Prefix length (`24`).
    pub prefix: u8,
}

/// Interface-name prefixes that never lead to a console: loopback,
/// VPN tunnels, Apple Wireless Direct Link, low-latency WLAN, bridges.
const SKIP_PREFIXES: &[&str] = &["lo", "utun", "awdl", "llw", "bridge", "gif", "stf", "anpi"];

/// Whether `product` is a TF-series console.
#[must_use]
pub fn is_tf_product(product: &str) -> bool {
    product.trim().starts_with("TF")
}

/// Private (RFC 1918) address.
const fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// The machine's scannable networks: private IPv4, prefix `/24` or
/// longer, interface not in the skip list.
#[must_use]
pub fn local_networks() -> Vec<LocalNet> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    ifaces
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(LocalNet {
                interface: i.name,
                ip: v4.ip,
                prefix: v4.prefixlen,
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .filter(|n| scannable(n))
        .collect()
}

fn scannable(n: &LocalNet) -> bool {
    !SKIP_PREFIXES.iter().any(|p| n.interface.starts_with(p))
        && !n.ip.is_loopback()
        && !n.ip.is_link_local()
        && is_private(n.ip)
        && (24..=30).contains(&n.prefix)
}

/// Every host address of `nets` (network and broadcast addresses and
/// the machine's own addresses excluded), deduplicated, in network
/// order, capped at `max_hosts`. Pure.
#[must_use]
pub fn scan_targets(nets: &[LocalNet], max_hosts: usize) -> Vec<Ipv4Addr> {
    let own: BTreeSet<Ipv4Addr> = nets.iter().map(|n| n.ip).collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for n in nets.iter().filter(|n| scannable(n)) {
        let host_bits = 32_u32.saturating_sub(u32::from(n.prefix));
        let Some(size) = 1_u32.checked_shl(host_bits) else {
            continue;
        };
        let mask = u32::MAX.checked_shl(host_bits).unwrap_or(0);
        let base = u32::from(n.ip) & mask;
        // Skip the network (0) and broadcast (size - 1) addresses.
        for offset in 1..size.saturating_sub(1) {
            if out.len() >= max_hosts {
                return out;
            }
            let Some(raw) = base.checked_add(offset) else {
                break;
            };
            let ip = Ipv4Addr::from(raw);
            if !own.contains(&ip) && seen.insert(ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// Probe one address: connect, ask `devinfo productname`, return the
/// product name if the peer answered like an RCP console. Read-only.
///
/// `None` = nothing listening / not RCP.
pub async fn probe(
    addr: SocketAddr,
    connect_timeout: Duration,
    reply_timeout: Duration,
) -> Option<String> {
    match probe_detail(addr, connect_timeout, reply_timeout).await {
        Probe::Console(product) => Some(product),
        Probe::Silent | Probe::NoAnswer => None,
    }
}

/// What was at an address.
///
/// [`Probe::Silent`] is the interesting one: something is listening on
/// the RCP port and will not answer `devinfo`. A console does that while
/// it is still booting, and reporting it as "nothing found" sends people
/// looking for a network fault that isn't there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing accepted a connection.
    NoAnswer,
    /// Connected, but no RCP reply inside the timeout.
    Silent,
    /// Answered `devinfo productname`.
    Console(String),
}

/// Probe one address, keeping what happened.
pub async fn probe_detail(
    addr: SocketAddr,
    connect_timeout: Duration,
    reply_timeout: Duration,
) -> Probe {
    let Ok(Ok(mut stream)) = tokio::time::timeout(connect_timeout, TcpStream::connect(addr)).await
    else {
        return Probe::NoAnswer;
    };
    let ask = async {
        stream.write_all(b"devinfo productname\n").await.ok()?;
        let mut lines = BufReader::new(&mut stream).lines();
        // A console may interleave NOTIFY lines; take the first reply.
        while let Ok(Some(line)) = lines.next_line().await {
            match Line::parse(line.trim_end_matches('\r')) {
                Line::Ok { verb, args, .. } if verb == "devinfo" => {
                    return args.get(1).map(|t| t.text.clone());
                }
                Line::Error { .. } => return None,
                _ => {}
            }
        }
        None
    };
    let product = tokio::time::timeout(reply_timeout, ask)
        .await
        .ok()
        .flatten();
    let _ = stream.shutdown().await;
    product.map_or(Probe::Silent, Probe::Console)
}

/// What a scan of some addresses turned up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Consoles that answered `devinfo` with a TF product name.
    pub consoles: Vec<FoundConsole>,
    /// Addresses with the RCP port open that never replied — usually a
    /// console still booting, occasionally something else on 49280.
    pub silent: Vec<SocketAddr>,
}

/// Probe `targets` concurrently and return every TF console found.
pub async fn scan_addrs(targets: &[Ipv4Addr], opts: &ScanOptions) -> Vec<FoundConsole> {
    scan_addrs_detail(targets, opts).await.consoles
}

/// As [`scan_addrs`], keeping the addresses that accepted a connection
/// and then said nothing.
pub async fn scan_addrs_detail(targets: &[Ipv4Addr], opts: &ScanOptions) -> Scan {
    let port = opts.port;
    let (ct, rt) = (opts.connect_timeout, opts.reply_timeout);
    let results: Vec<(SocketAddr, Probe)> = stream::iter(targets.iter().copied())
        .map(|ip| async move {
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            (addr, probe_detail(addr, ct, rt).await)
        })
        .buffer_unordered(opts.concurrency.max(1))
        .collect()
        .await;
    let mut scan = Scan::default();
    for (addr, probe) in results {
        match probe {
            Probe::Console(product) if is_tf_product(&product) => {
                scan.consoles.push(FoundConsole {
                    addr,
                    product,
                    mac: None,
                });
            }
            // Something answered, but not as a TF: worth saying so.
            Probe::Console(_) | Probe::Silent => scan.silent.push(addr),
            Probe::NoAnswer => {}
        }
    }
    scan.consoles.sort_by_key(|f| f.addr);
    scan.silent.sort();
    scan
}

/// IEEE OUIs registered to Yamaha Corporation.
const YAMAHA_OUIS: &[[u8; 3]] = &[[0x00, 0xa0, 0xde], [0xac, 0x44, 0xf2]];

/// IEEE OUI of Audinate — Dante chipsets and cards. A console's Dante
/// card (e.g. the TF's NY64-D) has its own address and never speaks RCP;
/// the console is reached on its NETWORK port, a different interface.
const AUDINATE_OUI: [u8; 3] = [0x00, 0x1d, 0xc1];

/// A resolved entry of the OS neighbour (ARP) table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Neighbor {
    /// Neighbour address.
    pub ip: Ipv4Addr,
    /// Its MAC address.
    pub mac: [u8; 6],
}

impl Neighbor {
    /// Whether the MAC carries a Yamaha OUI.
    #[must_use]
    pub fn is_yamaha(&self) -> bool {
        let [a, b, c, ..] = self.mac;
        YAMAHA_OUIS.contains(&[a, b, c])
    }

    /// Whether the MAC is a Dante (Audinate) interface.
    #[must_use]
    pub fn is_dante(&self) -> bool {
        let [a, b, c, ..] = self.mac;
        [a, b, c] == AUDINATE_OUI
    }
}

/// Format a MAC as lower-case `aa:bb:cc:dd:ee:ff`.
#[must_use]
pub fn format_mac(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Parse a MAC written as six `:`-separated hex octets. BSD `arp` drops
/// leading zeros (`0:a0:de:5c:6d:1b`), so one-digit octets are accepted.
#[must_use]
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut mac = [0_u8; 6];
    let mut parts = s.split(':');
    for slot in &mut mac {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

/// Parse BSD/macOS `arp -an` output:
/// `? (192.168.1.214) at 0:a0:de:5c:6d:1b on en8 ifscope [ethernet]`.
/// Incomplete, broadcast and multicast entries are dropped. Pure.
#[must_use]
pub fn parse_arp_an(text: &str) -> Vec<Neighbor> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let ip = words
                .find(|w| w.starts_with('('))?
                .trim_start_matches('(')
                .trim_end_matches(')')
                .parse()
                .ok()?;
            words.find(|w| *w == "at")?;
            let mac = parse_mac(words.next()?)?;
            Some(Neighbor { ip, mac })
        })
        .filter(usable_neighbor)
        .collect()
}

/// Parse Linux `/proc/net/arp` (header line, then
/// `IP HWtype Flags HWaddress Mask Device`). Pure.
#[must_use]
pub fn parse_proc_net_arp(text: &str) -> Vec<Neighbor> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let ip = cols.next()?.parse().ok()?;
            let flags = cols.nth(1)?;
            // 0x0 = incomplete.
            if flags == "0x0" {
                return None;
            }
            let mac = parse_mac(cols.next()?)?;
            Some(Neighbor { ip, mac })
        })
        .filter(usable_neighbor)
        .collect()
}

fn usable_neighbor(n: &Neighbor) -> bool {
    let [first, ..] = n.mac;
    // All-zero, broadcast and group (multicast) MACs never host a console.
    n.mac != [0; 6] && n.mac != [0xff; 6] && first & 1 == 0
}

/// Read the OS neighbour table, best-effort (empty on any failure).
pub async fn neighbors() -> Vec<Neighbor> {
    if cfg!(target_os = "linux") {
        return tokio::fs::read_to_string("/proc/net/arp")
            .await
            .map(|t| parse_proc_net_arp(&t))
            .unwrap_or_default();
    }
    let run = tokio::process::Command::new("/usr/sbin/arp")
        .arg("-an")
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(2), run).await {
        Ok(Ok(out)) if out.status.success() => parse_arp_an(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// Order the probe: Yamaha-OUI neighbours, other neighbours, the rest.
///
/// Each stage is deduplicated against the earlier ones and limited to
/// `sweep` (the scannable networks). Known Dante (Audinate) interfaces
/// are left out of every stage. Pure.
#[must_use]
pub fn plan_stages(neighbors: &[Neighbor], sweep: &[Ipv4Addr]) -> [Vec<Ipv4Addr>; 3] {
    let in_sweep: BTreeSet<Ipv4Addr> = sweep.iter().copied().collect();
    let mut seen: BTreeSet<Ipv4Addr> = neighbors
        .iter()
        .filter(|n| n.is_dante())
        .map(|n| n.ip)
        .collect();
    let mut take = |ips: &mut dyn Iterator<Item = Ipv4Addr>| -> Vec<Ipv4Addr> {
        ips.filter(|ip| in_sweep.contains(ip) && seen.insert(*ip))
            .collect()
    };
    let yamaha = take(&mut neighbors.iter().filter(|n| n.is_yamaha()).map(|n| n.ip));
    let others = take(&mut neighbors.iter().map(|n| n.ip));
    let rest = take(&mut sweep.iter().copied());
    [yamaha, others, rest]
}

/// Fill in each console's MAC from the neighbour table (the probe just
/// talked to it, so its entry is fresh).
async fn with_macs(mut found: Vec<FoundConsole>) -> Vec<FoundConsole> {
    let table = neighbors().await;
    for f in &mut found {
        f.mac = table
            .iter()
            .find(|n| IpAddr::V4(n.ip) == f.addr.ip())
            .map(|n| n.mac);
    }
    found
}

/// Re-find a console by its MAC: look the MAC up in the neighbour
/// table and confirm the address with the read-only probe. This follows
/// a console across DHCP address changes in one round trip.
pub async fn find_by_mac(mac: [u8; 6], opts: &ScanOptions) -> Option<FoundConsole> {
    let table = neighbors().await;
    for n in table.iter().filter(|n| n.mac == mac) {
        let addr = SocketAddr::new(IpAddr::V4(n.ip), opts.port);
        if let Some(product) = probe(addr, opts.connect_timeout, opts.reply_timeout)
            .await
            .filter(|p| is_tf_product(p))
        {
            return Some(FoundConsole {
                addr,
                product,
                mac: Some(mac),
            });
        }
    }
    None
}

/// Scan the machine's local networks for TF consoles, in stages (see
/// the module docs); returns the consoles of the first stage that
/// found any.
pub async fn discover(opts: &ScanOptions) -> Vec<FoundConsole> {
    discover_detail(opts).await.consoles
}

/// As [`discover`], keeping what answered TCP without speaking RCP.
pub async fn discover_detail(opts: &ScanOptions) -> Scan {
    let nets = local_networks();
    let sweep = scan_targets(&nets, opts.max_hosts);
    let arp = neighbors().await;
    let stages = plan_stages(&arp, &sweep);
    let mut silent: Vec<SocketAddr> = Vec::new();
    tracing::info!(
        neighbors = arp.len(),
        yamaha = stages.first().map_or(0, Vec::len),
        sweep = sweep.len(),
        "yamaha: TF discovery"
    );
    for (stage, targets) in ["yamaha-oui", "neighbors", "sweep"].iter().zip(&stages) {
        if targets.is_empty() {
            continue;
        }
        tracing::debug!(
            stage,
            networks = nets.len(),
            hosts = targets.len(),
            "yamaha: scanning for TF consoles"
        );
        let mut scan = scan_addrs_detail(targets, opts).await;
        if !scan.consoles.is_empty() {
            tracing::info!(stage, hosts = targets.len(), "yamaha: TF console found");
            scan.consoles = with_macs(scan.consoles).await;
            return scan;
        }
        silent.extend(scan.silent);
    }
    silent.sort();
    silent.dedup();
    Scan {
        consoles: Vec::new(),
        silent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(interface: &str, ip: [u8; 4], prefix: u8) -> LocalNet {
        LocalNet {
            interface: interface.into(),
            ip: Ipv4Addr::from(ip),
            prefix,
        }
    }

    #[test]
    fn targets_cover_subnets_without_self_and_dupes() {
        let nets = [
            net("en8", [192, 168, 1, 116], 24),
            net("en10", [10, 10, 10, 107], 24),
            net("en11", [10, 10, 10, 154], 24), // same subnet, second NIC
            net("en12", [172, 30, 15, 122], 30),
            net("utun3", [10, 8, 0, 2], 24),      // tunnel: skipped
            net("lo0", [127, 0, 0, 1], 8),        // loopback: skipped
            net("en0", [192, 168, 0, 5], 16),     // too big: skipped
            net("en1", [203, 0, 113, 5], 24),     // public: skipped
            net("bridge0", [192, 168, 2, 1], 24), // bridge: skipped
        ];
        let t = scan_targets(&nets, 1024);
        // 253 + 252 (two own addresses) + 1 (/30 minus self).
        assert_eq!(t.len(), 253 + 252 + 1);
        assert!(t.contains(&Ipv4Addr::new(192, 168, 1, 214)));
        assert!(!t.contains(&Ipv4Addr::new(192, 168, 1, 116)));
        assert!(!t.contains(&Ipv4Addr::new(192, 168, 1, 0)));
        assert!(!t.contains(&Ipv4Addr::new(192, 168, 1, 255)));
        assert!(!t.contains(&Ipv4Addr::new(10, 10, 10, 154)));
        assert!(t.contains(&Ipv4Addr::new(172, 30, 15, 121)));
        assert!(!t.iter().any(|ip| ip.octets()[0] == 127));
        assert_eq!(scan_targets(&nets, 10).len(), 10);
    }

    const ARP_AN: &str = "\
? (192.168.1.1) at 2e:67:be:f7:97:63 on en8 ifscope [ethernet]
? (192.168.1.2) at (incomplete) on en8 ifscope [ethernet]
? (192.168.1.214) at 0:a0:de:5c:6d:1b on en8 ifscope [ethernet]
? (10.10.10.110) at 0:1d:c1:15:24:34 on en10 ifscope [ethernet]
? (192.168.1.255) at ff:ff:ff:ff:ff:ff on en8 ifscope [ethernet]
? (224.0.0.251) at 1:0:5e:0:0:fb on en8 ifscope permanent [ethernet]
";

    #[test]
    fn arp_an_parses_resolved_unicast_only() {
        let n = parse_arp_an(ARP_AN);
        assert_eq!(n.len(), 3);
        let tf = n
            .iter()
            .find(|n| n.ip == Ipv4Addr::new(192, 168, 1, 214))
            .expect("tf");
        assert_eq!(tf.mac, [0x00, 0xa0, 0xde, 0x5c, 0x6d, 0x1b]);
        assert!(tf.is_yamaha());
        assert_eq!(n.iter().filter(|n| n.is_yamaha()).count(), 1);
    }

    #[test]
    fn proc_net_arp_parses() {
        let text = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.1.214    0x1         0x2         00:a0:de:5c:6d:1b     *        eth0
192.168.1.9      0x1         0x0         00:00:00:00:00:00     *        eth0
192.168.1.1      0x1         0x2         2e:67:be:f7:97:63     *        eth0
";
        let n = parse_proc_net_arp(text);
        assert_eq!(n.len(), 2);
        assert!(
            n.iter()
                .any(|n| n.is_yamaha() && n.ip == Ipv4Addr::new(192, 168, 1, 214))
        );
    }

    #[test]
    fn mac_format_round_trips() {
        let mac = [0x00, 0xa0, 0xde, 0x5c, 0x6d, 0x1b];
        assert_eq!(format_mac(mac), "00:a0:de:5c:6d:1b");
        assert_eq!(parse_mac(&format_mac(mac)), Some(mac));
    }

    #[test]
    fn mac_parsing_edges() {
        assert_eq!(
            parse_mac("0:a0:de:5c:6d:1b"),
            Some([0, 0xa0, 0xde, 0x5c, 0x6d, 0x1b])
        );
        assert_eq!(parse_mac("(incomplete)"), None);
        assert_eq!(parse_mac("0:a0:de:5c:6d"), None);
        assert_eq!(parse_mac("0:a0:de:5c:6d:1b:00"), None);
        assert_eq!(parse_mac("0:a0:de:5c:6d:1bb"), None);
    }

    #[test]
    fn stages_order_yamaha_then_neighbors_then_rest() {
        let nets = [
            net("en8", [192, 168, 1, 116], 24),
            net("en10", [10, 10, 10, 107], 24),
        ];
        let sweep = scan_targets(&nets, 1024);
        let arp = parse_arp_an(ARP_AN);
        let [yamaha, others, rest] = plan_stages(&arp, &sweep);
        assert_eq!(yamaha, vec![Ipv4Addr::new(192, 168, 1, 214)]);
        // 10.10.10.110 is the TF's Dante card (Audinate OUI): never probed.
        let dante_card = Ipv4Addr::new(10, 10, 10, 110);
        assert_eq!(others, vec![Ipv4Addr::new(192, 168, 1, 1)]);
        assert!(!rest.contains(&dante_card));
        assert_eq!(rest.len(), sweep.len() - 3);
        assert!(!rest.contains(&Ipv4Addr::new(192, 168, 1, 214)));
    }

    #[test]
    fn product_filter() {
        assert!(is_tf_product("TF1"));
        assert!(is_tf_product("TF-RACK"));
        assert!(!is_tf_product("CL5"));
        assert!(!is_tf_product(""));
    }

    #[tokio::test]
    async fn probe_reads_product_and_sends_only_devinfo() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let (r, mut w) = sock.split();
            let mut lines = BufReader::new(r).lines();
            let got = lines.next_line().await.expect("read").expect("line");
            w.write_all(b"NOTIFY mtr foo\nOK devinfo productname \"TF1\"\n")
                .await
                .expect("write");
            got
        });
        let product = probe(addr, Duration::from_secs(1), Duration::from_secs(1)).await;
        assert_eq!(product.as_deref(), Some("TF1"));
        assert_eq!(server.await.expect("join"), "devinfo productname");
    }

    #[tokio::test]
    async fn probe_closed_port_is_none() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        assert!(
            probe(addr, Duration::from_millis(300), Duration::from_millis(300))
                .await
                .is_none()
        );
    }
}
