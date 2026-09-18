//! Backend-agnostic error type.

use thiserror::Error;

/// Errors surfaced by any [`crate::HostBackend`].
///
/// Variants carry strings rather than source errors so the type stays
/// `Clone` and can cross the RPC boundary later.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostError {
    /// The backend (or this platform) doesn't support the operation.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// No node / port / link / virtual device with this id.
    #[error("not found: {0}")]
    NotFound(String),
    /// A link, channel map or virtual-device spec was rejected by planning.
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
    /// The OS refused for privacy reasons (macOS TCC "System Audio
    /// Recording", say). The string says what to grant.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// An OS call returned an error status.
    #[error("{op} failed: {status}")]
    Os {
        /// The call that failed.
        op: String,
        /// OS status, rendered (`OSStatus` four-char code or errno).
        status: String,
    },
    /// The OS did not answer in time (e.g. a permission prompt pending).
    #[error("timed out: {0}")]
    Timeout(String),
    /// The backend's worker is gone.
    #[error("backend closed")]
    Closed,
}
