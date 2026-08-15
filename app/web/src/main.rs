//! Patchbay browser remote — the exact same [`patchbay_ui::PatchbayApp`]
//! the desktop shell mounts, connected over vox WebSocket instead of an
//! in-process link. Served BY the engine (fts-patchbay embeds this
//! bundle behind `--features embed-web`), so any device on the LAN
//! opens `http://<host>:4046/` and gets the patchbay.

use std::rc::Rc;
use std::sync::Arc;

use dioxus::prelude::*;
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_proto::{GraphEvent, PatchbayServiceClient};
use patchbay_ui::{PatchbayApp, PatchbayHandle};

/// Same-origin `/vox` (the engine that served this page serves the
/// service too); a `dx serve` dev page on a non-4046 localhost port
/// falls back to the local engine.
#[cfg(target_arch = "wasm32")]
fn server_url() -> String {
    let derived = web_sys::window().and_then(|w| {
        let loc = w.location();
        let host = loc.host().ok()?;
        let hostname = loc.hostname().ok()?;
        let scheme = match loc.protocol().ok()?.as_str() {
            "https:" => "wss",
            _ => "ws",
        };
        let is_local = hostname == "localhost" || hostname == "127.0.0.1";
        if is_local && !host.ends_with(":4046") {
            return Some("ws://127.0.0.1:4046/vox".to_string());
        }
        Some(format!("{scheme}://{host}/vox"))
    });
    derived.unwrap_or_else(|| "ws://127.0.0.1:4046/vox".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn server_url() -> String {
    std::env::var("PATCHBAY_URL").unwrap_or_else(|_| "ws://127.0.0.1:4046/vox".to_string())
}

/// The connected clients, passed by props (wasm vox clients are !Send,
/// so no statics). Pointer equality — the connection never changes
/// identity without remounting.
#[derive(Clone)]
struct EngineHandles {
    handle: PatchbayHandle,
    stream: Rc<PatchbayServiceStreamClient>,
}

impl PartialEq for EngineHandles {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.handle.0, &other.handle.0) && Rc::ptr_eq(&self.stream, &other.stream)
    }
}

async fn connect() -> Result<EngineHandles, String> {
    let url = server_url();
    tracing::info!("patchbay-web: dialing {url}");
    let link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| format!("connect {url}: {e:?}"))?;
    tracing::info!("patchbay-web: link up, establishing service client");
    let client: PatchbayServiceClient = vox_core::initiator_on(link)
        .establish()
        .await
        .map_err(|e| format!("establish service: {e:?}"))?;
    tracing::info!("patchbay-web: service client established");
    let stream_link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| format!("connect (stream) {url}: {e:?}"))?;
    let stream: PatchbayServiceStreamClient = vox_core::initiator_on(stream_link)
        .establish()
        .await
        .map_err(|e| format!("establish stream: {e:?}"))?;
    Ok(EngineHandles {
        handle: PatchbayHandle(Arc::new(client)),
        stream: Rc::new(stream),
    })
}

fn main() {
    dioxus::logger::initialize_default();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut engine = use_signal(|| None::<EngineHandles>);
    let mut error = use_signal(String::new);

    use_future(move || async move {
        match connect().await {
            Ok(handles) => engine.set(Some(handles)),
            Err(e) => error.set(e),
        }
    });

    if let Some(handles) = engine.read().clone() {
        return rsx! { Connected { engine: handles } };
    }
    rsx! {
        div {
            style: "display:flex;align-items:center;justify-content:center;\
                    width:100vw;height:100vh;background:#14171c;color:#7a8494;\
                    font-family:system-ui;font-size:14px;",
            if error.read().is_empty() {
                "connecting to the patchbay engine…"
            } else {
                "engine unreachable: {error}"
            }
        }
    }
}

/// Mounted once the clients exist: provides the handle, bridges the
/// event stream, reconciles periodically — the same wiring as the
/// desktop shell.
#[component]
fn Connected(engine: EngineHandles) -> Element {
    use_context_provider(|| engine.handle.clone());

    let bridge = engine.clone();
    use_future(move || {
        let engine = bridge.clone();
        async move {
            let (tx, mut rx) = vox::channel::<GraphEvent>();
            let stream = engine.stream.clone();
            spawn(async move {
                if let Err(e) = stream.graph_events(tx).await {
                    tracing::warn!("graph_events subscription ended: {e:?}");
                }
            });

            patchbay_ui::refresh_all(&engine.handle).await;

            while let Ok(Some(ev)) = rx.recv().await {
                let ev = ev.get();
                patchbay_ui::apply_graph_event(ev);
                if matches!(ev, GraphEvent::Reset) {
                    patchbay_ui::refresh_all(&engine.handle).await;
                }
            }
            tracing::warn!("graph event stream ended");
        }
    });

    // Periodic reconcile — streams can drop under burst.
    let reconcile = engine.clone();
    use_future(move || {
        let engine = reconcile.clone();
        async move {
            loop {
                patchbay_ui::sleep_secs(10).await;
                match engine.handle.0.graph().await {
                    Ok(snap) => patchbay_ui::replace_graph(snap),
                    Err(e) => tracing::warn!("graph reconcile failed: {e:?}"),
                }
            }
        }
    });

    rsx! {
        PatchbayApp {}
    }
}
