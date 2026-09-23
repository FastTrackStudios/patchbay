//! Patchbay browser remote — the exact same [`patchbay_ui::PatchbayApp`]
//! the desktop shell mounts, connected over vox WebSocket instead of an
//! in-process link. Served BY the engine (fts-patchbay embeds this
//! bundle behind `--features embed-web`), so any device on the LAN
//! opens `http://<host>:4046/` and gets the patchbay.

use std::rc::Rc;
use std::sync::Arc;

use dioxus::prelude::*;
use patchbay_proto::PatchbayServiceClient;
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_ui::{Dialer, EngineLink, PatchbayApp, PatchbayHandle, Shell};

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

/// The home connection as a prop: equal when it is the same connection
/// (wasm vox clients are !Send, so no statics).
#[derive(Clone)]
struct Home(EngineLink);

impl PartialEq for Home {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0.handle.0, &other.0.handle.0)
            && Rc::ptr_eq(&self.0.stream, &other.0.stream)
    }
}

/// Dial one engine: a client for calls, a second link for its event
/// streams. Used for home and — through the [`Dialer`] handed to the UI —
/// for every other engine it switches to.
async fn connect(url: String) -> Result<EngineLink, String> {
    tracing::info!("patchbay-web: dialing {url}");
    let link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| format!("connect {url}: {e:?}"))?;
    let client: PatchbayServiceClient = vox_core::initiator_on(link)
        .establish()
        .await
        .map_err(|e| format!("establish service: {e:?}"))?;
    let stream_link = vox_websocket::WsLink::connect(&url)
        .await
        .map_err(|e| format!("connect (stream) {url}: {e:?}"))?;
    let stream: PatchbayServiceStreamClient = vox_core::initiator_on(stream_link)
        .establish()
        .await
        .map_err(|e| format!("establish stream: {e:?}"))?;
    tracing::info!("patchbay-web: {url} established");
    Ok(EngineLink {
        handle: PatchbayHandle(Arc::new(client)),
        stream: Rc::new(stream),
    })
}

fn main() {
    dioxus::logger::initialize_default();
    dioxus::launch(App);
}

/// First retry delay after the link drops or a dial fails, and the cap
/// it backs off to. A phone that was asleep reconnects within a second
/// of waking; an engine that is down isn't hammered.
const RETRY_MIN_SECS: u64 = 1;
const RETRY_MAX_SECS: u64 = 8;

#[component]
fn App() -> Element {
    let mut engine = use_signal(|| None::<Home>);
    let mut error = use_signal(String::new);
    // Bumped to remount `Connected` on every new connection: its
    // context, streams and tasks all belong to one set of clients.
    let mut generation = use_signal(|| 0_u32);
    // Set by `Connected` when its event stream ends — a phone that slept,
    // an engine that restarted. The dial loop below picks it up.
    let mut lost = use_signal(|| false);

    use_future(move || async move {
        let mut delay = RETRY_MIN_SECS;
        loop {
            match connect(server_url()).await {
                Ok(link) => {
                    delay = RETRY_MIN_SECS;
                    error.set(String::new());
                    lost.set(false);
                    generation.with_mut(|g| *g = g.wrapping_add(1));
                    engine.set(Some(Home(link)));
                    while !*lost.peek() {
                        patchbay_ui::sleep_secs(1).await;
                    }
                    engine.set(None);
                    error.set("the connection to the engine dropped".to_owned());
                }
                Err(e) => error.set(e),
            }
            patchbay_ui::sleep_secs(delay).await;
            delay = delay.saturating_mul(2).min(RETRY_MAX_SECS);
        }
    });

    if let Some(home) = engine.read().clone() {
        return rsx! {
            Connected {
                key: "{generation}",
                home,
                on_lost: move |()| lost.set(true),
            }
        };
    }
    let failed = !error.read().is_empty();
    rsx! {
        patchbay_ui::Splash {
            title: if failed { "Reconnecting…" } else { "Connecting…" },
            detail: if failed { format!("{error} — trying again.") } else { server_url() },
            failed,
        }
    }
}

/// Mounted once home is connected: hands the UI its engine, a way to
/// dial others, and a way to say the link dropped. Everything else —
/// bridging event streams, switching engines — is the UI crate's.
#[component]
fn Connected(home: Home, on_lost: EventHandler<()>) -> Element {
    use_context_provider(|| Shell {
        home: home.0.clone(),
        dial: Some(Dialer(Rc::new(|url| Box::pin(connect(url))))),
        on_home_lost: Callback::new(move |()| on_lost.call(())),
    });
    rsx! {
        PatchbayApp {}
    }
}
