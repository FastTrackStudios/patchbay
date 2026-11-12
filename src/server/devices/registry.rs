//! The built-in device families ([`builtin`]): one [`DeviceFamily`] per
//! config `kind`, each a thin call into its adapter's own
//! discovery/connect. Everything else (supervision, reconnect, events,
//! RPC, CLI, UI, snapshots) is the hub's, generic over
//! [`crate::device::DynDeviceAdapter`]. Each family becomes an extension
//! of its own in studio#410's stack.
//!
//! Every connect function **fails soft**: it returns a
//! [`ConnectError`] (never panics, never blocks the others), and
//! distinguishes "discovery found nothing" ([`ConnectError::NotFound`])
//! from "found / pinned but unreachable" ([`ConnectError::Failed`]) so
//! `device list` can say *not found* vs *offline*.

use std::sync::Arc;

use crate::family::{Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily, Families};
use crate::proto::{DeviceConfig, FamilyInfo};
use parking_lot::RwLock;

use super::cache::DiscoveryCache;
use crate::server::store::GraphStore;

/// The families compiled into the engine, in the order of the default
/// device list (each auto-discovered). `graph` is the `PipeWire` graph
/// mirror, system audio's source off macOS. The others are extensions
/// that register themselves (`crate::family::register`): the Yamaha TF
/// is `patchbay-yamaha-tf`, Antelope `patchbay-antelope`.
///
/// On a phone or a tablet, none: it has no device families of its own (no
/// `PipeWire`, no Core Audio routing, no driver) and scans the LAN for no
/// console or interface. Patchbay there is a controller of the engines on
/// other machines (`patchbay-ios`, studio#466). The same gate keeps out
/// every registered family (`crate::family::assemble`).
pub(crate) fn builtin(graph: Arc<RwLock<GraphStore>>) -> Families {
    if cfg!(target_os = "ios") {
        drop(graph);
        return Families::new();
    }
    Families::new().with(SystemAudio { graph }).with(Dante)
}

/// Why a configured device has no family to connect it.
///
/// On a host that runs no families (a phone or tablet, `families_here`
/// false) no kind connects here: it does on the machine that runs it.
/// Elsewhere the kind's family is an extension this app doesn't have (or
/// a kind nobody knows). The core keeps no list of extensions to name the
/// missing one: adding a family is a registration, and a manifest that
/// isn't installed can't be read.
pub(crate) fn no_family(kind: &str, known: &[&str], families_here: bool) -> String {
    if families_here {
        format!(
            "no device family for kind '{kind}' in this app: the extension that adds it is not \
             installed (this app has: {})",
            known.join(", ")
        )
    } else {
        format!(
            "this device ({kind}) connects on the machine whose engine runs it: no device \
             family runs on a phone or tablet"
        )
    }
}

/// A linked family's [`FamilyInfo`].
fn linked(kind: &str, label: &str, platforms: &[&str], discovers: bool) -> FamilyInfo {
    FamilyInfo::linked(kind, label, platforms, discovers)
}

/// This machine's own audio system, read only.
struct SystemAudio {
    graph: Arc<RwLock<GraphStore>>,
}

impl DeviceFamily for SystemAudio {
    fn kind(&self) -> &'static str {
        "system-audio"
    }
    fn info(&self) -> FamilyInfo {
        linked(self.kind(), "System audio", &["linux", "macos"], false)
    }
    fn defaults(&self) -> Vec<DeviceConfig> {
        vec![DeviceConfig::new("system-audio", self.kind())]
    }
    fn connect(&self, config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        system_audio(config, Arc::clone(&self.graph))
    }
}

/// The Dante network.
struct Dante;

impl DeviceFamily for Dante {
    fn kind(&self) -> &'static str {
        "dante"
    }
    fn info(&self) -> FamilyInfo {
        linked(self.kind(), "Dante network", &[], true)
    }
    fn defaults(&self) -> Vec<DeviceConfig> {
        vec![DeviceConfig::new("dante", self.kind())]
    }
    fn connect(&self, config: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
        dante(config, ctx)
    }
}

// ── System audio ─────────────────────────────────────────────────────

