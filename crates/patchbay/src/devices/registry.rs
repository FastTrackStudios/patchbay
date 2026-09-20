//! Adapter registry: config `kind` → connect function.
//!
//! Adding a hardware family is one entry in [`KINDS`] plus its connect
//! function (usually a thin call into the adapter crate's own
//! discovery/connect). Everything else — supervision, reconnect,
//! events, RPC, CLI, UI, snapshots — is generic over
//! [`DynDeviceAdapter`].
//!
//! Every connect function **fails soft**: it returns a
//! [`ConnectError`] (never panics, never blocks the others), and
//! distinguishes "discovery found nothing" ([`ConnectError::NotFound`])
//! from "found / pinned but unreachable" ([`ConnectError::Failed`]) so
//! `device list` can say *not found* vs *offline*.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use patchbay_device::DynDeviceAdapter;
use patchbay_proto::DeviceConfig;

use super::cache::DiscoveryCache;
use crate::store::GraphStore;

/// A connected adapter, type-erased.
pub(crate) type Adapter = Arc<dyn DynDeviceAdapter>;

/// Why a connect attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectError {
    /// Auto-discovery ran and found nothing.
    NotFound(String),
    /// Found (or pinned) but the connection failed.
    Failed(String),
}

impl ConnectError {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::NotFound(m) | Self::Failed(m) => m,
        }
    }
}

/// What a connect function returns.
pub(crate) type ConnectFuture = Pin<Box<dyn Future<Output = Result<Adapter, ConnectError>> + Send>>;

/// Shared context handed to connect functions.
#[derive(Clone)]
pub(crate) struct ConnectCtx {
    /// Last-found addresses of auto-discovered devices.
    pub cache: Arc<DiscoveryCache>,
    /// The `PipeWire` graph mirror (system audio off macOS).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub graph: Arc<RwLock<GraphStore>>,
}

/// One adapter family the hub can start from config.
pub(crate) struct AdapterKind {
    /// Config `kind` string.
    pub kind: &'static str,
    /// Finds the device itself when `addr` is empty (initial state
    /// *searching* rather than *connecting*).
    pub discovers: bool,
    /// Connect (discover if needed) using the config entry.
    pub connect: fn(DeviceConfig, ConnectCtx) -> ConnectFuture,
}

/// Every supported `kind`. One line per family.
pub(crate) const KINDS: &[AdapterKind] = &[
    AdapterKind {
        kind: "system-audio",
        discovers: false,
        connect: system_audio,
    },
    AdapterKind {
        kind: "antelope-galaxy32",
        discovers: true,
        connect: antelope_galaxy32,
    },
    AdapterKind {
        kind: "yamaha-tf",
        discovers: true,
        connect: yamaha_tf,
    },
    AdapterKind {
        kind: "dante",
        discovers: true,
        connect: dante,
    },
];

/// The entries used when the config has no `devices` section: all four
/// families, each auto-discovered.
pub(crate) fn default_devices() -> Vec<DeviceConfig> {
    vec![
        DeviceConfig::new("system-audio", "system-audio"),
        DeviceConfig::new("galaxy32", "antelope-galaxy32"),
        DeviceConfig::new("tf1", "yamaha-tf"),
        DeviceConfig::new("dante", "dante"),
    ]
}

pub(crate) fn find(kind: &str) -> Option<&'static AdapterKind> {
    KINDS.iter().find(|k| k.kind == kind)
}

// ── System audio ─────────────────────────────────────────────────────

