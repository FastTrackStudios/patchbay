//! macOS-only glue for running as `Patchbay.app` (see `packaging/macos/`).
//!
//! - **Bundled environment.** A Finder/Dock launch gets `cwd = /`, a
//!   minimal `PATH`, stdout/stderr going nowhere and launchd's soft limit
//!   of 256 open files. [`prepare_env`] fixes all four: cwd = `$HOME`,
//!   Homebrew/`/usr/local` on `PATH`, logs in
//!   `~/Library/Logs/Patchbay/patchbay.log` (previous run kept as
//!   `patchbay.log.1`), and a raised `RLIMIT_NOFILE` (LAN discovery
//!   probes hundreds of hosts at once; at 256 fds unrelated opens fail —
//!   `AppKit` aborted at launch reading `SystemVersion.plist`).
//! - **Loopback by default.** A bundled app serves RPC on
//!   `127.0.0.1:4046` unless `PATCHBAY_ADDR` says otherwise.
//! - **Single instance.** If something already listens on the RPC port,
//!   a second copy exits instead of running a second engine.
//! - **Permissions.** [`AppPermissions`] runs the flow once the window is
//!   up (touching TCC/AVFoundation while `AppKit` registers the app races
//!   it): undecided System Audio Recording / Microphone → system prompts;
//!   denied → a native alert offering System Settings (at most once per
//!   app version after "Not Now", or always when asked via
//!   `--request-permissions` / `patchbay permissions request`). It also
//!   answers the `permissions` RPC and writes `permissions.json` next to
//!   the config.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use patchbay::permissions::PermissionProvider;
use patchbay_host_coreaudio::{
    CapturePermission, MicrophonePermission, capture_permission, microphone_permission,
    open_capture_settings, open_microphone_settings, permission_alert, request_capture_permission,
    request_microphone_permission,
};
use patchbay_proto::PermissionsStatus;
use serde::{Deserialize, Serialize};

/// Re-run the permission flow even after "Not Now".
pub const REQUEST_PERMISSIONS_FLAG: &str = "--request-permissions";

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Open-file soft limit for the app (launchd gives GUI apps 256).
const WANT_NOFILE: libc::rlim_t = 10_240;

