//! This process's way to this machine's Patchbay engine, and the view
//! over it: shared by `fts-patchbay` and the Architect extension
//! (`crate::ext`).
//!
//! [`engine`] holds the process's [`Engine`]: the one it started, or the
//! one it joined. When a joined engine goes away (its app quit), the next
//! call starts or joins again, so a survivor takes the port and nobody
//! is left talking to nothing. [`Home`] draws `PatchbayApp` over it and
//! does the same when its link ends.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use dioxus::prelude::*;

use crate::server::{Engine, EngineOptions};
use crate::ui::{Dialer, EngineLink, Page, PatchbayApp, PatchbayHandle, Shell};

/// The page this process draws in; [`native_page`] unless the app says
/// otherwise ([`configure_page`]).
static PAGE: OnceLock<fn() -> Page> = OnceLock::new();

/// Set the page this process draws in (a browser shell's, with its
/// JavaScript). The first call wins; call it before [`Home`] mounts.
pub fn configure_page(page: fn() -> Page) {
    let _ = PAGE.set(page);
}

/// Architect's window (Blitz): no JavaScript, so preferences live in a
/// file beside the engine's config ([`crate::server::ui_prefs_path`]),
/// the scheme is the default, and the page says it is native.
#[must_use]
pub fn native_page() -> Page {
    Page {
        native: true,
        load: Some(Rc::new(|key: &'static str| {
            let value = read_prefs().remove(key);
            Box::pin(async move { value })
        })),
        save: Some(Rc::new(|key: &'static str, value: String| {
            let mut prefs = read_prefs();
            if prefs.get(key) == Some(&value) {
                return;
            }
            prefs.insert(key.to_owned(), value);
            write_prefs(&prefs);
        })),
        ..Page::default()
    }
}

type Prefs = std::collections::BTreeMap<String, String>;

fn read_prefs() -> Prefs {
    std::fs::read_to_string(crate::server::ui_prefs_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Best effort, like a browser's `localStorage`: a preference that can't
/// be saved is logged and the UI carries on.
fn write_prefs(prefs: &Prefs) {
    let path = crate::server::ui_prefs_path();
    let written = serde_json::to_string_pretty(prefs)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, text).map_err(|e| e.to_string())
        });
    if let Err(e) = written {
        tracing::warn!(path = %path.display(), error = %e, "patchbay: could not save UI preferences");
    }
}

/// How this process starts its engine; [`EngineOptions::machine`] (this
/// machine's listen address) unless the app says otherwise
/// ([`configure`]).
static OPTIONS: OnceLock<fn() -> EngineOptions> = OnceLock::new();

/// Set how this process starts its engine (the address, its web bundle,
/// its permission flow). The first call wins; call it before [`engine`].
pub fn configure(options: fn() -> EngineOptions) {
    let _ = OPTIONS.set(options);
}

fn options() -> EngineOptions {
    OPTIONS
        .get()
        .map_or_else(EngineOptions::machine, |make| make())
}

/// This machine's engine, as this process reaches it: started here, or
/// joined.
///
/// A start runs on the engine runtime; the caller only awaits it (one
/// start at a time). A failed start is tried again by the next call, and
/// so is one whose joined engine has gone away.
///
/// # Errors
/// The port is held by something that is not a Patchbay engine, or the
/// engine could not start.
// The lock is held across the start on purpose: one start at a time.
#[allow(clippy::significant_drop_tightening)]
pub async fn engine() -> Result<Arc<Engine>, String> {
    static ENGINE: tokio::sync::Mutex<Option<Arc<Engine>>> = tokio::sync::Mutex::const_new(None);
    let mut slot = ENGINE.lock().await;
    if let Some(engine) = slot.as_ref().filter(|e| !e.is_lost()) {
        return Ok(Arc::clone(engine));
    }
    if slot.take().is_some() {
        tracing::warn!("patchbay: the engine this app joined is gone; starting or joining again");
    }
    let engine = Arc::new(
        Engine::start_or_attach(options())
            .await
            .map_err(|e| e.to_string())?,
    );
    tracing::info!(mode = ?engine.mode(), "patchbay engine ready");
    *slot = Some(Arc::clone(&engine));
    Ok(engine)
}

/// [`engine`] for a caller that is not on a runtime (an app's `main`).
///
/// # Errors
/// As [`engine`].
///
/// # Panics
/// When called from inside a tokio runtime.
pub fn engine_blocking() -> Result<Arc<Engine>, String> {
    crate::server::runtime()
        .map_err(|e| e.to_string())?
        .block_on(engine())
}

/// `PatchbayApp` over this machine's engine, able to switch to other
/// machines' engines. When the link to the engine ends, it reaches the
/// engine again (joining the survivor, or taking over).
#[component]
pub fn Home() -> Element {
    let generation = use_signal(|| 0_u64);
    let started = use_resource(move || async move {
        // Read so that a new generation asks for the engine again.
        let _ = generation();
        engine().await.map(EngineRef)
    });
    let state = started.read().clone();
    match state {
        None => rsx! {
            div { style: "padding: 24px;", "Starting Patchbay…" }
        },
        Some(Err(e)) => rsx! {
            div { style: "padding: 24px; display: flex; flex-direction: column; gap: 8px;",
                div { style: "font-weight: 600;", "Patchbay's engine did not start" }
                div { "{e}" }
            }
        },
        Some(Ok(engine)) => rsx! {
            Linked { key: "{generation}", engine, generation }
        },
    }
}

/// An engine as a prop: equal when it is the same engine.
#[derive(Clone)]
struct EngineRef(Arc<Engine>);

impl PartialEq for EngineRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[component]
fn Linked(engine: EngineRef, generation: Signal<u64>) -> Element {
    use_context_provider(|| shell(&engine.0, generation));
    rsx! {
        PatchbayApp {}
    }
}

/// The view's shell: home is this machine's engine, other machines are
/// dialled on the engine runtime, and a lost home starts a new generation.
fn shell(engine: &Arc<Engine>, mut generation: Signal<u64>) -> Shell {
    let lost = Arc::clone(engine);
    Shell {
        page: PAGE.get().map_or_else(native_page, |make| make()),
        home: EngineLink {
            handle: PatchbayHandle(Arc::new(engine.client().clone())),
            stream: Rc::new(engine.stream_client().clone()),
        },
        dial: Some(Dialer(Rc::new(|url: String| {
            Box::pin(async move {
                let (client, stream) =
                    crate::server::spawn(async move { crate::server::dial(&url).await }).await??;
                Ok(EngineLink {
                    handle: PatchbayHandle(Arc::new(client)),
                    stream: Rc::new(stream),
                })
            })
        }))),
        on_home_lost: Callback::new(move |()| {
            tracing::warn!("patchbay: the link to this machine's engine ended; reaching it again");
            lost.mark_lost();
            let next = generation.peek().wrapping_add(1);
            generation.set(next);
        }),
    }
}
