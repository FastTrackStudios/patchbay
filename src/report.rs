//! Reports composed from several calls, shared by the `patchbay` CLI and
//! the extension's actions so both answer with the same JSON.

use serde::Serialize;

use crate::proto::{ClockInfo, DanteDevice, DanteStatus, PatchbayServiceClient, ServiceStatus};

/// `patchbay status`: the graph's size, the clock and whether the Dante
/// stack runs.
///
/// # Errors
/// The graph, clock or Dante status call failed.
pub async fn status(c: &PatchbayServiceClient) -> Result<serde_json::Value, String> {
    let (graph, clock, dante, pipewire) =
        futures_util::join!(c.graph(), c.clock(), c.dante_status(), pipewire(c));
    let (graph, clock, dante) = (text(graph)?, text(clock)?, text(dante)?);
    Ok(serde_json::json!({
        "nodes": graph.nodes.len(), "ports": graph.ports.len(), "links": graph.links.len(),
        "clock": clock, "dante_active": dante.active, "pipewire": pipewire,
    }))
}

/// The engine's `PipeWire` connection (`on`, `off`, `unsupported`), or
/// `unknown` from an engine too old to say.
async fn pipewire(c: &PatchbayServiceClient) -> String {
    c.engine_info()
        .await
        .map_or_else(|_| "unknown".to_owned(), |info| info.pipewire)
}

/// One finding of a health check.
#[derive(Debug, Clone, Serialize)]
pub struct HealthIssue {
    pub severity: String,
    pub code: String,
    pub target: String,
    pub detail: String,
    pub remediation: Option<String>,
}

/// The graph's size, for the report.
#[derive(Debug, Clone, Serialize)]
pub struct GraphHealth {
    pub nodes: usize,
    pub ports: usize,
    pub links: usize,
    pub active_links: usize,
}

/// `patchbay health`: the rig and Dante health check, as agents read it.
#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub ok: bool,
    /// The engine's `PipeWire` connection: `on`, `off`
    /// (`PATCHBAY_PIPEWIRE=off`), `unsupported` (not Linux), `unknown`.
    pub pipewire: String,
    pub graph: GraphHealth,
    pub clock: ClockInfo,
    pub dante: DanteStatus,
    pub services: Vec<ServiceStatus>,
    pub dante_devices: Vec<DanteDevice>,
    pub dante_scan_error: Option<String>,
    pub issues: Vec<HealthIssue>,
}

/// One finding.
fn issue(
    severity: &str,
    code: &str,
    target: impl Into<String>,
    detail: impl Into<String>,
    remediation: Option<&str>,
) -> HealthIssue {
    HealthIssue {
        severity: severity.to_owned(),
        code: code.to_owned(),
        target: target.into(),
        detail: detail.into(),
        remediation: remediation.map(str::to_owned),
    }
}

