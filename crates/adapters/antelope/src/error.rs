//! Adapter error type.

use patchbay_device::DeviceError;
use thiserror::Error;

/// Everything that can go wrong talking to an Antelope Manager Server.
#[derive(Debug, Error)]
pub enum AntelopeError {
    /// Socket-level failure.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Malformed frame or unexpected message shape.
    #[error("protocol: {0}")]
    Protocol(String),
    /// No reply within the request timeout.
    #[error("`{method}` timed out")]
    Timeout {
        /// Method that timed out.
        method: String,
    },
    /// The server answered `COMMAND_STATUS: FAIL`.
    #[error("server rejected `{method}` (COMMAND_STATUS FAIL)")]
    CommandFailed {
        /// Method (best effort — header-less FAIL replies are matched to
        /// the oldest pending request).
        method: String,
    },
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
    /// Nothing answered discovery / no usable control endpoint.
    #[error("no device found: {0}")]
    NotFound(String),
    /// A caller-supplied value is out of range or malformed.
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// A write was sent but the read-back disagrees.
    #[error("write not confirmed by read-back: {0}")]
    NotConfirmed(String),
}

/// Crate-wide result alias.
pub type Result<T, E = AntelopeError> = std::result::Result<T, E>;

impl From<AntelopeError> for DeviceError {
    fn from(e: AntelopeError) -> Self {
        match e {
            AntelopeError::Io(e) => Self::Transport(e.to_string()),
            AntelopeError::Closed => Self::Offline,
            AntelopeError::Timeout { method } => Self::Timeout(method),
            AntelopeError::Invalid(reason) => Self::InvalidValue {
                target: String::new(),
                reason,
            },
            AntelopeError::NotFound(what) => Self::Transport(format!("no device found: {what}")),
            e @ (AntelopeError::Json(_)
            | AntelopeError::Protocol(_)
            | AntelopeError::CommandFailed { .. }
            | AntelopeError::NotConfirmed(_)) => Self::Protocol(e.to_string()),
        }
    }
}
