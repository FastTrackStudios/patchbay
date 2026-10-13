//! macOS privacy permissions Patchbay needs, and how to ask for them.
//!
//! - **System Audio Recording** (TCC `kTCCServiceAudioCapture`) — process
//!   taps deliver silence without it. There is no public request API; the
//!   supported way to get the system prompt is to *use* a tap:
//!   [`request_capture_permission`] creates a private, non-muting global
//!   tap (excluding Patchbay itself) inside a private tap-only aggregate,
//!   runs a no-op `IOProc` for ~200 ms and tears everything down. The prompt is
//!   asynchronous; with the `tcc-spi` feature the private
//!   `TCCAccessRequest` reports the answer (and prompts itself if the
//!   probe could not be set up). Without it the in-process preflight is
//!   polled, which libTCC caches — the answer may only show after a
//!   relaunch.
//! - **Microphone** — `AVCaptureDevice` authorization for audio input
//!   ([`request_microphone_permission`]).
//!
//! The grant belongs to the *responsible* app: `Patchbay.app` when
//! bundled (keyed on bundle id + signing team), the terminal for a
//! `cargo run`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use patchbay_host::HostError;

use crate::config::{CapturePermission, MicrophonePermission};
use crate::ffi::hal;
use crate::ffi::ioproc::{Buffers, BuffersMut, IoProc, Render};
use crate::ffi::permission::{self as sys_perm, MicStatus};
use crate::ffi::tap::{AggregateDevice, Mute, ProcessTap};
use crate::monitor::capture_permission;

/// How long the probe keeps IO running on the tap.
const PROBE_RUN: Duration = Duration::from_millis(200);
/// How long the probe may block: tap creation waits for the user to
/// answer the prompt, so this matches [`ANSWER_TIMEOUT`].
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the user to answer a prompt.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(500);

/// System Settings → Privacy & Security → Screen & System Audio Recording.
/// The `Privacy_AudioCapture` anchor exists in the `PrivacySecurity`
/// extension on macOS 15+; the legacy pane id and the Screen Recording
/// anchor are fallbacks for older/changed systems.
const CAPTURE_SETTINGS_URLS: [&str; 3] = [
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AudioCapture",
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture",
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
];
/// System Settings → Privacy & Security → Microphone.
const MICROPHONE_SETTINGS_URLS: [&str; 2] = [
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone",
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
];

