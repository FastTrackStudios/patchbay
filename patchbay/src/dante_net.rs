//! Dante network control for the `dante_*` RPCs (the routing grid):
//! discovery, channel/subscription reads and subscription edits all go
//! through `patchbay-dante` (inferno-net) — the same code the `dante`
//! device adapter uses. This module only converts to the proto types
//! and remembers device endpoints between calls.

use std::collections::HashMap;

use parking_lot::Mutex;
use patchbay_dante::{DanteControl, DanteDeviceState, Endpoint, InfernoControl};
use patchbay_proto::{DanteChannel, DanteDevice, DanteSubscription, PatchbayError};

/// Device-name → ARC endpoint, remembered from the last discovery so
/// subscribe/unsubscribe don't need a fresh mDNS browse.
#[derive(Default)]
pub(crate) struct DanteEndpoints {
    control: InfernoControl,
    map: Mutex<HashMap<String, Endpoint>>,
}

fn internal(e: &patchbay_dante::DanteError) -> PatchbayError {
    PatchbayError::Internal(e.to_string())
}

/// Proto view of one scanned device.
fn to_proto(d: DanteDeviceState) -> DanteDevice {
    let subscriptions =
        d.rx.iter()
            .filter(|c| c.tx_channel.is_some() || c.tx_device.is_some())
            .map(|c| DanteSubscription {
                rx_channel: u32::from(c.number),
                tx_channel: c.tx_channel.clone().unwrap_or_default(),
                tx_device: c.tx_device.clone().unwrap_or_default(),
                status: u32::from(c.status),
            })
            .collect();
    DanteDevice {
        ip: d.endpoint.addr.ip().to_string(),
        arc_port: d.endpoint.addr.port(),
        name: d.endpoint.name,
        tx: d
            .tx
            .into_iter()
            .map(|c| DanteChannel {
                number: u32::from(c.number),
                name: c.name,
            })
            .collect(),
        rx: d
            .rx
            .into_iter()
            .map(|c| DanteChannel {
                number: u32::from(c.number),
                name: c.name,
            })
            .collect(),
        subscriptions,
        unreachable: !d.reachable,
    }
}

impl DanteEndpoints {
    fn endpoint(&self, device: &str) -> Result<Endpoint, PatchbayError> {
        self.map
            .lock()
            .get(device)
            .cloned()
            .ok_or_else(|| PatchbayError::not_found("dante device (refresh the grid)", &device))
    }

    /// Discover devices and fetch channels + subscriptions from each.
    /// Devices that answer mDNS but not ARC come back `unreachable`
    /// (still visible in the grid, greyed).
    pub async fn network(&self) -> Result<Vec<DanteDevice>, PatchbayError> {
        let found = patchbay_dante::scan(&self.control, patchbay_dante::DISCOVER_TIMEOUT)
            .await
            .map_err(|e| internal(&e))?;
        {
            let mut map = self.map.lock();
            for d in &found {
                map.insert(d.endpoint.name.clone(), d.endpoint.clone());
            }
        }
        Ok(found.into_iter().map(to_proto).collect())
    }

    pub async fn subscribe(
        &self,
        rx_device: &str,
        rx_channel: u32,
        tx_device: &str,
        tx_channel: &str,
    ) -> Result<(), PatchbayError> {
        // ARC channel numbers are u16 on the wire. Refuse an
        // out-of-range channel rather than silently wrapping it onto a
        // DIFFERENT channel of the same device.
        let channel = u16::try_from(rx_channel).map_err(|_| {
            PatchbayError::Internal(format!("rx channel {rx_channel} out of range (1–65535)"))
        })?;
        let ep = self.endpoint(rx_device)?;
        self.control
            .subscribe(&ep, channel, tx_channel, tx_device)
            .await
            .map_err(|e| internal(&e))
    }

    pub async fn unsubscribe(&self, rx_device: &str, rx_channel: u32) -> Result<(), PatchbayError> {
        let channel = u16::try_from(rx_channel).map_err(|_| {
            PatchbayError::Internal(format!("rx channel {rx_channel} out of range (1–65535)"))
        })?;
        let ep = self.endpoint(rx_device)?;
        self.control
            .unsubscribe(&ep, channel)
            .await
            .map_err(|e| internal(&e))
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use patchbay_dante::{RxChannel, TxChannel};

    use super::*;

    #[test]
    fn scan_state_converts_to_the_grid_view() {
        let d = DanteDeviceState {
            endpoint: Endpoint {
                name: "Stage".into(),
                addr: SocketAddr::from(([10, 0, 0, 5], 4440)),
            },
            arc_name: None,
            model: None,
            sample_rate: None,
            latency_us: None,
            tx: vec![TxChannel {
                number: 1,
                name: "Mic".into(),
            }],
            rx: vec![
                RxChannel {
                    number: 1,
                    name: "01".into(),
                    tx_channel: Some("Main L".into()),
                    tx_device: Some("Console".into()),
                    status: 9,
                },
                RxChannel {
                    number: 2,
                    name: "02".into(),
                    tx_channel: None,
                    tx_device: None,
                    status: 0,
                },
            ],
            reachable: true,
        };
        let p = to_proto(d);
        assert_eq!((p.ip.as_str(), p.arc_port), ("10.0.0.5", 4440));
        assert_eq!(p.subscriptions.len(), 1);
        assert_eq!(p.subscriptions[0].tx_device, "Console");
        assert_eq!(p.rx.len(), 2);
        assert!(!p.unreachable);
    }
}
