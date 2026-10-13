//! Adapter-agnostic error type.

use thiserror::Error;

/// Errors surfaced by any [`crate::DeviceAdapter`].
///
/// Variants carry strings rather than source errors so the type stays
/// `Clone` and can cross the RPC boundary later.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeviceError {
    /// The control connection is down.
    #[error("device is offline")]
    Offline,
    /// The device did not answer in time.
    #[error("device did not answer within the timeout: {0}")]
    Timeout(String),
    /// No parameter with this path.
    #[error("unknown parameter `{0}`")]
    UnknownParam(String),
    /// No port group / channel with this reference.
    #[error("unknown port `{0}`")]
    UnknownPort(String),
    /// The value doesn't fit the parameter or route.
    #[error("invalid value for `{target}`: {reason}")]
    InvalidValue {
        /// Param path or port the value was for.
        target: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The parameter is read-only.
    #[error("parameter `{0}` is read-only")]
    ReadOnly(String),
    /// The parameter interrupts audio and the write lacked
    /// [`crate::WriteGuard::AllowDisruptive`].
    #[error("parameter `{0}` interrupts audio; write it with WriteGuard::AllowDisruptive")]
    DisruptiveWrite(String),
    /// The adapter doesn't support this operation (yet).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The device rejected the request or sent something unparseable.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Socket-level failure.
    #[error("transport error: {0}")]
    Transport(String),
}