/// Ask for System Audio Recording if it is still undecided.
///
/// Shows the system prompt and waits (without blocking the caller's
/// executor thread) up to 60 s for the answer. Already decided → returns
/// the current state immediately, no prompt.
pub async fn request_capture_permission() -> CapturePermission {
    let now = capture_permission();
    if !matches!(
        now,
        CapturePermission::NotDetermined | CapturePermission::Unknown
    ) {
        return now;
    }

    let probe = run_blocking(PROBE_TIMEOUT, probe_capture).await;
    let probe_ok = matches!(probe, Some(Ok(())));
    match &probe {
        Some(Ok(())) => tracing::info!("system audio recording: tap probe ran (prompt requested)"),
        Some(Err(e)) => tracing::warn!("system audio recording: tap probe failed: {e}"),
        None => tracing::warn!("system audio recording: tap probe timed out"),
    }

    // The answer. `TCCAccessPreflight` is cached inside this process
    // (verified on macOS 27: after the user allowed the prompt, tccd
    // recorded the grant but every in-process preflight kept saying
    // "not determined"), so polling it cannot see the answer. With
    // `tcc-spi`, `TCCAccessRequest` both reports the decision (no second
    // prompt once decided or while the tap's prompt is up) and, if the
    // probe failed, shows the prompt itself.
    if let Some(answer) = capture_answer_via_spi(probe_ok).await {
        return answer;
    }
    if now == CapturePermission::Unknown {
        return capture_permission();
    }
    let started = Instant::now();
    loop {
        let state = capture_permission();
        if state != CapturePermission::NotDetermined || started.elapsed() >= ANSWER_TIMEOUT {
            return state;
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(feature = "tcc-spi")]
async fn capture_answer_via_spi(probe_ok: bool) -> Option<CapturePermission> {
    if probe_ok {
        tracing::info!("system audio recording: waiting for the answer via TCCAccessRequest");
    } else {
        tracing::info!("system audio recording: prompting via TCCAccessRequest (private SPI)");
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    let tx = std::sync::Mutex::new(Some(tx));
    let asked = crate::ffi::sys::audio_capture_request(move |granted| {
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(granted);
        }
    });
    if !asked {
        tracing::warn!("system audio recording: TCCAccessRequest unavailable");
        return None;
    }
    match tokio::time::timeout(ANSWER_TIMEOUT, rx).await {
        Ok(Ok(true)) => Some(CapturePermission::Granted),
        Ok(Ok(false)) => Some(CapturePermission::Denied),
        _ => Some(CapturePermission::NotDetermined),
    }
}

#[cfg(not(feature = "tcc-spi"))]
#[allow(clippy::unused_async)]
async fn capture_answer_via_spi(_probe_ok: bool) -> Option<CapturePermission> {
    None
}

/// A render callback that does nothing: the probe only needs IO to run.
struct Silence;

impl Render for Silence {
    fn render(&self, _input: &Buffers<'_>, _output: &mut BuffersMut<'_>) {}
}

/// Create a private global tap (excluding this process) inside a private,
/// tap-only aggregate, run IO briefly, then tear it all down (explicit
/// drop order: IO, aggregate, tap).
///
/// Tap-only on purpose: with the default output as a subdevice, a duplex
/// interface (Galaxy32) makes `coreaudiod` ask for **Microphone** first
/// and the tap then blocks behind that prompt (seen on macOS 27). The
/// Microphone prompt is requested separately through `AVFoundation`.
/// Creating the tap is what raises the System Audio Recording prompt,
/// and it blocks until the user answers; IO failing afterwards (a
/// tap-only aggregate may not clock) doesn't matter.
fn probe_capture() -> Result<(), HostError> {
    let own = i32::try_from(std::process::id())
        .ok()
        .and_then(|pid| hal::process_for_pid(pid).ok().flatten());
    let excluded: Vec<u32> = own.into_iter().collect();
    let tap = ProcessTap::create_global(&excluded, Mute::Unmuted, "Patchbay permission check")?;
    let aggregate = AggregateDevice::create_private(
        "Patchbay permission check",
        &format!(
            "app.fasttrackstudio.patchbay.permission-check.{}",
            tap.uid()
        ),
        None,
        tap.uid(),
    )?;
    match IoProc::create(aggregate.id(), Arc::new(Silence)) {
        Ok(mut io) => {
            if let Err(e) = io.start() {
                tracing::debug!("permission probe: IO did not start: {e}");
            }
            std::thread::sleep(PROBE_RUN);
            drop(io);
        }
        Err(e) => tracing::debug!("permission probe: no IOProc: {e}"),
    }
    drop(aggregate);
    drop(tap);
    Ok(())
}

/// Current Microphone authorization (never prompts).
#[must_use]
pub fn microphone_permission() -> MicrophonePermission {
    match sys_perm::mic_status() {
        MicStatus::Authorized => MicrophonePermission::Granted,
        MicStatus::Denied => MicrophonePermission::Denied,
        MicStatus::Restricted => MicrophonePermission::Restricted,
        MicStatus::NotDetermined => MicrophonePermission::NotDetermined,
        MicStatus::Unknown => MicrophonePermission::Unknown,
    }
}

/// Ask for Microphone access if it is still undecided (shows the system
/// prompt and waits up to 60 s for the answer); otherwise returns the
/// current state.
pub async fn request_microphone_permission() -> MicrophonePermission {
    let now = microphone_permission();
    if now != MicrophonePermission::NotDetermined {
        return now;
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    let tx = std::sync::Mutex::new(Some(tx));
    let asked = sys_perm::mic_request(move |granted| {
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(granted);
        }
    });
    if !asked {
        return MicrophonePermission::Unknown;
    }
    let _ = tokio::time::timeout(ANSWER_TIMEOUT, rx).await;
    microphone_permission()
}

fn open_first(urls: &[&str]) -> bool {
    urls.iter().any(|url| sys_perm::open_url(url))
}

/// Open System Settings at Privacy & Security → Screen & System Audio
/// Recording. `false` if no URL could be opened.
#[must_use]
pub fn open_capture_settings() -> bool {
    open_first(&CAPTURE_SETTINGS_URLS)
}

/// Open System Settings at Privacy & Security → Microphone.
#[must_use]
pub fn open_microphone_settings() -> bool {
    open_first(&MICROPHONE_SETTINGS_URLS)
}

/// Show a native two-button alert (on the main thread, which must be
/// running an `AppKit` run loop) and wait for the answer without blocking
/// the caller's executor. `true` = `primary` was clicked.
pub async fn permission_alert(title: &str, message: &str, primary: &str, secondary: &str) -> bool {
    let (title, message, primary, secondary) = (
        title.to_owned(),
        message.to_owned(),
        primary.to_owned(),
        secondary.to_owned(),
    );
    run_blocking(Duration::MAX, move || {
        sys_perm::alert(&title, &message, &primary, &secondary)
    })
    .await
    .unwrap_or(false)
}

/// Run `f` on a fresh thread; `None` if it didn't finish within `timeout`
/// (it keeps running and its late result is dropped).
async fn run_blocking<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("patchbay-permission".to_owned())
        .spawn(move || {
            let _ = tx.send(f());
        });
    if spawned.is_err() {
        return None;
    }
    if timeout == Duration::MAX {
        return rx.await.ok();
    }
    tokio::time::timeout(timeout, rx).await.ok()?.ok()
}
