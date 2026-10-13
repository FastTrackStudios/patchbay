//! Adapter-local error type.

use patchbay_device::DeviceError;
use thiserror::Error;

/// Anything the Dante control layer can fail with.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DanteError {
    /// mDNS browse failed.
    #[error("mdns browse: {0}")]
    Discovery(String),
    /// An ARC request failed or timed out.
    #[error("{op}: {message}")]
    Arc {
        /// Which operation (`add_subscription`, `set_channel_name`, …).
        op: &'static str,
        /// What went wrong.
        message: String,
    },
    /// A value can't be sent (name too long, channel out of range).
    #[error("invalid: {0}")]
    Invalid(String),
    /// mDNS found no Dante device at all.
    #[error("no Dante devices found on the network (mDNS)")]
    Empty,
    /// No such device on the network.
    #[error("no dante device named `{0}`")]
    NotFound(String),
}

impl From<DanteError> for DeviceError {
    fn from(e: DanteError) -> Self {
        match e {
            DanteError::Discovery(m) => Self::Transport(format!("mdns browse: {m}")),
            DanteError::Arc { op, message } if message.contains("timeout") => {
                Self::Timeout(format!("{op}: {message}"))
            }
            DanteError::Arc { op, message } => Self::Protocol(format!("{op}: {message}")),
            DanteError::Invalid(m) => Self::InvalidValue {
                target: "dante".to_owned(),
                reason: m,
            },
            DanteError::NotFound(d) => Self::UnknownPort(d),
            DanteError::Empty => Self::Offline,
        }
    }
}
