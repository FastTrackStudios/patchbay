//! What only the page a UI is drawn in can do, given by the shell.
//!
//! In a browser (the browser remote, the desktop app's webview) that is
//! `localStorage`, the system's colour scheme and the page's own chrome,
//! done with JavaScript by the shell (`apps/patchbay`). Architect's window
//! (Blitz) has no JavaScript: its shell keeps preferences in a file and
//! says the page is native, which also turns on the sheet for Blitz's
//! gaps (`theme/native.css`). The components here only ask; they never
//! reach for a page API themselves.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use dioxus::prelude::*;

/// A future on the UI's executor.
pub type PageFuture<T> = Pin<Box<dyn Future<Output = T>>>;
/// Reads a saved preference.
pub type LoadPref = Rc<dyn Fn(&'static str) -> PageFuture<Option<String>>>;
/// Saves a preference.
pub type SavePref = Rc<dyn Fn(&'static str, String)>;

/// What the page can do. [`Page::default`] can do nothing: preferences
/// are not kept, the scheme is dark, and the pointer is a mouse.
#[derive(Clone, Default)]
pub struct Page {
    /// Drawn by Blitz (Architect's window) rather than a browser.
    pub native: bool,
    /// Its home engine has no device families of its own (a phone or a
    /// tablet): it is a controller of other machines' engines, and Route
    /// and Mix offer the hosts to connect to until one of them is live.
    pub controller: bool,
    /// Read a saved preference (`None`: never saved).
    pub load: Option<LoadPref>,
    /// Save a preference.
    pub save: Option<SavePref>,
    /// Follow the system's colour scheme: `on` gets `true` for light, now
    /// and on every change.
    pub watch_scheme: Option<Rc<dyn Fn(Callback<bool>)>>,
    /// Paint what is outside the app root (a browser's page background
    /// and `theme-color`) in the root's colours.
    pub paint_chrome: Option<Rc<dyn Fn()>>,
    /// Whether the pointer is coarse (a finger): touch-first defaults.
    pub coarse_pointer: Option<Rc<dyn Fn() -> PageFuture<bool>>>,
}

impl Page {
    /// Read preference `key`.
    // UI futures run on the UI's own executor; they are never sent.
    #[allow(clippy::future_not_send)]
    pub async fn load(&self, key: &'static str) -> Option<String> {
        match &self.load {
            Some(load) => load(key).await,
            None => None,
        }
    }

    /// Save preference `key`.
    pub fn save(&self, key: &'static str, value: String) {
        if let Some(save) = &self.save {
            save(key, value);
        }
    }

    /// Whether the pointer is coarse; `false` when the page can't say.
    #[allow(clippy::future_not_send)]
    pub async fn coarse_pointer(&self) -> bool {
        match &self.coarse_pointer {
            Some(ask) => ask().await,
            None => false,
        }
    }
}

/// The page the shell gave, or one that can do nothing.
#[must_use]
pub fn use_page() -> Page {
    try_use_context::<crate::ui::Shell>()
        .map(|shell| shell.page)
        .unwrap_or_default()
}