/// The host's own audio system, read-only (Core Audio on macOS, the
/// `PipeWire` engine's graph elsewhere).
fn system_audio(cfg: DeviceConfig, graph: Arc<RwLock<GraphStore>>) -> ConnectFuture {
    Box::pin(async move {
        #[cfg(target_os = "macos")]
        let adapter = {
            let _ = &graph;
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
        let adapter = super::system_audio::SystemAudioAdapter::pipewire(&cfg.name, graph);
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
        let opts = crate::dante::DanteOptions {
            device_id: crate::device::DeviceId::from_parts("dante", "network", &cfg.name)
                .to_string(),
            ..crate::dante::DanteOptions::default()
        };
        let control: Arc<dyn crate::dante::DanteControl> =
            Arc::new(crate::dante::InfernoControl::default());
        // Last known members first: online in one ARC round trip instead
        // of an 8 s mDNS browse; the browse then runs in the background.
        let seeds: Vec<crate::dante::Endpoint> = ctx
            .cache
            .get_members(&cfg.name)
            .into_iter()
            .filter_map(|(name, addr)| {
                addr.parse()
                    .ok()
                    .map(|addr| crate::dante::Endpoint { name, addr })
            })
            .collect();
        let adapter = crate::dante::DanteNetworkAdapter::connect_seeded(control, opts, seeds)
            .await
            .map_err(|e| match e {
                crate::dante::DanteError::Empty => ConnectError::NotFound(e.to_string()),
                other => ConnectError::Failed(other.to_string()),
            })?;
        remember_members(&ctx.cache, &cfg.name, &adapter);
        // Keep the cache current as the background browse finds or drops
        // devices.
        let (cache, name, watched) = (Arc::clone(&ctx.cache), cfg.name.clone(), adapter.clone());
        let mut rx = crate::device::DeviceAdapter::subscribe(&adapter);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(crate::device::DeviceEvent::SnapshotReplaced) => {
                        remember_members(&cache, &name, &watched);
                    }
                    Ok(crate::device::DeviceEvent::Offline)
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
    adapter: &crate::dante::DanteNetworkAdapter,
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

    fn families() -> Families {
        builtin(Arc::new(RwLock::new(GraphStore::default())))
    }

    #[test]
    fn defaults_cover_every_kind_once() {
        let families = families();
        let d = families.defaults();
        assert_eq!(d.len(), families.kinds().len());
        for k in families.kinds() {
            assert_eq!(d.iter().filter(|c| c.kind == k).count(), 1, "{k}");
        }
        assert!(d.iter().all(DeviceConfig::is_enabled));
    }

    /// The built-ins' default devices, in the order they had before the
    /// families (names and kinds as `KINDS`/`default_devices` gave
    /// them). The Yamaha TF's `tf1` is its extension's now, and comes
    /// after these when it is registered (`patchbay-yamaha-tf`'s tests).
    #[test]
    fn the_builtin_default_devices_keep_their_order() {
        let d: Vec<(String, String)> = families()
            .defaults()
            .into_iter()
            .map(|c| (c.name, c.kind))
            .collect();
        let want = [("system-audio", "system-audio"), ("dante", "dante")];
        assert_eq!(d, want.map(|(n, k)| (n.to_owned(), k.to_owned())).to_vec());
    }

    #[test]
    fn a_kind_from_an_extension_that_isnt_installed_says_so() {
        let known = families().kinds();
        let missing = no_family("antelope-galaxy32", &known, true);
        assert!(
            missing.starts_with(
                "no device family for kind 'antelope-galaxy32' in this app: the extension that \
                 adds it is not installed (this app has: system-audio, "
            ),
            "{missing}"
        );
        // On a phone no kind is missing an extension: none runs there.
        let phone = no_family("yamaha-tf", &[], false);
        assert!(!phone.contains("not installed"), "{phone}");
        assert!(phone.contains("phone or tablet"), "{phone}");
    }

    /// The built-ins all bring views: an engine with only them warns of
    /// nothing at start.
    #[cfg(feature = "ui")]
    #[test]
    fn the_builtins_have_views() {
        let kinds = families().kinds();
        let views = crate::ui::family_view_kinds();
        for k in &kinds {
            assert!(views.contains(k), "the built-in family '{k}' has no views");
        }
    }

    #[test]
    fn families_say_where_they_run() {
        let infos = families().infos();
        let system = infos.iter().find(|f| f.kind == "system-audio").unwrap();
        assert_eq!(system.platforms, ["linux", "macos"]);
        assert_eq!(
            system.available,
            cfg!(any(target_os = "linux", target_os = "macos"))
        );
        for net in ["dante"] {
            let f = infos.iter().find(|f| f.kind == net).unwrap();
            assert!(
                f.platforms.is_empty() && f.available && f.discovers,
                "{net}"
            );
        }
        assert!(infos.iter().all(|f| f.origin == "linked"));
    }
}
