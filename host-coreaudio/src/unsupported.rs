//! Non-macOS stub: same public names, every constructor fails with
//! [`HostError::Unsupported`].

use std::time::Duration;

use patchbay_host::HostError;
use serde::Serialize;

use crate::config::{CapturePermission, MicrophonePermission, TapMonitorConfig};

fn unsupported() -> HostError {
    HostError::Unsupported("Core Audio is only available on macOS".to_owned())
}

/// Stub of the macOS backend; [`Self::new`] always fails.
#[derive(Debug)]
pub struct CoreAudioBackend {
    _never: (),
}

impl CoreAudioBackend {
    /// Always [`HostError::Unsupported`] off macOS.
    ///
    /// # Errors
    /// Always.
    pub fn new() -> Result<Self, HostError> {
        Err(unsupported())
    }
}

/// Stub; see the macOS docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TapMonitorInfo {
    _never: (),
}

/// Stub of the macOS tap monitor; constructors always fail.
#[derive(Debug)]
pub struct TapMonitor {
    _never: (),
}

impl TapMonitor {
    /// Always [`HostError::Unsupported`] off macOS.
    ///
    /// # Errors
    /// Always.
    pub fn start(_config: &TapMonitorConfig) -> Result<Self, HostError> {
        Err(unsupported())
    }

    /// Always [`HostError::Unsupported`] off macOS.
    ///
    /// # Errors
    /// Always.
    pub fn start_with_timeout(
        _config: &TapMonitorConfig,
        _timeout: Duration,
    ) -> Result<Self, HostError> {
        Err(unsupported())
    }
}

/// Always [`CapturePermission::Unknown`] off macOS.
#[must_use]
pub const fn capture_permission() -> CapturePermission {
    CapturePermission::Unknown
}

/// Always [`CapturePermission::Unknown`] off macOS.
#[allow(clippy::unused_async)]
pub async fn request_capture_permission() -> CapturePermission {
    CapturePermission::Unknown
}

/// Always [`MicrophonePermission::Unknown`] off macOS.
#[must_use]
pub const fn microphone_permission() -> MicrophonePermission {
    MicrophonePermission::Unknown
}

/// Always [`MicrophonePermission::Unknown`] off macOS.
#[allow(clippy::unused_async)]
pub async fn request_microphone_permission() -> MicrophonePermission {
    MicrophonePermission::Unknown
}

/// No System Settings off macOS; always `false`.
#[must_use]
pub const fn open_capture_settings() -> bool {
    false
}

/// No System Settings off macOS; always `false`.
#[must_use]
pub const fn open_microphone_settings() -> bool {
    false
}

/// No native alert off macOS; always `false`.
#[allow(clippy::unused_async)]
pub async fn permission_alert(
    _title: &str,
    _message: &str,
    _primary: &str,
    _secondary: &str,
) -> bool {
    false
}
