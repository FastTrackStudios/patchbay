//! The Yamaha TF family: how the hub finds and connects a TF console
//! (studio#417). `kind = "yamaha-tf"`, one default device, `tf1`,
//! auto-discovered.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use patchbay::family::{
    Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily, DiscoveryCache,
};
use patchbay::proto::{DeviceConfig, FamilyInfo};

pub use crate::KIND;

/// Yamaha TF consoles.
#[derive(Debug, Clone, Copy, Default)]
pub struct YamahaTfFamily;

impl DeviceFamily for YamahaTfFamily {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo::linked(KIND, "Yamaha TF", &[], true)
    }

    fn defaults(&self) -> Vec<DeviceConfig> {
        vec![DeviceConfig::new("tf1", KIND)]
    }

    fn connect(&self, config: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
        yamaha_tf(config, ctx)
    }
}

/// Resolve `host` / `host:port` (default port 49280).
async fn resolve_rcp(raw: &str) -> Result<SocketAddr, ConnectError> {
    let with_port = if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("{raw}:{}", crate::RCP_PORT)
    };
    tokio::net::lookup_host(&with_port)
        .await
        .map_err(|e| ConnectError::Failed(format!("addr '{raw}': {e}")))?
        .next()
        .ok_or_else(|| ConnectError::Failed(format!("addr '{raw}' did not resolve")))
}

/// Don't sweep the subnets more often than this.
///
/// A sweep is up to `max_hosts` TCP connections per local network. Doing
/// that on every reconnect cycle is antisocial on any network, and on a
/// console whose RCP listener is already struggling it is what keeps it
/// down: the TF accepts connections into its backlog and only services
/// them one session at a time, so a repeating sweep can wedge the
/// listener until the desk is restarted.
const SWEEP_EVERY: Duration = Duration::from_secs(300);

/// When the last full discovery sweep ran, per device name.
static LAST_SWEEP: std::sync::LazyLock<parking_lot::Mutex<BTreeMap<String, Instant>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(BTreeMap::new()));

/// What the last sweep concluded, per device — reused while the next
/// sweep is rate-limited, so the reason stays the reason instead of
/// becoming "we haven't looked recently".
static LAST_SCAN_NOTE: std::sync::LazyLock<parking_lot::Mutex<BTreeMap<String, String>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(BTreeMap::new()));

/// Consecutive failed connects against a cached address, per device.
static CACHED_MISSES: std::sync::LazyLock<parking_lot::Mutex<BTreeMap<String, u32>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(BTreeMap::new()));

/// Keep trying a cached address this many times before giving up on it.
///
/// A console that is merely switched off should cost one connection per
/// reconnect cycle and nothing else — going back to discovery on the
/// first failure would put the sweep back in the loop, which is what
/// wedged a live TF's RCP listener in the first place.
const CACHED_MISSES_BEFORE_REDISCOVER: u32 = 5;

/// Record a failed connect; `true` when the cached address should go.
fn cached_miss(name: &str) -> bool {
    let mut misses = CACHED_MISSES.lock();
    let n = misses.entry(name.to_owned()).or_insert(0);
    *n = n.saturating_add(1);
    if *n >= CACHED_MISSES_BEFORE_REDISCOVER {
        misses.remove(name);
        true
    } else {
        false
    }
}

/// Forget the failure history for a device that just connected.
fn cached_hit(name: &str) {
    CACHED_MISSES.lock().remove(name);
}

/// Whether a sweep for `name` is due, recording it if so.
fn sweep_due(name: &str) -> bool {
    let mut last = LAST_SWEEP.lock();
    match last.get(name) {
        Some(t) if t.elapsed() < SWEEP_EVERY => false,
        _ => {
            last.insert(name.to_owned(), Instant::now());
            true
        }
    }
}

