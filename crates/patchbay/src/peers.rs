//! Other Patchbay engines on the network.
//!
//! Engines don't talk to each other. A UI switches between them by
//! dialling each one itself; all an engine contributes is an address
//! book — the hosts someone saved, plus the ones Bonjour can see — so
//! that the desktop window and a phone attached to the same engine
//! offer the same machines.
//!
//! Discovery is best-effort, like every other network nicety here: if
//! the responder can't start, or the segment blocks multicast, the saved
//! list still works and `discovery_note` says why nothing turns up.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use patchbay_proto::{HostsStatus, PatchbayHost};

/// DNS-SD service type every LAN-reachable engine advertises.
const SERVICE: &str = "_patchbay._tcp.local.";
const DEFAULT_PORT: u16 = 4046;

static PEERS: OnceLock<Peers> = OnceLock::new();

struct Peers {
    /// Kept alive: dropping the daemon withdraws the advertisement.
    _daemon: Option<ServiceDaemon>,
    /// DNS-SD fullname → what it resolved to.
    seen: Arc<Mutex<HashMap<String, Seen>>>,
    note: String,
}

#[derive(Debug, Clone)]
struct Seen {
    name: String,
    addr: String,
    addrs: Vec<String>,
}

/// Start advertising (when `bound` is reachable from other machines) and
/// browsing. Called once by the shell, after it has bound its socket.
pub fn start(bound: &str) {
    let _ = PEERS.get_or_init(|| Peers::start(bound));
}

impl Peers {
    fn start(bound: &str) -> Self {
        let seen = Arc::new(Mutex::new(HashMap::new()));
        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("peer discovery unavailable: {e}");
                return Self {
                    _daemon: None,
                    seen,
                    note: format!("discovery couldn't start ({e}) — add hosts by name"),
                };
            }
        };

        let mut note = String::new();
        let port = crate::net::port_of(bound);
        if crate::net::is_lan(bound) {
            let this = this_name();
            let host = format!("{this}.local.");
            // No addresses given: `enable_addr_auto` publishes whatever
            // interfaces are up, and follows them as they change.
            match ServiceInfo::new(SERVICE, &this, &host, "", port, None) {
                Ok(info) => {
                    if let Err(e) = daemon.register(info.enable_addr_auto()) {
                        tracing::warn!("peer advertise failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("peer advertise failed: {e}"),
            }
        } else {
            "this engine is this-machine-only, so other engines can't discover it \
             (Settings → Network opens it)"
                .clone_into(&mut note);
        }

        match daemon.browse(SERVICE) {
            Ok(rx) => {
                let seen = Arc::clone(&seen);
                let spawned = std::thread::Builder::new()
                    .name("patchbay-peers".into())
                    .spawn(move || {
                        while let Ok(event) = rx.recv() {
                            match event {
                                ServiceEvent::ServiceResolved(info) => {
                                    let host = info.get_hostname().trim_end_matches('.').to_owned();
                                    let port = info.get_port();
                                    let mut addrs: Vec<String> = info
                                        .get_addresses()
                                        .iter()
                                        .filter(|ip| ip.is_ipv4() && !ip.is_loopback())
                                        .map(|ip| format!("{ip}:{port}"))
                                        .collect();
                                    addrs.sort();
                                    seen.lock().insert(
                                        info.get_fullname().to_owned(),
                                        Seen {
                                            name: name_of(&host),
                                            addr: format!("{host}:{port}"),
                                            addrs,
                                        },
                                    );
                                }
                                ServiceEvent::ServiceRemoved(_, fullname) => {
                                    seen.lock().remove(&fullname);
                                }
                                _ => {}
                            }
                        }
                    });
                if let Err(e) = spawned {
                    tracing::warn!("peer browse thread: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("peer browse failed: {e}");
                note = format!("discovery couldn't browse ({e}) — add hosts by name");
            }
        }

        Self {
            _daemon: Some(daemon),
            seen,
            note,
        }
    }
}

/// This machine's short name (`airlock`).
pub fn this_name() -> String {
    crate::net::short_hostname().unwrap_or_else(|| "patchbay".to_owned())
}

/// `thebattleship.local` → `thebattleship`; an IP stays an IP.
fn name_of(host: &str) -> String {
    let host = host.trim_end_matches('.');
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_owned();
    }
    let lower = host.to_ascii_lowercase();
    lower.strip_suffix(".local").unwrap_or(&lower).to_owned()
}

/// What a person types → `host:port`. Accepts a pasted URL
/// (`http://airlock.local:4046/`), a bare name, or `host:port`.
pub fn normalize(input: &str) -> Result<String, String> {
    let mut s = input.trim();
    for scheme in ["http://", "https://", "ws://", "wss://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest;
        }
    }
    let s = s.split(['/', '?', '#']).next().unwrap_or_default().trim();
    if s.is_empty() {
        return Err("host is empty".to_owned());
    }
    if s.chars().any(|c| c.is_whitespace() || c == '@') {
        return Err(format!("`{s}` isn't a host name or address"));
    }
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => match p.parse::<u16>() {
            Ok(port) if port > 0 => (h, port),
            _ => return Err(format!("`{p}` isn't a port")),
        },
        _ => (s, DEFAULT_PORT),
    };
    Ok(format!("{}:{port}", host.to_ascii_lowercase()))
}

