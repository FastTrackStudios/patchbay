//! The control surface: [`DanteControl`] and its inferno-net
//! implementation, plus [`scan`] (discover + read everything).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use inferno_net::protocol::DanteClient;
use inferno_net::protocol::commands::ChannelDirection;
use patchbay_device::BoxFuture;

use crate::DanteError;

/// Hardware Dante boxes (consoles, stageboxes) answer mDNS lazily — a
/// 2 s browse reliably finds only the local Inferno device; 8 s finds
/// the whole network (verified against Galaxy32 / Apollo / Yamahas).
pub const DISCOVER_TIMEOUT: Duration = Duration::from_secs(8);
/// Per-request ARC timeout.
pub const ARC_TIMEOUT: Duration = Duration::from_secs(3);

/// A device's ARC endpoint, from mDNS.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Endpoint {
    /// Dante device name (what subscriptions reference).
    pub name: String,
    /// ARC `ip:port`.
    pub addr: SocketAddr,
}

/// Which side of a device a channel is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSide {
    /// Transmit (a router input / source).
    Tx,
    /// Receive (a router output / destination).
    Rx,
}

impl ChannelSide {
    /// Path segment (`tx` / `rx`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tx => "tx",
            Self::Rx => "rx",
        }
    }
}

/// One transmit channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxChannel {
    /// 1-based Dante channel number.
    pub number: u16,
    /// Channel name (custom label or default).
    pub name: String,
}

/// One receive channel and its subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxChannel {
    /// 1-based Dante channel number.
    pub number: u16,
    /// Channel name.
    pub name: String,
    /// Subscribed TX channel name, if any.
    pub tx_channel: Option<String>,
    /// Subscribed TX device name, if any.
    pub tx_device: Option<String>,
    /// ARC subscription status code.
    pub status: u16,
}

impl RxChannel {
    /// Whether the channel has a subscription.
    #[must_use]
    pub fn is_subscribed(&self) -> bool {
        self.tx_channel.as_deref().is_some_and(|s| !s.is_empty())
            || self.tx_device.as_deref().is_some_and(|s| !s.is_empty())
    }
}

/// Everything read from one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanteDeviceState {
    /// Where it was reached.
    pub endpoint: Endpoint,
    /// Name the device reports over ARC (may lag mDNS after a rename).
    pub arc_name: Option<String>,
    /// Model name from ARC device info.
    pub model: Option<String>,
    /// Sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Receive latency in µs. (ARC reports nanoseconds — 1 ms reads as
    /// 1 000 000 on every device here — although inferno-net's field is
    /// named `latency_us`; converted at read.)
    pub latency_us: Option<u32>,
    /// Transmit channels (empty when unreachable).
    pub tx: Vec<TxChannel>,
    /// Receive channels (empty when unreachable).
    pub rx: Vec<RxChannel>,
    /// Answered the channel queries.
    pub reachable: bool,
}

impl DanteDeviceState {
    /// A device seen over mDNS that did not answer ARC.
    #[must_use]
    pub const fn unreachable(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            arc_name: None,
            model: None,
            sample_rate: None,
            latency_us: None,
            tx: Vec::new(),
            rx: Vec::new(),
            reachable: false,
        }
    }

    /// Device name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.endpoint.name
    }
}

/// What the network adapter needs from the Dante network. Object safe
/// (boxed futures) so tests can substitute a fake.
pub trait DanteControl: Send + Sync {
    /// Browse mDNS for `timeout`; one endpoint per device (IPv4 ARC).
    fn discover(&self, timeout: Duration) -> BoxFuture<'_, Result<Vec<Endpoint>, DanteError>>;
    /// Read one device. Never fails: an unresponsive device comes back
    /// with `reachable: false`.
    fn read<'a>(&'a self, endpoint: &'a Endpoint) -> BoxFuture<'a, DanteDeviceState>;
    /// Subscribe `rx_channel` (1-based) to `tx_channel@tx_device`.
    /// **Writes to the network.**
    fn subscribe<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        rx_channel: u16,
        tx_channel: &'a str,
        tx_device: &'a str,
    ) -> BoxFuture<'a, Result<(), DanteError>>;
    /// Clear `rx_channel`'s subscription. **Writes to the network.**
    fn unsubscribe<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        rx_channel: u16,
    ) -> BoxFuture<'a, Result<(), DanteError>>;
    /// Rename the device. **Writes to the network.**
    fn set_device_name<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), DanteError>>;
    /// Rename one channel. **Writes to the network.**
    fn set_channel_name<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        side: ChannelSide,
        channel: u16,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), DanteError>>;
}

/// The real network, through `inferno-net`.
#[derive(Debug, Clone)]
pub struct InfernoControl {
    /// Per-request ARC timeout.
    pub arc_timeout: Duration,
}

impl Default for InfernoControl {
    fn default() -> Self {
        Self {
            arc_timeout: ARC_TIMEOUT,
        }
    }
}

fn arc(op: &'static str) -> impl Fn(inferno_net::Error) -> DanteError {
    move |e| DanteError::Arc {
        op,
        message: e.to_string(),
    }
}

