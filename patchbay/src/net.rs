//! What this process is actually serving, and where to reach it.
//!
//! The shell binds the socket and decides whether it has a browser
//! bundle to serve, so it records both here at startup; the service
//! reports them (and the addresses another machine would use) through
//! `listen_address`.

use std::net::SocketAddr;
use std::sync::OnceLock;

/// What the running process bound, recorded by the shell.
static BOUND: OnceLock<Bound> = OnceLock::new();

#[derive(Debug, Clone)]
struct Bound {
    addr: String,
    /// Empty when the browser remote is served; otherwise why it isn't.
    web_note: String,
}

/// Record what this process bound and whether it serves the browser
/// remote. Called once by the shell; later calls are ignored.
pub fn record_bound(addr: &str, web_note: &str) {
    let _ = BOUND.set(Bound {
        addr: addr.to_owned(),
        web_note: web_note.to_owned(),
    });
}

/// Whether `addr` is reachable from another machine.
pub(crate) fn is_lan(addr: &str) -> bool {
    addr.parse::<SocketAddr>()
        .is_ok_and(|a| !a.ip().is_loopback())
}

pub(crate) fn port_of(addr: &str) -> u16 {
    addr.parse::<SocketAddr>().map_or(4046, |a| a.port())
}

/// This machine's Bonjour hostname (`airlock.local`), if it has one.
fn local_hostname() -> Option<String> {
    let host = hostname()?;
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    // A suffix check, not a file extension: `hostname -s` may already
    // carry the mDNS suffix, and appending a second one is wrong.
    let already = host
        .rsplit('.')
        .next()
        .is_some_and(|s| s.eq_ignore_ascii_case("local"));
    Some(if already {
        host.to_owned()
    } else {
        format!("{host}.local")
    })
}

/// This machine's short name (`airlock`), without any domain.
pub(crate) fn short_hostname() -> Option<String> {
    let host = hostname()?;
    let short = host
        .trim()
        .trim_end_matches('.')
        .split('.')
        .next()?
        .to_ascii_lowercase();
    (!short.is_empty()).then_some(short)
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
}

/// Non-loopback IPv4 addresses that are up right now.
fn local_ips() -> Vec<String> {
    let Ok(out) = std::process::Command::new("ifconfig").output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("inet ")?;
            let ip = rest.split_whitespace().next()?;
            (!ip.starts_with("127.")).then(|| ip.to_owned())
        })
        .collect()
}

/// Where the RPC and the browser remote listen, and how to reach them.
#[must_use]
pub fn listen_address(configured: String) -> patchbay_proto::ListenAddress {
    let bound = BOUND.get();
    let current = bound.map_or_else(String::new, |b| b.addr.clone());
    let lan = is_lan(&current);
    let port = port_of(&current);
    // Only advertise addresses that would actually answer: bound to
    // loopback, the LAN ones would just fail for whoever tried them.
    let urls = if lan {
        local_hostname()
            .into_iter()
            .chain(local_ips())
            .map(|h| format!("http://{h}:{port}/"))
            .collect()
    } else {
        vec![format!("http://127.0.0.1:{port}/")]
    };
    patchbay_proto::ListenAddress {
        configured: if configured.trim().is_empty() {
            current.clone()
        } else {
            configured
        },
        current,
        lan,
        urls,
        web_note: bound.map_or_else(String::new, |b| b.web_note.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::is_lan;

    #[test]
    fn loopback_is_not_the_lan() {
        assert!(!is_lan("127.0.0.1:4046"));
        assert!(!is_lan("[::1]:4046"));
        assert!(is_lan("0.0.0.0:4046"));
        assert!(is_lan("192.168.0.65:4046"));
        // Not an address at all: nothing to advertise.
        assert!(!is_lan(""));
    }
}
