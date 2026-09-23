//! FTS Patchbay — the `PipeWire` studio-routing desktop app.
//!
//! The [`patchbay::PatchbayBackend`] engine runs in-process and is ALSO
//! served at `ws://0.0.0.0:4046/vox` (override `PATCHBAY_ADDR`), so any
//! browser/tablet on the LAN can drive the same graph later. The UI
//! talks to it through the exact same generated client every remote
//! uses — over an in-process `architect::LocalServer` link.

use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use architect::host::{EngineHost, WebBundle};
use dioxus::prelude::*;
use patchbay::PatchbayBackend;
use patchbay_proto::PatchbayServiceClient;
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_ui::{Dialer, EngineLink, PatchbayApp, PatchbayHandle, Shell};

#[cfg(target_os = "macos")]
mod macos;

const DEFAULT_ADDR: &str = "0.0.0.0:4046";

/// The staged web bundle, compiled into the binary (`just
/// patchbay-web-stage` copies the dx build into `web-dist/`).
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
            exe_dir.join("../dx/patchbay-web/release/web/public"),
            exe_dir.join("../dx/patchbay-web/debug/web/public"),
        ];
        candidates
            .into_iter()
            .find(|p| p.join("index.html").is_file())
            .map(WebBundle::Dir)
    }
}

/// In-process clients over the `LocalServer` conduit — the same shape
/// every network remote uses.
struct Engine {
    client: PatchbayServiceClient,
    stream_client: PatchbayServiceStreamClient,
    /// Keeps the `LocalServer`'s acceptor + lanes alive.
    _scope: Arc<architect::Scope>,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// The bootstrapped engine. `main` refuses to reach the dioxus launch
/// unless `bootstrap_blocking` succeeded, so this is set for the whole
/// life of the UI; a `None` here can only mean the component tree was
/// mounted without `main`, which the UI treats as "nothing to show".
fn bootstrapped() -> Option<&'static Engine> {
    ENGINE.get()
}

fn bind_addr() -> String {
    // Patchbay.app defaults to loopback (the RPC is unauthenticated).
    // `PATCHBAY_ADDR` wins, then the saved `network.bind` (set with
    // `patchbay listen lan`), then the default.
    #[cfg(target_os = "macos")]
    let default = macos::default_addr(DEFAULT_ADDR);
    #[cfg(not(target_os = "macos"))]
    let default = DEFAULT_ADDR;
    std::env::var("PATCHBAY_ADDR")
        .ok()
        .or_else(patchbay::configured_bind)
        .unwrap_or_else(|| default.to_string())
}

/// Bring up the backend + serving before the UI launches. The runtime
/// is leaked so it keeps hosting the ws server, the stream pumps, and
/// the local acceptor for the life of the process.
fn bootstrap_blocking() -> eyre::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt = Box::leak(Box::new(rt));

    rt.block_on(async {
        let backend = PatchbayBackend::new();
        // macOS privacy permissions belong to Patchbay.app: the app
        // answers the `permissions` RPCs and runs the prompt flow once
        // the window is up (see `App`).
        #[cfg(target_os = "macos")]
        backend.set_permission_provider(Arc::new(macos::AppPermissions::install(
            tokio::runtime::Handle::current(),
        )));

        // In-process client link.
        let scope = architect::Scope::new();
        let server = architect::LocalServer::serve(backend.router(), Arc::clone(&scope));
        let caller = server
            .caller()
            .await
            .map_err(|e| eyre::eyre!("local patchbay caller: {e:?}"))?;
        let client = PatchbayServiceClient::new(caller);
        let stream_client = server
            .establish::<PatchbayServiceStreamClient>()
            .await
            .map_err(|e| eyre::eyre!("local patchbay stream client: {e:?}"))?;

        // Network serving for future remotes (browser/tablet). Spawned,
        // not awaited — serve() never returns.
        let router = backend.router();
        let addr = bind_addr();
        let web = web_bundle();
        // Record what we're serving so `patchbay listen` can report it
        // (and tell the user which URLs would actually answer).
        patchbay::record_bound(
            &addr,
            if web.is_some() {
                ""
            } else {
                "no browser remote in this build (install `dx` and rebuild, \
                 or set PATCHBAY_WEB_DIST) — /health and /vox still work"
            },
        );
        // Advertise to (when LAN-reachable) and look for other engines, so
        // a UI attached here can offer to switch to them.
        patchbay::start_peer_discovery(&addr);
        tokio::spawn(async move {
            EngineHost::new(router, addr).web(web).serve().await;
        });

        ENGINE
            .set(Engine {
                client,
                stream_client,
                _scope: scope,
            })
            .map_err(|_| eyre::eyre!("engine already initialised"))?;
        Ok(())
    })
}

fn main() -> ExitCode {
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

    // Single instance: a second Patchbay.app (or one started next to
    // `patchbay serve`) would fight over the RPC port.
    #[cfg(target_os = "macos")]
    if macos::is_bundled() && macos::already_running(&bind_addr()) {
        tracing::error!(
            addr = bind_addr(),
            "another Patchbay is already serving this address; exiting"
        );
        return ExitCode::FAILURE;
    }

    if let Err(e) = bootstrap_blocking() {
        eprintln!("patchbay engine failed to start: {e:?}");
        return ExitCode::FAILURE;
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

/// Dial another machine's engine over WebSocket — the same two links a
/// browser remote opens. Home is in-process and never goes through this.
async fn dial(url: String) -> Result<EngineLink, String> {
    let link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;
    let client: PatchbayServiceClient = vox_core::initiator_on(link)
        .establish()
        .await
        .map_err(|e| format!("handshake with {url}: {e:?}"))?;
    let stream_link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| format!("connect (stream) {url}: {e}"))?;
    let stream: PatchbayServiceStreamClient = vox_core::initiator_on(stream_link)
        .establish()
        .await
        .map_err(|e| format!("handshake (stream) with {url}: {e:?}"))?;
    Ok(EngineLink {
        handle: PatchbayHandle(Arc::new(client)),
        stream: Rc::new(stream),
    })
}

#[component]
fn App() -> Element {
    let Some(engine) = bootstrapped() else {
        return rsx! { div { "patchbay engine not bootstrapped" } };
    };
    // All a shell provides: its own engine, and a way to reach others.
    // Bridging the event streams into the UI (and switching between
    // engines) is `patchbay_ui`'s.
    use_context_provider(|| Shell {
        home: EngineLink {
            handle: PatchbayHandle(Arc::new(engine.client.clone())),
            stream: Rc::new(engine.stream_client.clone()),
        },
        dial: Some(Dialer(Rc::new(|url| Box::pin(dial(url))))),
        // In-process: if this link ends, so has the app.
        on_home_lost: Callback::new(|()| tracing::error!("in-process engine link ended")),
    });

    // AppKit is registered now: safe to prompt for permissions (doing it
    // during launch races NSApplication's own registration).
    #[cfg(target_os = "macos")]
    use_hook(macos::AppPermissions::on_window_ready);

    rsx! {
        PatchbayApp {}
    }
}
