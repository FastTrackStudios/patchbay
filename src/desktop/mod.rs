//! FTS Patchbay — the `PipeWire` studio-routing desktop app.
//!
//! The [`patchbay::server::PatchbayBackend`] engine runs in-process and is ALSO
//! served at `ws://0.0.0.0:4046/vox` (override `PATCHBAY_ADDR`), so any
//! browser/tablet on the LAN can drive the same graph later. The UI
//! talks to it through the exact same generated client every remote
//! uses — over an in-process `architect::LocalServer` link.

use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::sync::Arc;

use architect::host::WebBundle;
use dioxus::prelude::*;
use patchbay::server::{EngineOptions, IfTaken};

#[cfg(target_os = "macos")]
mod macos;

/// The staged web bundle, compiled into the binary (`just
/// patchbay-web-stage` copies the dx web build into `web-dist/`).
#[cfg(feature = "embed-web")]
static EMBEDDED_WEB: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/web-dist");

/// Locate the browser-remote bundle so the engine serves it itself —
/// any device on the LAN opens `http://<host>:4046/` and gets the
/// patchbay. First match wins: `PATCHBAY_WEB_DIST` env override, the
/// embedded bundle (feature `embed-web`), then the dx dev build output.
/// `None` = headless (`/health` + `/vox` only).
fn web_bundle() -> Option<WebBundle> {
    if let Ok(dir) = std::env::var("PATCHBAY_WEB_DIST") {
        let p = std::path::PathBuf::from(&dir);
        if p.join("index.html").is_file() {
            return Some(WebBundle::Dir(p));
        }
        tracing::warn!("PATCHBAY_WEB_DIST={dir} has no index.html — ignoring");
    }

    #[cfg(feature = "embed-web")]
    {
        return Some(WebBundle::Embedded(&EMBEDDED_WEB));
    }

    #[cfg(not(feature = "embed-web"))]
    {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf))?;
        let candidates = [
            exe_dir.join("../dx/fts-patchbay/release/web/public"),
            exe_dir.join("../dx/fts-patchbay/debug/web/public"),
        ];
        candidates
            .into_iter()
            .find(|p| p.join("index.html").is_file())
            .map(WebBundle::Dir)
    }
}

/// How this app reaches the machine's engine: it joins a Patchbay engine
/// already serving this machine's listen address
/// (`patchbay::server::listen_addr`: the engine config decides, not this
/// app) or runs it here and serves it. A port held by something that
/// answers and is not Patchbay is logged and the engine runs unserved, as
/// this app always did; one held by a Patchbay that does not answer yet is
/// waited for, never doubled.
fn engine_options() -> EngineOptions {
    let mut opts = EngineOptions::machine();
    opts.web = web_bundle();
    opts.if_taken = IfTaken::RunUnserved;
    // macOS privacy permissions belong to Patchbay.app: the app answers
    // the `permissions` RPCs and runs the prompt flow once the window is
    // up (see `App`).
    #[cfg(target_os = "macos")]
    {
        opts.permissions = Some(Box::new(|runtime| {
            Arc::new(macos::AppPermissions::install(runtime))
        }));
    }
    opts
}

pub fn main() -> ExitCode {
    // Patchbay.app from Finder: cwd `/`, bare PATH, no stdout. Fix up
    // before any thread exists.
    #[cfg(target_os = "macos")]
    macos::prepare_env();

    // WebKitGTK on NVIDIA/Wayland lags hard; force X11 before any GTK
    // init (same workaround as apps/fasttrackstudio).
    #[cfg(target_os = "linux")]
    {
        // SAFETY: called before any threads are spawned.
        unsafe {
            std::env::set_var("GDK_BACKEND", "x11");
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    // Console logs (RUST_LOG-filtered fmt, same as host::init_tracing) plus
    // OTLP export of traces/logs/metrics when OTEL_EXPORTER_OTLP_ENDPOINT is
    // set (http/protobuf → the local collector on :4318). Guards are leaked —
    // main hands control to the dioxus event loop, which never returns.
    let (sentry_guard, otel_guard) = architect_telemetry::init_tracing_full("fts-patchbay", "info");
    if let Some(g) = sentry_guard {
        std::mem::forget(g);
    }
    if let Some(g) = otel_guard {
        std::mem::forget(g);
    }

    // The device families that are extensions of their own, before the
    // engine starts and the window draws.
    patchbay_yamaha_tf::register();

    // Start or join this machine's engine before the window opens, so a
    // failure is an exit code rather than an empty window.
    patchbay::home::configure(engine_options);
    // A WebView is a browser: preferences in `localStorage`, the system
    // scheme followed, the page's chrome painted.
    patchbay::home::configure_page(crate::page::browser);
    match patchbay::home::engine_blocking() {
        Ok(engine) => tracing::info!(mode = ?engine.mode(), "patchbay engine ready"),
        Err(e) => {
            eprintln!("patchbay engine failed to start: {e}");
            return ExitCode::FAILURE;
        }
    }

    let window = dioxus::desktop::WindowBuilder::new()
        .with_title("Patchbay")
        .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(1480.0, 940.0));
    dioxus::LaunchBuilder::new()
        .with_cfg(
            dioxus::desktop::Config::new()
                .with_window(window)
                .with_menu(None),
        )
        .launch(App);
    ExitCode::SUCCESS
}

/// The window: Patchbay over this machine's engine. When this app joined
/// an engine another app runs, it shows that engine (its graph, devices
/// and settings, as any remote does), and if that app quits it takes
/// the engine over (`patchbay::home::Home`).
#[component]
fn App() -> Element {
    // AppKit is registered now: safe to prompt for permissions (doing it
    // during launch races NSApplication's own registration).
    #[cfg(target_os = "macos")]
    use_hook(macos::AppPermissions::on_window_ready);

    rsx! {
        patchbay::home::Home {}
    }
}
