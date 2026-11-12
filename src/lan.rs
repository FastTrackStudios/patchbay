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
//!   join (`patchbay-antelope`'s `Listener::bind` refuses);
//! - the Yamaha TF: no subnet sweep and no neighbour-table probe
//!   (`patchbay-yamaha-tf`'s discovery returns nothing).
//!
//! A device pinned to an address is still connected: that is a target
//! the config names (in tests, a fake on loopback), not a search.

/// What a refused LAN operation says.
pub const OFF: &str = "PATCHBAY_LAN=off: this process does not touch the local network";

/// Whether this process may reach the local network.
///
/// `PATCHBAY_LAN=off` (or `0`, `false`) says no, `on` (`1`, `true`) yes.
/// Unset, it is on, except under a test runner: a process nextest runs
/// (`NEXTEST_RUN_ID`) is off unless it says `on`, so a test that forgets
/// the switch still sends nothing. A deliberate live test sets `on`.
#[must_use]
pub fn enabled() -> bool {
    decide(
        std::env::var("PATCHBAY_LAN").ok().as_deref(),
        std::env::var_os("NEXTEST_RUN_ID").is_some(),
    )
}

/// [`enabled`] from the variable's value and whether a test runner
/// started this process.
fn decide(var: Option<&str>, under_test_runner: bool) -> bool {
    match var.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "off" | "0" | "false") => false,
        Some(v) if matches!(v.as_str(), "on" | "1" | "true") => true,
        _ => !under_test_runner,
    }
}

#[cfg(test)]
mod tests {
    use super::decide;

    #[test]
    fn a_test_runner_keeps_the_lan_off_unless_told_otherwise() {
        // Unset: on for an app, off under a test runner.
        assert!(decide(None, false));
        assert!(!decide(None, true));
        // Off says off everywhere; on is the deliberate live run.
        assert!(!decide(Some("off"), false));
        assert!(!decide(Some("0"), true));
        assert!(decide(Some("on"), true));
        // Anything else reads as unset.
        assert!(!decide(Some("maybe"), true));
    }

    /// This very run: nextest, the variable unset or off.
    #[test]
    fn this_test_run_is_lan_off() {
        if std::env::var_os("NEXTEST_RUN_ID").is_some()
            && std::env::var("PATCHBAY_LAN").map_or(true, |v| v != "on")
        {
            assert!(!super::enabled());
        }
    }
}