impl InfernoControl {
    fn client(&self, endpoint: &Endpoint) -> DanteClient {
        DanteClient::new(endpoint.addr, self.arc_timeout)
    }
}

impl DanteControl for InfernoControl {
    fn discover(&self, timeout: Duration) -> BoxFuture<'_, Result<Vec<Endpoint>, DanteError>> {
        Box::pin(async move {
            let found = inferno_net::discovery::browse(timeout)
                .await
                .map_err(|e| DanteError::Discovery(e.to_string()))?;
            let mut out: Vec<Endpoint> = found
                .into_iter()
                .filter_map(|(name, dev)| {
                    let ip = dev.addresses.iter().find(|a| matches!(a, IpAddr::V4(_)))?;
                    Some(Endpoint {
                        addr: SocketAddr::new(*ip, dev.arc_port()),
                        name,
                    })
                })
                .collect();
            out.sort();
            Ok(out)
        })
    }

    fn read<'a>(&'a self, endpoint: &'a Endpoint) -> BoxFuture<'a, DanteDeviceState> {
        Box::pin(async move {
            let client = self.client(endpoint);
            let (tx, rx, settings, info, name) = tokio::join!(
                client.all_tx_channels(),
                client.all_rx_channels(),
                client.device_settings(),
                client.device_info(),
                client.device_name(),
            );
            let (tx, rx) = match (tx, rx) {
                (Ok(tx), Ok(rx)) => (tx, rx),
                (t, r) => {
                    tracing::warn!(
                        device = %endpoint.name,
                        "ARC channel query failed: tx={:?} rx={:?}",
                        t.err(),
                        r.err()
                    );
                    return DanteDeviceState::unreachable(endpoint.clone());
                }
            };
            let settings = settings.ok();
            DanteDeviceState {
                endpoint: endpoint.clone(),
                arc_name: name.ok().filter(|n| !n.is_empty()),
                // Some devices answer the device-info query with their
                // own name; that's no model.
                model: info
                    .ok()
                    .map(|i| i.model_name)
                    .filter(|m| !m.trim().is_empty() && *m != endpoint.name),
                sample_rate: settings.as_ref().and_then(|s| s.sample_rate),
                latency_us: settings
                    .as_ref()
                    .and_then(|s| s.latency_us)
                    .map(|ns| ns.div_ceil(1000)),
                tx: tx
                    .into_iter()
                    .map(|c| TxChannel {
                        number: c.number,
                        name: c.name,
                    })
                    .collect(),
                rx: rx
                    .into_iter()
                    .map(|c| RxChannel {
                        number: c.number,
                        name: c.name,
                        tx_channel: c.tx_channel_name,
                        tx_device: c.tx_device_name,
                        status: c.subscription_status,
                    })
                    .collect(),
                reachable: true,
            }
        })
    }

    fn subscribe<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        rx_channel: u16,
        tx_channel: &'a str,
        tx_device: &'a str,
    ) -> BoxFuture<'a, Result<(), DanteError>> {
        Box::pin(async move {
            self.client(endpoint)
                .add_subscription(rx_channel, tx_channel, tx_device)
                .await
                .map_err(arc("add_subscription"))
        })
    }

    fn unsubscribe<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        rx_channel: u16,
    ) -> BoxFuture<'a, Result<(), DanteError>> {
        Box::pin(async move {
            self.client(endpoint)
                .remove_subscription(u32::from(rx_channel))
                .await
                .map_err(arc("remove_subscription"))
        })
    }

    fn set_device_name<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), DanteError>> {
        Box::pin(async move {
            self.client(endpoint)
                .set_device_name(name)
                .await
                .map_err(arc("set_device_name"))
        })
    }

    fn set_channel_name<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        side: ChannelSide,
        channel: u16,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), DanteError>> {
        Box::pin(async move {
            // The ARC rename command carries the channel as one byte.
            let n = u8::try_from(channel).map_err(|_| {
                DanteError::Invalid(format!(
                    "channel {channel} can't be renamed over ARC (max 255)"
                ))
            })?;
            let dir = match side {
                ChannelSide::Tx => ChannelDirection::Tx,
                ChannelSide::Rx => ChannelDirection::Rx,
            };
            self.client(endpoint)
                .set_channel_name(dir, n, name)
                .await
                .map_err(arc("set_channel_name"))
        })
    }
}

/// Discover the network and read every device (concurrently). Sorted by
/// device name.
///
/// # Errors
/// Only discovery failure; unreachable devices are included, flagged.
pub async fn scan(
    control: &dyn DanteControl,
    timeout: Duration,
) -> Result<Vec<DanteDeviceState>, DanteError> {
    let endpoints = control.discover(timeout).await?;
    Ok(read_all(control, &endpoints).await)
}

/// Read every endpoint concurrently, sorted by device name.
pub(crate) async fn read_all(
    control: &dyn DanteControl,
    endpoints: &[Endpoint],
) -> Vec<DanteDeviceState> {
    let mut out = futures_util::future::join_all(endpoints.iter().map(|e| control.read(e))).await;
    out.sort_by(|a, b| a.endpoint.name.cmp(&b.endpoint.name));
    out
}
