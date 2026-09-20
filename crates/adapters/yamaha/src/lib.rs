//! `patchbay-yamaha` — Yamaha digital mixers for patchbay (TF-series,
//! TF1 first) over **RCP**, the console's text remote-control protocol on
//! TCP 49280.
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
//! - [`TfAdapter`]: the console as a [`patchbay_device::DeviceAdapter`]
//!   (params only — the TF has no routing over RCP).
//! - [`discover_consoles`]: find consoles on the local subnets (RCP has no
//!   announce): a bounded TCP probe of port 49280 confirmed with the
//!   read-only `devinfo productname`.
//!
//! Protocol reference: `docs/yamaha-tf-rcp.md`. The MIT `yamaha-rcp`
//! crate (0.1.0) was consulted for the TF colour names and scene syntax
//! only; it is not a dependency.

mod client;
mod discovery;
mod error;
pub mod rcp;
mod tf;

pub use client::{Client, ClientEvent, ClientOptions, Notification, RCP_PORT, Reply};
pub use discovery::{
    FoundConsole, LocalNet, Neighbor, Probe, Scan, ScanOptions, discover as discover_consoles,
    discover_detail as discover_consoles_detail, find_by_mac,
    format_mac, is_tf_product, local_networks, neighbors, parse_arp_an, parse_mac,
    parse_proc_net_arp, plan_stages, probe as probe_console, scan_addrs, scan_targets,
};
pub use error::{Result, TfError};
pub use rcp::command::{Command, MAX_SCENE, RcpValue, SceneBank};
pub use rcp::reply::{Line, ParamReply, PrmInfo, PrmType, SceneCurrent, SceneInfo};
pub use tf::adapter::{NO_ROUTING, SceneState, TfAdapter, TfOptions, parse_scene};
pub use tf::table::{
    Block, ModelLimits, ParamDef, ParamTable, SceneField, TF1_PRMINFO_JSON, Target, ValueKind,
    canonical_address, embedded_prminfo,
};
pub use tf::values::{MAX_DB, MIN_FINITE_DB, NEG_INF_RAW, db_to_raw, raw_to_db};
pub use tf::vocab::{TF_CATEGORIES, TF_COLORS, TF_ICONS, Vocab, VocabKind};
