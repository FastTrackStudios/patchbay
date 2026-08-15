//! Managed audio-stack services — status + start/stop/restart for the
//! whitelisted systemd user units the rig depends on. When "no sound"
//! strikes mid-setup, this is the panel that says WHICH layer died
//! (PipeWire? PTP clock? Inferno nodes? routing links?) and restarts it.

use std::process::Command;

use patchbay_proto::{PatchbayError, ServiceAction, ServiceStatus};

/// (unit, label). Whitelist — `service_action` refuses anything else,
/// so the RPC surface is never a general systemctl proxy.
pub(crate) const MANAGED_UNITS: &[(&str, &str)] = &[
    ("pipewire.service", "PipeWire"),
    ("wireplumber.service", "WirePlumber"),
    ("pipewire-pulse.service", "PulseAudio bridge"),
    ("dante.target", "Dante stack"),
    ("statime-inferno.service", "PTP clock (statime)"),
    ("inferno-nodes.service", "Inferno nodes"),
    ("studio-routing-links.service", "Studio routing links"),
];

/// One `systemctl show` for all units; missing units report
/// `present: false` (LoadState=not-found).
pub(crate) fn status_all() -> Vec<ServiceStatus> {
    let mut args = vec![
        "--user",
        "show",
        "--property=Id,ActiveState,SubState,LoadState",
    ];
    args.extend(MANAGED_UNITS.iter().map(|(u, _)| *u));
    let out = Command::new("systemctl")
        .args(&args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    // Blocks separated by blank lines, one per unit, same order as args.
    let mut statuses = Vec::new();
    for (block, (unit, label)) in out.split("\n\n").zip(MANAGED_UNITS) {
        let field = |key: &str| {
            block
                .lines()
                .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
                .unwrap_or("")
                .to_string()
        };
        statuses.push(ServiceStatus {
            unit: unit.to_string(),
            label: label.to_string(),
            state: field("ActiveState"),
            sub_state: field("SubState"),
            present: field("LoadState") != "not-found",
        });
    }
    statuses
}

pub(crate) fn action(unit: &str, action: ServiceAction) -> Result<(), PatchbayError> {
    if !MANAGED_UNITS.iter().any(|(u, _)| *u == unit) {
        return Err(PatchbayError::not_found("managed unit", unit));
    }
    let verb = match action {
        ServiceAction::Start => "start",
        ServiceAction::Stop => "stop",
        ServiceAction::Restart => "restart",
    };
    let st = Command::new("systemctl")
        .args(["--user", verb, unit])
        .status()
        .map_err(|e| PatchbayError::Internal(format!("systemctl spawn failed: {e}")))?;
    st.success().then_some(()).ok_or_else(|| {
        PatchbayError::Internal(format!("systemctl --user {verb} {unit} exited {st}"))
    })
}
