//! Microphone authorization (`AVCaptureDevice`), opening System Settings
//! (`NSWorkspace`) and the native "permission needed" alert (`NSAlert`).

use objc2::MainThreadMarker;
use objc2::runtime::Bool;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSWorkspace};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSString, NSURL};

/// `AVCaptureDevice` authorization for audio input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MicStatus {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    /// `AVMediaTypeAudio` missing or an unknown status value.
    Unknown,
}

/// `+[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]`.
pub(crate) fn mic_status() -> MicStatus {
    // SAFETY: reading an immutable framework constant.
    let Some(media) = (unsafe { AVMediaTypeAudio }) else {
        return MicStatus::Unknown;
    };
    // SAFETY: class method, callable from any thread, with a valid media
    // type constant.
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) } {
        AVAuthorizationStatus::NotDetermined => MicStatus::NotDetermined,
        AVAuthorizationStatus::Restricted => MicStatus::Restricted,
        AVAuthorizationStatus::Denied => MicStatus::Denied,
        AVAuthorizationStatus::Authorized => MicStatus::Authorized,
        _ => MicStatus::Unknown,
    }
}

/// `+[AVCaptureDevice requestAccessForMediaType:completionHandler:]` —
/// shows the Microphone prompt when undecided. `done(granted)` runs on
/// an arbitrary queue. Returns `false` (and never calls `done`) if the
/// media type constant is missing.
pub(crate) fn mic_request(done: impl Fn(bool) + Send + Sync + 'static) -> bool {
    // SAFETY: reading an immutable framework constant.
    let Some(media) = (unsafe { AVMediaTypeAudio }) else {
        return false;
    };
    let block = block2::RcBlock::new(move |granted: Bool| done(granted.as_bool()));
    // SAFETY: class method, callable from any thread; AVFoundation copies
    // the block before returning.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block) };
    true
}

/// `-[NSWorkspace openURL:]`; `false` if the URL is malformed or nothing
/// handles it.
pub(crate) fn open_url(url: &str) -> bool {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return false;
    };
    NSWorkspace::sharedWorkspace().openURL(&url)
}

/// Run an app-modal `NSAlert` with two buttons **on the main thread**
/// (dispatched there and waited for; the main thread must be running its
/// run loop, as the Dioxus/tao event loop does). `true` = the first
/// (`primary`) button.
///
/// Blocks the calling thread until the user answers: call it from a
/// blocking worker, never from the main thread's own async tasks.
pub(crate) fn alert(title: &str, message: &str, primary: &str, secondary: &str) -> bool {
    let (title, message, primary, secondary) = (
        title.to_owned(),
        message.to_owned(),
        primary.to_owned(),
        secondary.to_owned(),
    );
    dispatch2::run_on_main(move |mtm: MainThreadMarker| {
        // Bring the app forward so the alert isn't hidden behind others.
        NSApplication::sharedApplication(mtm).activate();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&title));
        alert.setInformativeText(&NSString::from_str(&message));
        alert.addButtonWithTitle(&NSString::from_str(&primary));
        alert.addButtonWithTitle(&NSString::from_str(&secondary));
        alert.runModal() == NSAlertFirstButtonReturn
    })
}