/// The host's own audio system, read-only (Core Audio on macOS, the
/// `PipeWire` engine's graph elsewhere).
fn system_audio(cfg: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
    Box::pin(async move {
        #[cfg(target_os = "macos")]
        let adapter = {
            let _ = &ctx;
            let name = cfg.name.clone();
            // HAL enumeration is synchronous; keep it off the runtime.
            tokio::task::spawn_blocking(move || {
                super::system_audio::SystemAudioAdapter::coreaudio(&name)
            })
            .await
            .map_err(|e| ConnectError::Failed(e.to_string()))?
            .map_err(ConnectError::Failed)?
        };
        #[cfg(not(target_os = "macos"))]
        let adapter = super::system_audio::SystemAudioAdapter::pipewire(&cfg.name, ctx.graph);
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}

// ── Antelope ─────────────────────────────────────────────────────────

/// How long Antelope discovery listens for multicast announces.
const ANTELOPE_DISCOVERY: Duration = Duration::from_secs(2);

fn antelope_error(e: &patchbay_antelope::AntelopeError) -> ConnectError {
    match e {
        patchbay_antelope::AntelopeError::NotFound(_) => ConnectError::NotFound(e.to_string()),
        _ => ConnectError::Failed(e.to_string()),
    }
}

fn antelope_galaxy32(cfg: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
    Box::pin(async move {
        let serial = (!cfg.serial.is_empty()).then_some(cfg.serial.as_str());
        let adapter = if cfg.addr.is_empty() {
            patchbay_antelope::Galaxy32Adapter::discover_and_connect(serial, ANTELOPE_DISCOVERY)
                .await
                .map_err(|e| antelope_error(&e))?
        } else {
            // Pinned endpoint: identity still comes from the announce,
            // so discovery runs and we pick the matching one.
            let addr: SocketAddr = cfg
                .addr
                .parse()
                .map_err(|e| ConnectError::Failed(format!("addr '{}': {e}", cfg.addr)))?;
            let announces = patchbay_antelope::discover(ANTELOPE_DISCOVERY)
                .await
                .map_err(|e| antelope_error(&e))?;
            let announce = announces
                .iter()
                .find(|a| {
                    a.is_control()
                        && a.socket_addr() == Some(addr)
                        && serial.is_none_or(|s| a.serial() == Some(s))
                })
                .ok_or_else(|| {
                    ConnectError::NotFound(format!(
                        "no Antelope control endpoint announced at {addr}"
                    ))
                })?;
            patchbay_antelope::Galaxy32Adapter::connect(addr, announce)
                .await
                .map_err(|e| antelope_error(&e))?
        };
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}

// ── Yamaha TF ────────────────────────────────────────────────────────

/// Resolve `host` / `host:port` (default port 49280).
async fn resolve_rcp(raw: &str) -> Result<SocketAddr, ConnectError> {
    let with_port = if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("{raw}:{}", patchbay_yamaha::RCP_PORT)
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
    let scan = patchbay_yamaha::ScanOptions::default();
    if let Some(addr) = cache
        .get(&cfg.name)
        .and_then(|c| c.parse::<SocketAddr>().ok())
    {
        return Ok(addr);
    }
    if let Some(mac) = cache
        .get_mac(&cfg.name)
        .and_then(|m| patchbay_yamaha::parse_mac(&m))
        && let Some(found) = patchbay_yamaha::find_by_mac(mac, &scan).await
    {
        tracing::info!(addr = %found.addr, "yamaha-tf: console moved; re-found by MAC");
        cache.set(&cfg.name, &found.addr.to_string());
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
    let scanned = patchbay_yamaha::discover_consoles_detail(&scan).await;
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
                patchbay_yamaha::RCP_PORT,
            );
            remember(&cfg.name, &msg);
            return ConnectError::NotFound(msg);
        }
        let msg = format!(
            "no TF console answered on TCP {} on {} local network(s)",
            patchbay_yamaha::RCP_PORT,
            patchbay_yamaha::local_networks().len()
        );
        remember(&cfg.name, &msg);
        ConnectError::NotFound(msg)
    })?;
    tracing::info!(addr = %first.addr, product = %first.product, "yamaha-tf: console found");
    LAST_SCAN_NOTE.lock().remove(&cfg.name);
    cache.set(&cfg.name, &first.addr.to_string());
    if let Some(mac) = first.mac {
        cache.set_mac(&cfg.name, &patchbay_yamaha::format_mac(mac));
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
            find_tf(&cfg, &ctx.cache).await?
        } else {
            resolve_rcp(raw).await?
        };
        let opts = patchbay_yamaha::TfOptions {
            device_id: Some(if cfg.serial.is_empty() {
                cfg.name.clone()
            } else {
                cfg.serial.clone()
            }),
            client: patchbay_yamaha::ClientOptions {
                keepalive: None,
                ..patchbay_yamaha::ClientOptions::default()
            },
            ..patchbay_yamaha::TfOptions::default()
        };
        let adapter = match patchbay_yamaha::TfAdapter::connect_with(addr, opts).await {
            Ok(a) => a,
            Err(e) => {
                // The cached address is now the only thing we probe, so
                // it is also the only thing that can be wrong. Forget it
                // and let the next attempt discover rather than retrying
                // an address the console has left.
                if raw.is_empty() && cached_miss(&cfg.name) {
                    ctx.cache.forget_addr(&cfg.name);
                }
                return Err(ConnectError::Failed(format!("{addr}: {e}")));
            }
        };
        cached_hit(&cfg.name);
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}

// ── Dante ────────────────────────────────────────────────────────────

/// The Dante network (mDNS + ARC) as one device, id
/// `dante:network:<config name>`. Connect only browses; nothing is
/// written.
fn dante(cfg: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
    Box::pin(async move {
        let opts = patchbay_dante::DanteOptions {
            device_id: patchbay_device::DeviceId::from_parts("dante", "network", &cfg.name)
                .to_string(),
            ..patchbay_dante::DanteOptions::default()
        };
        let control: Arc<dyn patchbay_dante::DanteControl> =
            Arc::new(patchbay_dante::InfernoControl::default());
        // Last known members first: online in one ARC round trip instead
        // of an 8 s mDNS browse; the browse then runs in the background.
        let seeds: Vec<patchbay_dante::Endpoint> = ctx
            .cache
            .get_members(&cfg.name)
            .into_iter()
            .filter_map(|(name, addr)| {
                addr.parse()
                    .ok()
                    .map(|addr| patchbay_dante::Endpoint { name, addr })
            })
            .collect();
        let adapter = patchbay_dante::DanteNetworkAdapter::connect_seeded(control, opts, seeds)
            .await
            .map_err(|e| match e {
                patchbay_dante::DanteError::Empty => ConnectError::NotFound(e.to_string()),
                other => ConnectError::Failed(other.to_string()),
            })?;
        remember_members(&ctx.cache, &cfg.name, &adapter);
        // Keep the cache current as the background browse finds or drops
        // devices.
        let (cache, name, watched) = (Arc::clone(&ctx.cache), cfg.name.clone(), adapter.clone());
        let mut rx = patchbay_device::DeviceAdapter::subscribe(&adapter);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(patchbay_device::DeviceEvent::SnapshotReplaced) => {
                        remember_members(&cache, &name, &watched);
                    }
                    Ok(patchbay_device::DeviceEvent::Offline)
                    | Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        });
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}

fn remember_members(
    cache: &DiscoveryCache,
    name: &str,
    adapter: &patchbay_dante::DanteNetworkAdapter,
) {
    let members = adapter
        .endpoints()
        .into_iter()
        .map(|e| (e.name, e.addr.to_string()))
        .collect();
    cache.set_members(name, members);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_kind_once() {
        let d = default_devices();
        assert_eq!(d.len(), KINDS.len());
        for k in KINDS {
            assert_eq!(
                d.iter().filter(|c| c.kind == k.kind).count(),
                1,
                "{}",
                k.kind
            );
        }
        assert!(d.iter().all(DeviceConfig::is_enabled));
    }
}