/// The Dante findings: the stack, the network scan, each subscription.
pub fn add_dante_issues(
    dante: &DanteStatus,
    devices: &[DanteDevice],
    scan_error: Option<&str>,
    issues: &mut Vec<HealthIssue>,
) {
    if !dante.installed {
        issues.push(issue(
            "warning",
            "dante_not_installed",
            "dante.target",
            "the managed Dante stack is not installed on this host",
            Some("install/deploy the Dante stack, then run `patchbay dante health` again"),
        ));
    } else if !dante.active {
        issues.push(issue(
            "error",
            "dante_stack_inactive",
            "dante.target",
            "the managed Dante stack is installed but inactive",
            Some("patchbay dante repair --start-stack"),
        ));
    }

    if let Some(error) = scan_error {
        issues.push(issue(
            if dante.active { "error" } else { "warning" },
            "dante_scan_failed",
            "dante network",
            error,
            Some("verify mDNS/ARC reachability, then retry `patchbay dante health`"),
        ));
        return;
    }

    let device_names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
    for device in devices {
        if device.unreachable {
            issues.push(issue(
                "error",
                "dante_device_unreachable",
                &device.name,
                format!(
                    "{} is visible by mDNS at {} but did not answer ARC",
                    device.name, device.ip
                ),
                Some("check network/VLAN reachability and the device's Dante control service"),
            ));
        }
        for subscription in &device.subscriptions {
            if subscription.status != 1 {
                issues.push(issue(
                    "error",
                    "dante_subscription_unhealthy",
                    format!("{}:rx{}", device.name, subscription.rx_channel),
                    format!(
                        "subscription to {}@{} has ARC status {} (1 is healthy)",
                        subscription.tx_channel, subscription.tx_device, subscription.status
                    ),
                    Some("compare with the saved snapshot, then run `patchbay dante repair --apply-config`"),
                ));
            }
            if !subscription.tx_device.is_empty()
                && !device_names.contains(&subscription.tx_device.as_str())
            {
                issues.push(issue(
                    "warning",
                    "dante_source_not_discovered",
                    format!("{}:rx{}", device.name, subscription.rx_channel),
                    format!(
                        "source device '{}' was not discovered in this scan",
                        subscription.tx_device
                    ),
                    Some("check the source device's network/VLAN and repeat the scan"),
                ));
            }
        }
    }
}

/// Run the health check against the engine behind `c`.
///
/// # Errors
/// The graph, clock, Dante status or services call failed.
pub async fn collect_health(c: &PatchbayServiceClient) -> Result<HealthReport, String> {
    let (graph, clock, dante, services, network, pipewire) = futures_util::join!(
        c.graph(),
        c.clock(),
        c.dante_status(),
        c.services(),
        c.dante_network(),
        pipewire(c),
    );
    let graph = text(graph)?;
    let clock = text(clock)?;
    let dante = text(dante)?;
    let services = text(services)?;

    let (dante_devices, dante_scan_error) = match network {
        Ok(devices) => (devices, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };

    let mut issues = Vec::new();
    if pipewire == "off" {
        issues.push(issue(
            "warning",
            "pipewire_disabled",
            "PATCHBAY_PIPEWIRE",
            "this engine runs with PATCHBAY_PIPEWIRE=off: no PipeWire connection, so no graph",
            Some("unset PATCHBAY_PIPEWIRE and restart the engine"),
        ));
    }
    for service in &services {
        if service.unit == "dante.target" {
            continue;
        }
        let critical = matches!(
            service.unit.as_str(),
            "pipewire.service" | "wireplumber.service" | "pipewire-pulse.service"
        );
        if !service.present {
            issues.push(issue(
                if critical { "error" } else { "warning" },
                "managed_service_missing",
                &service.unit,
                format!("{} is not installed", service.label),
                Some("deploy the managed audio stack or remove this optional service from the rig"),
            ));
        } else if service.state != "active" {
            issues.push(issue(
                if critical { "error" } else { "warning" },
                "managed_service_unhealthy",
                &service.unit,
                format!(
                    "{} is {}/{}",
                    service.label, service.state, service.sub_state
                ),
                Some("inspect `patchbay services --json`, then restart the affected managed unit"),
            ));
        }
    }
    add_dante_issues(
        &dante,
        &dante_devices,
        dante_scan_error.as_deref(),
        &mut issues,
    );

    let report = HealthReport {
        ok: !issues.iter().any(|finding| finding.severity == "error"),
        pipewire,
        graph: GraphHealth {
            nodes: graph.nodes.len(),
            ports: graph.ports.len(),
            links: graph.links.len(),
            active_links: graph.links.iter().filter(|link| link.active).count(),
        },
        clock,
        dante,
        services,
        dante_devices,
        dante_scan_error,
        issues,
    };
    Ok(report)
}

/// A call's error as text, the way the CLI prints it and actions return it.
fn text<T, E: std::fmt::Display>(r: Result<T, E>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}