fn host_part(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(h, _)| h)
}

/// The address book: `saved` (in order) merged with what is discovered,
/// minus this engine itself.
pub fn status(saved: &[String]) -> HostsStatus {
    let this = this_name();
    let (seen, note): (Vec<Seen>, String) = PEERS.get().map_or_else(
        || (Vec::new(), "discovery hasn't started".to_owned()),
        |p| (p.seen.lock().values().cloned().collect(), p.note.clone()),
    );
    HostsStatus {
        hosts: merge(&this, saved, seen),
        this,
        discovery_note: note,
    }
}

fn merge(this: &str, saved: &[String], mut seen: Vec<Seen>) -> Vec<PatchbayHost> {
    seen.retain(|s| !s.name.eq_ignore_ascii_case(this));
    seen.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<PatchbayHost> = Vec::new();
    for addr in saved {
        let name = name_of(host_part(addr));
        if name.eq_ignore_ascii_case(this) {
            continue;
        }
        // The same machine seen on the network: one entry, with the
        // addresses discovery found for it.
        let found = seen.iter().position(|s| s.name.eq_ignore_ascii_case(&name));
        let addrs = found.map(|i| seen.remove(i).addrs).unwrap_or_default();
        out.push(PatchbayHost {
            name,
            addr: addr.clone(),
            addrs,
            saved: true,
            discovered: found.is_some(),
        });
    }
    out.extend(seen.into_iter().map(|s| PatchbayHost {
        name: s.name,
        addr: s.addr,
        addrs: s.addrs,
        saved: false,
        discovered: true,
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_people_type_becomes_host_and_port() {
        assert_eq!(
            normalize("thebattleship.local").as_deref(),
            Ok("thebattleship.local:4046")
        );
        assert_eq!(
            normalize(" Voyager.local:5000 ").as_deref(),
            Ok("voyager.local:5000")
        );
        // A URL pasted from Settings → Network works as it is.
        assert_eq!(
            normalize("http://192.168.0.65:4046/").as_deref(),
            Ok("192.168.0.65:4046")
        );
        assert_eq!(
            normalize("ws://airlock.local:4046/vox").as_deref(),
            Ok("airlock.local:4046")
        );
        assert!(normalize("").is_err());
        assert!(normalize("host:notaport").is_err());
        assert!(normalize("two words").is_err());
    }

    fn seen(name: &str, ip: &str) -> Seen {
        Seen {
            name: name.to_owned(),
            addr: format!("{name}.local:4046"),
            addrs: vec![format!("{ip}:4046")],
        }
    }

    #[test]
    fn a_saved_host_that_is_also_discovered_is_one_entry() {
        let saved = vec![
            "thebattleship.local:4046".to_owned(),
            "10.0.0.9:4046".to_owned(),
        ];
        let hosts = merge(
            "airlock",
            &saved,
            vec![
                seen("voyager", "192.168.0.7"),
                seen("thebattleship", "192.168.0.3"),
                seen("airlock", "192.168.0.65"),
            ],
        );
        let names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();
        // Saved first in the order added, then discovered; never ourselves.
        assert_eq!(names, ["thebattleship", "10.0.0.9", "voyager"]);
        assert!(hosts[0].saved && hosts[0].discovered);
        assert_eq!(hosts[0].addrs, ["192.168.0.3:4046"]);
        assert!(hosts[1].saved && !hosts[1].discovered);
        assert!(!hosts[2].saved && hosts[2].discovered);
    }

    #[test]
    fn saving_yourself_does_not_list_yourself() {
        let hosts = merge("airlock", &["Airlock.local:4046".to_owned()], Vec::new());
        assert!(hosts.is_empty());
    }
}