/// Running from inside an `.app` bundle (`…/X.app/Contents/MacOS/…`).
pub fn is_bundled() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.contains(".app/Contents/MacOS/")))
        .unwrap_or(false)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/Library/Application Support/fts/patchbay` — the engine's config dir
/// (`dirs::config_dir()` on macOS).
fn state_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Library/Application Support/fts/patchbay"))
}

/// Fix up a Finder-launched environment. Call first thing in `main`,
/// before any thread exists (it sets env vars).
pub fn prepare_env() {
    raise_fd_limit();
    if !is_bundled() {
        return;
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<&str> = path.split(':').filter(|d| !d.is_empty()).collect();
    for extra in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    // SAFETY: called at the top of `main`, before any threads are spawned.
    unsafe { std::env::set_var("PATH", dirs.join(":")) };
    if let Some(home) = home() {
        let _ = std::env::set_current_dir(home);
    }
    if !std::io::stderr().is_terminal() {
        redirect_output_to_log();
    }
}

/// Raise the soft `RLIMIT_NOFILE` to [`WANT_NOFILE`] (capped by the hard
/// limit). Best-effort.
fn raise_fd_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid out pointer for getrlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut lim) } != 0 {
        return;
    }
    let want = WANT_NOFILE.min(lim.rlim_max);
    if lim.rlim_cur >= want {
        return;
    }
    lim.rlim_cur = want;
    // SAFETY: `lim` is a valid, initialised rlimit.
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lim) };
}

/// Point stdout + stderr (and so the tracing console layer) at
/// `~/Library/Logs/Patchbay/patchbay.log`.
fn redirect_output_to_log() {
    use std::os::fd::{AsRawFd, IntoRawFd};
    let Some(dir) = home().map(|h| h.join("Library/Logs/Patchbay")) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let log = dir.join("patchbay.log");
    let _ = std::fs::rename(&log, dir.join("patchbay.log.1"));
    let Ok(file) = std::fs::File::create(&log) else {
        return;
    };
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid open file for the duration of the calls;
    // dup2 atomically replaces fds 1/2 with duplicates of it.
    unsafe {
        libc::dup2(fd, libc::STDOUT_FILENO);
        libc::dup2(fd, libc::STDERR_FILENO);
    }
    if fd <= libc::STDERR_FILENO {
        // The launcher had 1/2 closed and the log itself landed on one of
        // them: keep it open (dropping would close stdout/stderr again).
        let _ = file.into_raw_fd();
    }
}

/// Default RPC bind address: loopback only for the bundled app (the RPC
/// is unauthenticated; opt into the LAN with `PATCHBAY_ADDR=0.0.0.0:4046`).
pub fn default_addr(fallback: &'static str) -> &'static str {
    if is_bundled() {
        "127.0.0.1:4046"
    } else {
        fallback
    }
}

/// Another Patchbay (app or `patchbay serve`) already owns `addr`.
pub fn already_running(addr: &str) -> bool {
    matches!(
        std::net::TcpListener::bind(addr),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse
    )
}

// ─── Local Network ─────────────────────────────────────────────────────

/// A one-question mDNS query (PTR `_patchbay-lan-check._udp.local`) —
/// a name nobody answers, so the probe causes no replies.
const MDNS_PROBE: &[u8] = &[
    0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, // header: 1 question
    19, b'_', b'p', b'a', b't', b'c', b'h', b'b', b'a', b'y', b'-', b'l', b'a', b'n', b'-', b'c',
    b'h', b'e', b'c', b'k', 4, b'_', b'u', b'd', b'p', 5, b'l', b'o', b'c', b'a', b'l',
    0, // name
    0, 12, 0, 1, // PTR, IN
];

/// Local Network privacy has no query API: send one mDNS packet and see
/// whether macOS lets it out. The first attempt is also what makes macOS
/// ask the user. `granted` | `blocked` (denied, or not answered yet) |
/// `unknown` (no network, other errors).
fn local_network_probe() -> &'static str {
    let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return "unknown";
    };
    match socket.send_to(MDNS_PROBE, "224.0.0.251:5353") {
        Ok(_) => "granted",
        Err(e) if e.raw_os_error() == Some(libc::EHOSTUNREACH) => "blocked",
        Err(e) => {
            tracing::debug!("local network probe: {e}");
            "unknown"
        }
    }
}

// ─── Privacy permissions ───────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct PromptMemory {
    /// App version at which the user clicked "Not Now".
    dismissed_version: Option<String>,
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Atomic write: temp file in the same dir, then rename over.
fn write_json<T: Serialize>(path: &Path, value: &T) {
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        tracing::warn!("write {}: {e}", path.display());
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug, Clone, Copy)]
struct Known {
    capture: CapturePermission,
    mic: MicrophonePermission,
    checked_at: u64,
}

struct Shared {
    known: Mutex<Option<Known>>,
    requesting: AtomicBool,
    launched: AtomicBool,
    runtime: tokio::runtime::Handle,
}

/// The app's permission state + flow; registered with the engine as its
/// [`PermissionProvider`].
#[derive(Clone)]
pub struct AppPermissions {
    shared: Arc<Shared>,
}

static APP_PERMISSIONS: OnceLock<AppPermissions> = OnceLock::new();

impl AppPermissions {
    /// Create (once) with the runtime the flow should run on.
    pub fn install(runtime: tokio::runtime::Handle) -> Self {
        APP_PERMISSIONS
            .get_or_init(|| Self {
                shared: Arc::new(Shared {
                    known: Mutex::new(None),
                    requesting: AtomicBool::new(false),
                    launched: AtomicBool::new(false),
                    runtime,
                }),
            })
            .clone()
    }

    /// Start the launch-time flow, once, after the window is up. Only
    /// for the bundled app (a `cargo run` binary's grants would go to the
    /// terminal) unless `--request-permissions` was passed.
    pub fn on_window_ready() {
        let Some(this) = APP_PERMISSIONS.get() else {
            return;
        };
        if this.shared.launched.swap(true, Ordering::SeqCst) {
            return;
        }
        let force = std::env::args().any(|a| a == REQUEST_PERMISSIONS_FLAG);
        if force || is_bundled() {
            this.spawn_flow(force);
        }
    }

    fn spawn_flow(&self, force: bool) {
        if self.shared.requesting.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        self.shared.runtime.spawn(async move {
            this.flow(force).await;
            this.shared.requesting.store(false, Ordering::SeqCst);
        });
    }

    fn known(&self) -> Option<Known> {
        *self
            .shared
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn set_known(&self, known: Known) {
        *self
            .shared
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(known);
    }

    /// Check and, where undecided, request System Audio Recording and
    /// Microphone; offer System Settings for anything denied. `force`
    /// ignores an earlier "Not Now".
    async fn flow(&self, force: bool) {
        let mut capture = capture_permission();
        let mut mic = microphone_permission();
        tracing::info!(?capture, ?mic, "macOS permissions");
        // A previous run of the flow in this process knows better than
        // the (process-cached) preflight.
        if let Some(k) = self.known()
            && capture == CapturePermission::NotDetermined
        {
            capture = k.capture;
        }

        if capture == CapturePermission::NotDetermined {
            tracing::info!("requesting System Audio Recording");
            capture = request_capture_permission().await;
            tracing::info!(?capture, "System Audio Recording after request");
        }
        if mic == MicrophonePermission::NotDetermined {
            tracing::info!("requesting Microphone");
            mic = request_microphone_permission().await;
            tracing::info!(?mic, "Microphone after request");
        }
        let lan = local_network_probe();
        tracing::info!(local_network = lan, "Local Network probe");

        let known = Known {
            capture,
            mic,
            checked_at: unix_now(),
        };
        self.set_known(known);
        let status = Self::build_status(known, lan, false);
        let Some(dir) = state_dir() else { return };
        write_json(&dir.join("permissions.json"), &status);

        let capture_missing = capture == CapturePermission::Denied;
        let mic_missing = matches!(
            mic,
            MicrophonePermission::Denied | MicrophonePermission::Restricted
        );
        if !capture_missing && !mic_missing {
            return;
        }

        let memory_path = dir.join("permission-prompt.json");
        let memory: PromptMemory = read_json(&memory_path);
        if !force && memory.dismissed_version.as_deref() == Some(APP_VERSION) {
            tracing::info!("permission alert skipped (\"Not Now\" for this version)");
            return;
        }

        let (title, message) = if capture_missing {
            (
                "Patchbay needs System Audio Recording to route app audio",
                "Without it, audio captured from other apps is silent.\n\nIn System Settings → Privacy & Security → Screen & System Audio Recording, add Patchbay under \"System Audio Recording Only\", then relaunch Patchbay.",
            )
        } else {
            (
                "Patchbay needs Microphone access to route audio inputs",
                "Without it, microphones and audio-interface inputs are silent.\n\nIn System Settings → Privacy & Security → Microphone, turn on Patchbay, then relaunch Patchbay.",
            )
        };
        if permission_alert(title, message, "Open System Settings", "Not Now").await {
            let opened = if capture_missing {
                open_capture_settings()
            } else {
                open_microphone_settings()
            };
            tracing::info!(opened, "opened System Settings for permissions");
        } else {
            tracing::info!("permission alert: Not Now");
            write_json(
                &memory_path,
                &PromptMemory {
                    dismissed_version: Some(APP_VERSION.to_owned()),
                },
            );
        }
    }

    fn build_status(known: Known, lan: &str, requesting: bool) -> PermissionsStatus {
        let mut notes: Vec<String> = Vec::new();
        match known.capture {
            CapturePermission::Denied => notes.push(
                "System Audio Recording is off: System Settings → Privacy & Security → Screen & \
                 System Audio Recording → System Audio Recording Only → Patchbay, then relaunch"
                    .to_owned(),
            ),
            CapturePermission::NotDetermined => notes.push(
                "System Audio Recording not decided: run `patchbay permissions request` and \
                 answer the prompt"
                    .to_owned(),
            ),
            CapturePermission::Granted | CapturePermission::Unknown => {}
        }
        if matches!(
            known.mic,
            MicrophonePermission::Denied | MicrophonePermission::Restricted
        ) {
            notes.push(
                "Microphone is off: System Settings → Privacy & Security → Microphone → Patchbay"
                    .to_owned(),
            );
        }
        if lan == "blocked" {
            notes.push(
                "Local Network is blocked (or not answered yet): System Settings → Privacy & \
                 Security → Local Network → Patchbay; device discovery (Dante, TF) needs it"
                    .to_owned(),
            );
        }
        if requesting {
            notes.push("a permission request is in progress in the app".to_owned());
        }
        if notes.is_empty() {
            notes.push("all permissions granted".to_owned());
        }
        PermissionsStatus {
            platform: "macos".to_owned(),
            bundled: is_bundled(),
            system_audio_recording: known.capture.as_str().to_owned(),
            microphone: known.mic.as_str().to_owned(),
            local_network: lan.to_owned(),
            requesting,
            checked_at: known.checked_at,
            note: notes.join("; "),
        }
    }
}

impl PermissionProvider for AppPermissions {
    fn status(&self) -> PermissionsStatus {
        let requesting = self.shared.requesting.load(Ordering::SeqCst);
        let known = self.known().unwrap_or_else(|| Known {
            capture: capture_permission(),
            mic: microphone_permission(),
            checked_at: 0,
        });
        // AVFoundation tracks Microphone changes live; the capture
        // preflight is cached per process, so keep the flow's answer.
        let known = Known {
            mic: microphone_permission(),
            ..known
        };
        Self::build_status(known, local_network_probe(), requesting)
    }

    fn request(&self) {
        self.spawn_flow(true);
    }
}