/// Find the console, cheapest first:
///
/// 1. the cached address, returned **without probing** — the session
///    that follows is the probe, so a console we already know costs one
///    connection instead of two;
/// 2. the cached MAC looked up in the neighbour table — follows the
///    console across DHCP address changes in one probe;
/// 3. the staged scan (Yamaha-OUI neighbours → other neighbours → full
///    sweep), at most once every [`SWEEP_EVERY`].
///
/// The console's Dante card has its own address and MAC (Audinate
/// OUI); it never speaks RCP and is skipped by the scan.
async fn find_tf(cfg: &DeviceConfig, cache: &DiscoveryCache) -> Result<SocketAddr, ConnectError> {
    let scan = crate::ScanOptions::default();
    if let Some(addr) = cache
        .get(&cfg.name)
        .and_then(|c| c.parse::<SocketAddr>().ok())
    {
        return Ok(addr);
    }
    if let Some(mac) = cache.mac(&cfg.name).and_then(|m| crate::parse_mac(&m))
        && let Some(found) = crate::find_by_mac(mac, &scan).await
    {
        tracing::info!(addr = %found.addr, "yamaha-tf: console moved; re-found by MAC");
        cache.remember(&cfg.name, &found.addr.to_string());
        return Ok(found.addr);
    }
    if !sweep_due(&cfg.name) {
        let note = LAST_SCAN_NOTE.lock().get(&cfg.name).cloned();
        return Err(ConnectError::NotFound(note.unwrap_or_else(|| {
            format!(
                "no TF console found; the subnets are swept at most every {}s",
                SWEEP_EVERY.as_secs()
            )
        })));
    }
    let scanned = crate::discover_consoles_detail(&scan).await;
    let found = scanned.consoles;
    if found.len() > 1 {
        tracing::info!(
            consoles = ?found.iter().map(|f| f.addr).collect::<Vec<_>>(),
            "yamaha-tf: several consoles found; using the first (pin one with `addr`)"
        );
    }
    let first = found.first().ok_or_else(|| {
        let remember = |name: &str, msg: &str| {
            LAST_SCAN_NOTE
                .lock()
                .insert(name.to_owned(), msg.to_owned());
        };
        // A host with the port open that never answers is a different
        // problem from an empty network, and saying "nothing found"
        // sends people hunting a fault that isn't there.
        //
        // Observed on a live TF1: the handshake completes and the
        // console never ACKs the bytes we send — the socket sits with
        // our `devinfo` still in the send queue. The stack accepted the
        // connection; the application never read it. That is what a
        // console whose RCP session is already taken looks like, so
        // name that first.
        if let Some(addr) = scanned.silent.first() {
            let more = scanned.silent.len().saturating_sub(1);
            let others = if more > 0 {
                format!(" (and {more} more)")
            } else {
                String::new()
            };
            let msg = format!(
                "{}{others} accepted a connection on TCP {} and never answered `devinfo` — \
                 another RCP client (TF Editor, StageMix) may be holding the console's \
                 session, or its remote-control listener needs the desk restarted",
                addr.ip(),
                crate::RCP_PORT,
            );
            remember(&cfg.name, &msg);
            return ConnectError::NotFound(msg);
        }
        let msg = format!(
            "no TF console answered on TCP {} on {} local network(s)",
            crate::RCP_PORT,
            crate::local_networks().len()
        );
        remember(&cfg.name, &msg);
        ConnectError::NotFound(msg)
    })?;
    tracing::info!(addr = %first.addr, product = %first.product, "yamaha-tf: console found");
    LAST_SCAN_NOTE.lock().remove(&cfg.name);
    cache.remember(&cfg.name, &first.addr.to_string());
    if let Some(mac) = first.mac {
        cache.remember_mac(&cfg.name, &crate::format_mac(mac));
    }
    Ok(first.addr)
}

/// Yamaha TF over RCP. `addr` pins the console (`host` or `host:port`);
/// without it the console is auto-discovered (see [`find_tf`]). The TF
/// reports no serial, so the device id is
/// `yamaha:<model>:<serial or config name>` — stable across IP changes.
///
/// The session never sends `scpmode` (no keepalive negotiation); link
/// liveness comes from the read-only `devstatus runmode` ping.
fn yamaha_tf(cfg: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
    Box::pin(async move {
        let raw = cfg.addr.trim();
        let addr = if raw.is_empty() {
            find_tf(&cfg, ctx.cache()).await?
        } else {
            resolve_rcp(raw).await?
        };
        let opts = crate::TfOptions {
            device_id: Some(if cfg.serial.is_empty() {
                cfg.name.clone()
            } else {
                cfg.serial.clone()
            }),
            client: crate::ClientOptions {
                keepalive: None,
                ..crate::ClientOptions::default()
            },
            ..crate::TfOptions::default()
        };
        let adapter = match crate::TfAdapter::connect_with(addr, opts).await {
            Ok(a) => a,
            Err(e) => {
                // The cached address is now the only thing we probe, so
                // it is also the only thing that can be wrong. Forget it
                // and let the next attempt discover rather than retrying
                // an address the console has left.
                if raw.is_empty() && cached_miss(&cfg.name) {
                    ctx.cache().forget(&cfg.name);
                }
                return Err(ConnectError::Failed(format!("{addr}: {e}")));
            }
        };
        cached_hit(&cfg.name);
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}
