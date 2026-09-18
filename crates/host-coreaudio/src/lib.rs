//! `patchbay-host-coreaudio` — the macOS Core Audio host backend.
//!
//! Implements [`patchbay_host::HostBackend`] over the Core Audio HAL:
//!
//! - **Milestone 1 (done)** — [`CoreAudioBackend`]: hardware / virtual /
//!   aggregate devices and audio-client processes as
//!   [`patchbay_host::HostNode`]s; HAL property listeners → live
//!   [`patchbay_host::HostEvent`]s.
//! - **Milestone 2 (first cut)** — [`TapMonitor`]: a Loopback-style
//!   "app → output device" monitor built from a process tap
//!   (`CATapDescription`, macOS 14.2+) and a private aggregate device, no
//!   HAL driver needed. Needs the "System Audio Recording" permission
//!   ([`capture_permission`]).
//! - **Permissions** — [`request_capture_permission`] /
//!   [`request_microphone_permission`] trigger the system prompts,
//!   [`open_capture_settings`] opens the right System Settings pane and
//!   [`permission_alert`] shows a native alert.
//!
//! On every other OS this crate compiles to a stub whose constructors
//! return [`patchbay_host::HostError::Unsupported`], so Linux builds of
//! the workspace never break.
//!
//! All `unsafe` is confined to the private `ffi` module; the rest of the
//! crate is `deny(unsafe_code)`. See `docs/host-backends.md`.
#![deny(unsafe_code)]

mod config;

#[cfg(target_os = "macos")]
mod backend;
#[cfg(target_os = "macos")]
mod enumerate;
#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
mod monitor;
#[cfg(target_os = "macos")]
mod permissions;

#[cfg(not(target_os = "macos"))]
mod unsupported;

pub use config::{CapturePermission, MicrophonePermission, TapMonitorConfig, TapMute};

#[cfg(target_os = "macos")]
pub use backend::CoreAudioBackend;
#[cfg(target_os = "macos")]
pub use monitor::{TapMonitor, TapMonitorInfo, capture_permission};
#[cfg(target_os = "macos")]
pub use permissions::{
    microphone_permission, open_capture_settings, open_microphone_settings, permission_alert,
    request_capture_permission, request_microphone_permission,
};

#[cfg(not(target_os = "macos"))]
pub use unsupported::{
    CoreAudioBackend, TapMonitor, TapMonitorInfo, capture_permission, microphone_permission,
    open_capture_settings, open_microphone_settings, permission_alert, request_capture_permission,
    request_microphone_permission,
};

/// Whether this build has a working Core Audio backend (macOS only).
pub const SUPPORTED: bool = cfg!(target_os = "macos");
