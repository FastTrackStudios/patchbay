//! Host privacy permissions (macOS) as seen over RPC.
//!
//! The engine can't own the permission flow — prompts and alerts need the
//! app's run loop, and the grant belongs to whichever app bundle hosts
//! the engine. So the host app registers a [`PermissionProvider`] with
//! [`crate::PatchbayBackend::set_permission_provider`]; without one (a
//! headless `patchbay serve`, Linux) the service answers with a direct,
//! read-only check.

use patchbay_proto::PermissionsStatus;

/// Implemented by the app hosting the engine (`Patchbay.app`).
pub trait PermissionProvider: Send + Sync {
    /// Current state, as last established by the app's flow.
    fn status(&self) -> PermissionsStatus;
    /// Start the request flow (prompts / alert) in the background.
    fn request(&self);
}

/// The answer when no app registered a provider.
pub(crate) fn fallback_status() -> PermissionsStatus {
    if cfg!(target_os = "macos") {
        PermissionsStatus {
            platform: "macos".to_owned(),
            bundled: false,
            system_audio_recording: patchbay_host_coreaudio::capture_permission()
                .as_str()
                .to_owned(),
            microphone: patchbay_host_coreaudio::microphone_permission()
                .as_str()
                .to_owned(),
            local_network: "unknown".to_owned(),
            requesting: false,
            checked_at: 0,
            note: "the engine is not running inside Patchbay.app (e.g. `patchbay serve`), so \
                   these grants belong to the app that launched it; open Patchbay.app instead"
                .to_owned(),
        }
    } else {
        PermissionsStatus {
            platform: std::env::consts::OS.to_owned(),
            bundled: false,
            system_audio_recording: "not_applicable".to_owned(),
            microphone: "not_applicable".to_owned(),
            local_network: "not_applicable".to_owned(),
            requesting: false,
            checked_at: 0,
            note: "no privacy permissions to manage on this platform".to_owned(),
        }
    }
}
