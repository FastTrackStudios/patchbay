//! Dante/Inferno AoIP stack switches — thin wrappers over the per-user
//! systemd units the flake deploys (`dante.target` + friends). Same
//! contract as the `dante on|off|status` shell helper, machine-readable.

use std::process::Command;

use patchbay_proto::{DanteStatus, UnitStatus};

/// Units surfaced in the status panel (superset is fine — absent units
/// report as `inactive`/unknown and the UI dims them).
const UNITS: &[&str] = &[
    "dante.target",
    "inferno-nodes.service",
    "statime-inferno.service",
    "studio-routing-links.service",
];

fn active_state(unit: &str) -> Option<String> {
    let out = Command::new("systemctl")
        .args(["--user", "show", "--property=ActiveState", "--value", unit])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn status() -> DanteStatus {
    let units: Vec<UnitStatus> = UNITS
        .iter()
        .filter_map(|u| {
            active_state(u).map(|state| UnitStatus {
                unit: u.to_string(),
                state,
            })
        })
        .collect();
    let target = units.iter().find(|u| u.unit == "dante.target");
    DanteStatus {
        installed: target.is_some(),
        active: target.is_some_and(|u| u.state == "active"),
        units,
    }
}

/// `dante on` / `dante off`.
pub fn set(on: bool) -> Result<(), String> {
    let verb = if on { "start" } else { "stop" };
    let st = Command::new("systemctl")
        .args(["--user", verb, "dante.target"])
        .status()
        .map_err(|e| format!("systemctl spawn failed: {e}"))?;
    st.success()
        .then_some(())
        .ok_or_else(|| format!("systemctl --user {verb} dante.target exited {st}"))
}
