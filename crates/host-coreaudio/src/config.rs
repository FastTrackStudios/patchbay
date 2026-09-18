//! Platform-independent configuration types (compiled everywhere so
//! callers can build configs without `cfg` noise).

use patchbay_host::{AppSelector, ChannelMap};
use serde::{Deserialize, Serialize};

/// What a tapped app hears on its own device while patchbay taps it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TapMute {
    /// The app keeps playing normally (default — patchbay never silences
    /// the user's apps unless asked).
    #[default]
    Unmuted,
    /// The app is silent on its own device while the tap exists (true
    /// "route away", like Loopback's mute option).
    Muted,
    /// Silent only while the tap is actually being read.
    MutedWhenTapped,
}

/// A Loopback-style "app → output device" monitor, the first concrete
/// piece of a virtual device without a HAL driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TapMonitorConfig {
    /// Which app(s) to tap (all processes matching the selector).
    pub app: AppSelector,
    /// `DeviceUID` of the output device to monitor to.
    pub output_device_uid: String,
    /// Tap channel (0 = L, 1 = R of the stereo mixdown) → output device
    /// channel.
    pub channel_map: ChannelMap,
    /// Linear gain, 0.0 up to [`patchbay_host::MAX_GAIN`].
    pub gain: f32,
    /// Mute behaviour of the tapped app.
    pub mute: TapMute,
}

impl TapMonitorConfig {
    /// Stereo, unity gain, non-muting.
    #[must_use]
    pub fn new(app: AppSelector, output_device_uid: impl Into<String>) -> Self {
        Self {
            app,
            output_device_uid: output_device_uid.into(),
            channel_map: ChannelMap::identity(2),
            gain: 1.0,
            mute: TapMute::Unmuted,
        }
    }
}

/// macOS "System Audio Recording" (TCC `kTCCServiceAudioCapture`) state
/// of the *responsible* app (`Patchbay.app` when bundled, the terminal
/// for a `cargo run`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePermission {
    /// Taps will deliver audio.
    Granted,
    /// Taps are created but deliver silence.
    Denied,
    /// Undecided: the first tap (or `request_capture_permission`) prompts.
    NotDetermined,
    /// Could not be determined (private preflight SPI missing, or not macOS).
    Unknown,
}

/// macOS Microphone (audio input) authorization of the responsible app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophonePermission {
    /// Audio input is allowed.
    Granted,
    /// The user said no (change it in System Settings).
    Denied,
    /// Blocked by policy (MDM / parental controls).
    Restricted,
    /// Undecided: `request_microphone_permission` prompts.
    NotDetermined,
    /// Could not be determined (not macOS).
    Unknown,
}

impl CapturePermission {
    /// The `snake_case` wire name (same as the serde form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::Unknown => "unknown",
        }
    }
}

impl MicrophonePermission {
    /// The `snake_case` wire name (same as the serde form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Restricted => "restricted",
            Self::NotDetermined => "not_determined",
            Self::Unknown => "unknown",
        }
    }
}
