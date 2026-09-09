//! Dante/Inferno `AoIP` stack switches.
//!
//! Thin wrappers over the per-user systemd units the flake deploys
//! (`dante.target` + friends). Same contract as the `dante on|off|status`
//! shell helper, machine-readable.

use patchbay_proto::{DanteStatus, UnitStatus};

/// The stack's master unit — `dante on`/`off` toggles exactly this.
const TARGET: &str = "dante.target";

/// Units surfaced in the status panel (superset is fine — absent units
/// report as not-present and the UI dims them).
const UNITS: &[(&str, &str)] = &[
    (TARGET, "Dante stack"),
    ("inferno-nodes.service", "Inferno nodes"),
    ("statime-inferno.service", "PTP clock (statime)"),
    ("studio-routing-links.service", "Studio routing links"),
];

/// State of the whole stack, from ONE `systemctl show` (this used to be
/// a separate spawn per unit).
#[must_use]
pub fn status() -> DanteStatus {
    let statuses = crate::units::status_of(UNITS);
    let target = statuses.iter().find(|s| s.unit == TARGET);
    DanteStatus {
        installed: target.is_some_and(|s| s.present),
        active: target.is_some_and(|s| s.state == "active"),
        units: statuses
            .iter()
            .filter(|s| s.present)
            .map(|s| UnitStatus {
                unit: s.unit.clone(),
                state: s.state.clone(),
            })
            .collect(),
    }
}

/// `dante on` / `dante off`.
///
/// # Errors
/// If `systemctl` can't be spawned or the unit fails to start/stop.
pub fn set(on: bool) -> Result<(), String> {
    crate::units::run_verb(if on { "start" } else { "stop" }, TARGET)
}
