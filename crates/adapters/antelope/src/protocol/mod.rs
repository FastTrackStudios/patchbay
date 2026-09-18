//! Antelope Manager Server wire protocol (JSON over TCP).
//!
//! - [`framing`]: 4-byte big-endian length (inclusive of the header) +
//!   UTF-8 JSON body.
//! - [`envelope`]: server → client tagged objects (`cyclic`, `single`,
//!   `notification`).
//! - [`call`]: client → server `["method", [args], {kwargs}]` arrays.

pub(crate) mod call;
pub(crate) mod envelope;
pub(crate) mod framing;

/// The captured `initialize_format` frame body the official panel sends
/// first on every control connection. The server needs it before it
/// answers `get_*` requests; we replay it verbatim.
pub(crate) const INITIALIZE_FORMAT: &[u8] =
    include_bytes!("../../assets/client_initialize_format.json");

/// The server's RPC schema (method → report ids, field layouts), as
/// dumped from Manager Server 1.8.19. Drives the generic AFX encoder.
pub(crate) const RPC_SCHEMA: &str = include_str!("../../assets/rpc_schema_full.json");
