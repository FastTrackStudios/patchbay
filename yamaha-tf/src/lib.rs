//! `patchbay-yamaha-tf` — Yamaha TF consoles for Patchbay (TF1 first)
//! over **RCP**, the console's text remote-control protocol on TCP 49280:
//! a device family of its own, and its own Architect extension
//! (FastTrackStudios/studio#410, #417).
//!
//! - [`register`]: add the family and its console view to this process,
//!   before the engine and the window start. [`ext::YamahaTf`] (feature
//!   `extension`) does it from its `build`; an app that is not an
//!   Architect host (`fts-patchbay`, its CLI, the browser remote) calls it
//!   itself.
//! - [`family`] (feature `server`): the `DeviceFamily` — finding a console
//!   (cache, MAC, staged scan) and connecting it.
//! - [`ui`] (feature `ui`): the console view, by bank.
//!
//! The protocol (feature `server`):
//!
//! - [`rcp`]: the pure wire layer — LF line codec, quote/escape-aware
//!   tokenizer, command encoding, `OK`/`OKm`/`NOTIFY`/`ERROR`/`prminfo`
//!   parsing.
//! - [`Client`]: one connection, a reader task, in-order request
//!   correlation with timeouts, a broadcast of `NOTIFY` lines, bounded
//!   pipelining, write spacing, keepalive and reconnect.
//! - [`ParamTable`]: `prminfo` rows → generic paths
//!   (`in/3/send/aux/2/level`, `dca/1/name`, …), clamped to the model's
//!   physical channel counts ([`ModelLimits::TF1`]).
//! - [`TfAdapter`]: the console as a [`patchbay::device::DeviceAdapter`]
//!   (params only — the TF has no routing over RCP).
//! - [`discover_consoles`]: find consoles on the local subnets (RCP has no
//!   announce): a bounded TCP probe of port 49280 confirmed with the
//!   read-only `devinfo productname`.
//!
//! Protocol reference: `docs/patchbay/yamaha-tf-rcp.md`. The MIT `yamaha-rcp`
//! crate (0.1.0) was consulted for the TF colour names and scene syntax
//! only; it is not a dependency.

#[cfg(feature = "server")]
mod client;
#[cfg(feature = "server")]
mod discovery;
#[cfg(feature = "server")]
mod error;
#[cfg(feature = "extension")]
pub mod ext;
#[cfg(feature = "server")]
pub mod family;
#[cfg(feature = "server")]
pub mod rcp;
#[cfg(feature = "server")]
mod tf;
#[cfg(feature = "ui")]
pub mod ui;

#[cfg(feature = "server")]
pub use client::{Client, ClientEvent, ClientOptions, Notification, RCP_PORT, Reply};
#[cfg(feature = "server")]
pub use discovery::{
    FoundConsole, LocalNet, Neighbor, Probe, Scan, ScanOptions, discover as discover_consoles,
    discover_detail as discover_consoles_detail, find_by_mac, format_mac, is_tf_product,
    local_networks, neighbors, parse_arp_an, parse_mac, parse_proc_net_arp, plan_stages,
    probe as probe_console, scan_addrs, scan_targets,
};
#[cfg(feature = "server")]
pub use error::{Result, TfError};
#[cfg(feature = "server")]
pub use rcp::command::{Command, MAX_SCENE, RcpValue, SceneBank};
#[cfg(feature = "server")]
pub use rcp::reply::{Line, ParamReply, PrmInfo, PrmType, SceneCurrent, SceneInfo};
#[cfg(feature = "server")]
pub use tf::adapter::{NO_ROUTING, SceneState, TfAdapter, TfOptions, parse_scene};
#[cfg(feature = "server")]
pub use tf::table::{
    Block, ModelLimits, ParamDef, ParamTable, SceneField, TF1_PRMINFO_JSON, Target, ValueKind,
    canonical_address, embedded_prminfo,
};
#[cfg(feature = "server")]
pub use tf::values::{MAX_DB, MIN_FINITE_DB, NEG_INF_RAW, db_to_raw, raw_to_db};
#[cfg(feature = "server")]
pub use tf::vocab::{TF_CATEGORIES, TF_COLORS, TF_ICONS, Vocab, VocabKind};

/// Add the Yamaha TF to this process, before the engine and the window
/// start.
///
/// Its device family goes to every engine started from now on (feature
/// `server`), its console view to every window (feature `ui`). Once is
/// enough; later calls do nothing, so the extension's `build` (run more
/// than once by a host) and an app's `main` can both call it.
pub fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(all(feature = "server", feature = "ui"))]
        patchbay::family::register_with_views(
            std::sync::Arc::new(family::YamahaTfFamily),
            ui::views(),
        );
        #[cfg(all(feature = "server", not(feature = "ui")))]
        let _ = patchbay::family::register(std::sync::Arc::new(family::YamahaTfFamily));
        #[cfg(all(feature = "ui", not(feature = "server")))]
        patchbay::ui::register_family_views(ui::views());
    });
}
