//! Dante network control — inferno-net (the inferno-control workspace)
//! wrapped for the routing grid: discover devices over mDNS, pull each
//! one's TX/RX channels + live subscriptions over ARC, and edit
//! subscriptions. This is the Dante Controller replacement surface.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use inferno_net::protocol::DanteClient;
use parking_lot::Mutex;
use patchbay_proto::{DanteChannel, DanteDevice, DanteSubscription, PatchbayError};

// Hardware Dante boxes (consoles, stageboxes) answer mDNS lazily — a
// 2 s browse reliably finds only the local Inferno device; 8 s finds
// the whole network (verified against Galaxy32 / Apollo / Yamahas).
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(8);
const ARC_TIMEOUT: Duration = Duration::from_secs(3);

/// Device-name → ARC endpoint, remembered from the last discovery so
/// subscribe/unsubscribe don't need a fresh mDNS browse.
#[derive(Default)]
pub(crate) struct DanteEndpoints {
    map: Mutex<HashMap<String, SocketAddr>>,
}

impl DanteEndpoints {
    fn client(&self, device: &str) -> Result<DanteClient, PatchbayError> {
        let addr =
            self.map.lock().get(device).copied().ok_or_else(|| {
                PatchbayError::not_found("dante device (refresh the grid)", device)
            })?;
        Ok(DanteClient::new(addr, ARC_TIMEOUT))
    }

    /// Discover devices and fetch channels + subscriptions from each.
    /// Devices that answer mDNS but not ARC come back `unreachable`
    /// (still visible in the grid, greyed).
    pub async fn network(&self) -> Result<Vec<DanteDevice>, PatchbayError> {
        let found = inferno_net::discovery::browse(DISCOVER_TIMEOUT)
            .await
            .map_err(|e| PatchbayError::Internal(format!("mdns browse: {e}")))?;

        let mut devices = Vec::new();
        for (name, dev) in found {
            let Some(ip) = dev.addresses.iter().find(|a| matches!(a, IpAddr::V4(_))) else {
                continue;
            };
            let addr = SocketAddr::new(*ip, dev.arc_port());
            self.map.lock().insert(name.clone(), addr);

            let client = DanteClient::new(addr, ARC_TIMEOUT);
            let (tx, rx) = tokio::join!(client.all_tx_channels(), client.all_rx_channels());
            let (tx, rx, unreachable) = match (tx, rx) {
                (Ok(tx), Ok(rx)) => (tx, rx, false),
                (t, r) => {
                    tracing::warn!(
                        device = %name,
                        "ARC channel query failed: tx={:?} rx={:?}",
                        t.err(),
                        r.err()
                    );
                    (Vec::new(), Vec::new(), true)
                }
            };

            let subscriptions = rx
                .iter()
                .filter(|c| c.tx_channel_name.is_some() || c.tx_device_name.is_some())
                .map(|c| DanteSubscription {
                    rx_channel: c.number as u32,
                    tx_channel: c.tx_channel_name.clone().unwrap_or_default(),
                    tx_device: c.tx_device_name.clone().unwrap_or_default(),
                    status: c.subscription_status as u32,
                })
                .collect();

            devices.push(DanteDevice {
                name,
                ip: ip.to_string(),
                arc_port: dev.arc_port(),
                tx: tx
                    .into_iter()
                    .map(|c| DanteChannel {
                        number: c.number as u32,
                        name: c.name,
                    })
                    .collect(),
                rx: rx
                    .into_iter()
                    .map(|c| DanteChannel {
                        number: c.number as u32,
                        name: c.name,
                    })
                    .collect(),
                subscriptions,
                unreachable,
            });
        }
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(devices)
    }

    pub async fn subscribe(
        &self,
        rx_device: &str,
        rx_channel: u32,
        tx_device: &str,
        tx_channel: &str,
    ) -> Result<(), PatchbayError> {
        self.client(rx_device)?
            .add_subscription(rx_channel as u16, tx_channel, tx_device)
            .await
            .map_err(|e| PatchbayError::Internal(format!("add_subscription: {e}")))
    }

    pub async fn unsubscribe(&self, rx_device: &str, rx_channel: u32) -> Result<(), PatchbayError> {
        self.client(rx_device)?
            .remove_subscription(rx_channel)
            .await
            .map_err(|e| PatchbayError::Internal(format!("remove_subscription: {e}")))
    }
}
