//! Adapter error type.

use patchbay_device::DeviceError;
use thiserror::Error;

/// Everything that can go wrong talking to a Yamaha console over RCP.
#[derive(Debug, Error)]
pub enum TfError {
    /// Socket-level failure.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed line or unexpected reply shape.
    #[error("protocol: {0}")]
    Protocol(String),
    /// No reply within the request timeout.
    #[error("`{command}` timed out")]
    Timeout {
        /// The command line (without LF) that timed out.
        command: String,
    },
    /// The console answered `ERROR <command> <reason>`.
    #[error("console rejected `{command}`: {reason}")]
    Rejected {
        /// The command verb the console echoed (`get`, `set`, …).
        command: String,
        /// The console's reason code (`InvalidArgument`, `UnknownAddress`, …).
        reason: String,
    },
    /// The connection is down (or was closed while the request waited).
    #[error("connection closed")]
    Closed,
    /// A caller-supplied value is out of range or malformed.
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// A write was acknowledged but the read-back disagrees.
    #[error("write not confirmed by read-back: {0}")]
    NotConfirmed(String),
}

/// Crate-wide result alias.
pub type Result<T, E = TfError> = std::result::Result<T, E>;

impl From<TfError> for DeviceError {
    fn from(e: TfError) -> Self {
        match e {
            TfError::Io(e) => Self::Transport(e.to_string()),
            TfError::Closed => Self::Offline,
            TfError::Timeout { command } => Self::Timeout(command),
            TfError::Invalid(reason) => Self::InvalidValue {
                target: String::new(),
                reason,
            },
            TfError::Rejected { command, reason } => match reason.as_str() {
                "InvalidArgument" | "WrongFormat" | "TooLongCommand" => Self::InvalidValue {
                    target: command,
                    reason,
                },
                "ReadOnly" | "AccessDenied" | "NoPermission" => Self::ReadOnly(command),
                _ => Self::Protocol(format!("console rejected `{command}`: {reason}")),
            },
            e @ (TfError::Protocol(_) | TfError::NotConfirmed(_)) => Self::Protocol(e.to_string()),
        }
    }
}
