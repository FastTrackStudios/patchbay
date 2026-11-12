//! The Antelope Galaxy32 family: how the hub finds (Manager Server
//! announces) and connects a Galaxy32 (studio#418). `kind =
//! "antelope-galaxy32"`, one default device, `galaxy32`, auto-discovered.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use patchbay::family::{Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily};
use patchbay::proto::{DeviceConfig, FamilyInfo};

pub use crate::KIND;

/// Antelope Galaxy32 interfaces.
#[derive(Debug, Clone, Copy, Default)]
pub struct AntelopeGalaxy32Family;

impl DeviceFamily for AntelopeGalaxy32Family {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo::linked(KIND, "Antelope Galaxy32", &[], true)
    }

    fn defaults(&self) -> Vec<DeviceConfig> {
        vec![DeviceConfig::new("galaxy32", KIND)]
    }

    fn connect(&self, config: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture {
        antelope_galaxy32(config, ctx)
    }
}

/// How long Antelope discovery listens for multicast announces.
const ANTELOPE_DISCOVERY: Duration = Duration::from_secs(2);

fn antelope_error(e: &crate::AntelopeError) -> ConnectError {
    match e {
        crate::AntelopeError::NotFound(_) => ConnectError::NotFound(e.to_string()),
        _ => ConnectError::Failed(e.to_string()),
    }
}

fn antelope_galaxy32(cfg: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
    Box::pin(async move {
        let serial = (!cfg.serial.is_empty()).then_some(cfg.serial.as_str());
        let adapter = if cfg.addr.is_empty() {
            crate::Galaxy32Adapter::discover_and_connect(serial, ANTELOPE_DISCOVERY)
                .await
                .map_err(|e| antelope_error(&e))?
        } else {
            // Pinned endpoint: identity still comes from the announce,
            // so discovery runs and we pick the matching one.
            let addr: SocketAddr = cfg
                .addr
                .parse()
                .map_err(|e| ConnectError::Failed(format!("addr '{}': {e}", cfg.addr)))?;
            let announces = crate::discover(ANTELOPE_DISCOVERY)
                .await
                .map_err(|e| antelope_error(&e))?;
            let announce = announces
                .iter()
                .find(|a| {
                    a.is_control()
                        && a.socket_addr() == Some(addr)
                        && serial.is_none_or(|s| a.serial() == Some(s))
                })
                .ok_or_else(|| {
                    ConnectError::NotFound(format!(
                        "no Antelope control endpoint announced at {addr}"
                    ))
                })?;
            crate::Galaxy32Adapter::connect(addr, announce)
                .await
                .map_err(|e| antelope_error(&e))?
        };
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}
