//! The Antelope Galaxy32 family: how the hub finds (Manager Server
//! announces) and connects a Galaxy32 (studio#418). `kind =
//! "antelope-galaxy32"`, one default device, `galaxy32`, auto-discovered.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::{Announce, AntelopeError};

use patchbay::family::{Adapter, ConnectCtx, ConnectError, ConnectFuture, DeviceFamily};
use patchbay::proto::{DeviceConfig, FamilyInfo};

pub use crate::KIND;

/// Where the family hears the Manager Server's announces. The real one
/// listens on its multicast group ([`ManagerServerAnnounces`]); a test
/// hands the family a fake, so no test reaches the LAN.
pub trait AnnounceSource: Send + Sync {
    /// Every announce heard within `timeout`.
    fn announces(
        &self,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Announce>, AntelopeError>> + Send + '_>>;
}

/// The Manager Server's multicast announces (`239.192.5.8:5008`):
/// [`crate::discover`]. Refused under `PATCHBAY_LAN=off`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManagerServerAnnounces;

impl AnnounceSource for ManagerServerAnnounces {
    fn announces(
        &self,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Announce>, AntelopeError>> + Send + '_>> {
        Box::pin(crate::discover(timeout))
    }
}

/// Antelope Galaxy32 interfaces.
#[derive(Clone)]
pub struct AntelopeGalaxy32Family {
    announces: Arc<dyn AnnounceSource>,
}

impl Default for AntelopeGalaxy32Family {
    /// The family on the real announces.
    fn default() -> Self {
        Self::with_announces(Arc::new(ManagerServerAnnounces))
    }
}

impl AntelopeGalaxy32Family {
    /// The family hearing its announces from `source`.
    #[must_use]
    pub fn with_announces(source: Arc<dyn AnnounceSource>) -> Self {
        Self { announces: source }
    }
}

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
        antelope_galaxy32(config, ctx, Arc::clone(&self.announces))
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

fn antelope_galaxy32(
    cfg: DeviceConfig,
    _ctx: ConnectCtx,
    source: Arc<dyn AnnounceSource>,
) -> ConnectFuture {
    Box::pin(async move {
        let serial = (!cfg.serial.is_empty()).then_some(cfg.serial.as_str());
        // A pinned address is checked before anything listens.
        let pinned: Option<SocketAddr> = if cfg.addr.is_empty() {
            None
        } else {
            Some(
                cfg.addr
                    .parse()
                    .map_err(|e| ConnectError::Failed(format!("addr '{}': {e}", cfg.addr)))?,
            )
        };
        let announces = source
            .announces(ANTELOPE_DISCOVERY)
            .await
            .map_err(|e| antelope_error(&e))?;
        let adapter = match pinned {
            None => crate::Galaxy32Adapter::connect_from_announces(announces, serial)
                .await
                .map_err(|e| antelope_error(&e))?,
            // Pinned endpoint: identity still comes from the announce,
            // so we pick the matching one.
            Some(addr) => {
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
            }
        };
        let adapter: Adapter = Arc::new(adapter);
        Ok(adapter)
    })
}
