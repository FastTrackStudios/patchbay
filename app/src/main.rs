// Lint debt: workspace flipped dead_code/unused to warn (task cleanup);
// this crate predates that — burn down separately.
#![allow(dead_code, unused)]

//! FTS Patchbay — the PipeWire studio-routing desktop app.
//!
//! The [`patchbay::PatchbayBackend`] engine runs in-process and is ALSO
//! served at `ws://0.0.0.0:4046/vox` (override `PATCHBAY_ADDR`), so any
//! browser/tablet on the LAN can drive the same graph later. The UI
//! talks to it through the exact same generated client every remote
//! uses — over an in-process `architect::LocalServer` link.

use std::sync::{Arc, OnceLock};

use architect::host::{self, EngineHost, WebBundle};
use dioxus::prelude::*;
use patchbay::PatchbayBackend;
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_proto::{GraphEvent, PatchbayServiceClient};
use patchbay_ui::{PatchbayApp, PatchbayHandle};

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
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))?;
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

/// In-process clients over the LocalServer conduit — the same shape
/// every network remote uses.
struct Engine {
    client: PatchbayServiceClient,
    stream_client: PatchbayServiceStreamClient,
    /// Keeps the LocalServer's acceptor + lanes alive.
    _scope: Arc<architect::Scope>,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

fn bind_addr() -> String {
    std::env::var("PATCHBAY_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string())
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
        tokio::spawn(async move {
            EngineHost::new(router, bind_addr())
                .web(web_bundle())
                .serve()
                .await;
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

fn main() {
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

    host::init_tracing("info");

    if let Err(e) = bootstrap_blocking() {
        eprintln!("patchbay engine failed to start: {e:?}");
        std::process::exit(1);
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
}

#[component]
fn App() -> Element {
    let engine = ENGINE.get().expect("engine bootstrapped in main");
    use_context_provider(|| PatchbayHandle(Arc::new(engine.client.clone())));

    // Bridge: initial snapshot + `#[subscribe]` events → UI signals.
    use_future(move || async move {
        let engine = ENGINE.get().expect("engine bootstrapped in main");
        let handle = PatchbayHandle(Arc::new(engine.client.clone()));

        // Consume the stream through the stream client so the vox lane
        // pumps it (a raw Tx attached to the hub is never drained).
        let (tx, mut rx) = vox::channel::<GraphEvent>();
        spawn(async move {
            let engine = ENGINE.get().expect("engine bootstrapped in main");
            if let Err(e) = engine.stream_client.graph_events(tx).await {
                tracing::warn!("graph_events subscription ended: {e:?}");
            }
        });

        patchbay_ui::refresh_all(&handle).await;

        while let Ok(Some(ev)) = rx.recv().await {
            let ev = ev.get();
            patchbay_ui::apply_graph_event(ev);
            // A Reset means the engine rebuilt its mirror (PipeWire
            // restart) — the follow-up flood can overrun any buffer,
            // so reconcile from the snapshot instead of trusting it.
            if matches!(ev, GraphEvent::Reset) {
                patchbay_ui::refresh_all(&handle).await;
            }
        }
        tracing::warn!("graph event stream ended");
    });

    // Belt-and-suspenders reconcile: streams can drop under burst
    // (an app connecting = hundreds of events at once); a periodic
    // snapshot swap guarantees the UI converges within seconds even
    // if the event path lost something.
    use_future(move || async move {
        let engine = ENGINE.get().expect("engine bootstrapped in main");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            match engine.client.graph().await {
                Ok(snap) => patchbay_ui::replace_graph(snap),
                Err(e) => tracing::warn!("graph reconcile failed: {e:?}"),
            }
        }
    });

    rsx! {
        PatchbayApp {}
    }
}
