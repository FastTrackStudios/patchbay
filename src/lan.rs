//! `PATCHBAY_LAN=off`: this process sends nothing onto the local network.
//!
//! Tests and scratch engines set it (with `PATCHBAY_PIPEWIRE=off` and a
//! scratch `PATCHBAY_CONFIG`). Off, every place that would reach the LAN
//! refuses with [`OFF`] instead, at its lowest point, so nothing above
//! can get around it:
//!
//! - peer discovery: no mDNS daemon advertising or browsing
//!   `_patchbay._tcp` (`EngineOptions` starts with `discover_peers` off,
//!   and `server::peers` refuses on its own);
//! - the Dante network: no mDNS browse and no ARC request to any device
//!   (`dante::InfernoControl` refuses every call);
//! - the Antelope Manager Server's announces: no UDP bind, no multicast
//!   join (`antelope::Listener::bind` refuses);
//! - the Yamaha TF: no subnet sweep and no neighbour-table probe
//!   (`patchbay-yamaha-tf`'s discovery returns nothing).
//!
//! A device pinned to an address is still connected: that is a target
//! the config names (in tests, a fake on loopback), not a search.

/// What a refused LAN operation says.
pub const OFF: &str = "PATCHBAY_LAN=off: this process does not touch the local network";

/// Whether this process may reach the local network: `PATCHBAY_LAN=off`
/// (or `0`, `false`) says no.
#[must_use]
pub fn enabled() -> bool {
    !std::env::var("PATCHBAY_LAN").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false"
        )
    })
}
