//! `patchbay-ios`: Patchbay on an iPhone or an iPad, painted by Blitz into
//! winit's `UIKit` window, as architect's `apps/mobile` draws the Architect
//! app (the `UIScene` adoption iOS 27 requires, the page under the safe
//! area with its bars kept clear of it).
//!
//! The window is Patchbay's own view, `patchbay::home::Home`: the view the
//! Patchbay distribution shows inside Architect's window (`patchbay-app`),
//! here the whole window. Not Architect's shell around it: that opens on
//! Architect's vault welcome, which a routing controller has no use for.
//!
//! A phone has no device families of its own (no `PipeWire`, no Core Audio
//! routing, no driver), so it is a controller: the engine `Home` starts in
//! process holds only its address book (the hosts saved, and the ones
//! Bonjour finds as `_patchbay._tcp`) and listens on loopback, so it is
//! neither advertised nor reachable from the network
//! (`patchbay::server::devices::registry::builtin` has no family there).
//! Bonjour is the engine's own (mdns-sd, raw multicast): the Simulator
//! browses with the Mac's network, a device only with Apple's multicast
//! entitlement (`com.apple.developer.networking.multicast`), which this
//! build has not got; without it a phone's list is the hosts added by name.
//! Route and Mix offer the hosts to connect to until one is open
//! (`patchbay::ui::settings::ConnectView`). `ios/build-app.sh` builds it;
//! off iOS it runs as a plain window, so `cargo check` holds everywhere.

use dioxus::prelude::*;
use dioxus_native::{Config, WindowAttributes, launch_cfg};

/// The window's title, the home screen's name.
const NAME: &str = "Patchbay";

#[component]
fn App() -> Element {
    // winit's window joins the app's scene, without which iOS 27 never
    // shows it (`app_shell::ios_scene`, the one every Studio app shares).
    #[cfg(target_os = "ios")]
    {
        let window = dioxus_native::use_window();
        use_hook(move || {
            use dioxus_native::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::UiKit(uikit) = handle.as_raw()
            {
                app_shell::ios_scene::window_created(uikit.ui_view);
            }
        });
    }
    let safe = use_safe_area();
    rsx! {
        document::Style { {PAGE_CSS} }
        div { class: "patchbay-ios", style: "{safe}", patchbay::home::Home {} }
    }
}

/// The window is the page: it fills it, edge to edge. The root takes its
/// `--safe-*` tokens from the wrapper ([`use_safe_area`]), not from its own
/// sheet's `env()`.
const PAGE_CSS: &str = "html, body { margin: 0; width: 100%; height: 100%; } \
     .patchbay-ios { width: 100vw; height: 100vh; } \
     .patchbay-ios .patchbay-root { width: 100vw; height: 100vh; \
     --safe-t: inherit; --safe-r: inherit; --safe-b: inherit; --safe-l: inherit; }";

/// The system's safe area (the status bar, the camera housing, the home
/// indicator) as Patchbay's own `--safe-*` tokens, which its phone layout
/// keeps its bars clear of (a browser fills them from `env()`, which Blitz
/// has not got). An inline style on the wrapper: Blitz applies a changed
/// attribute, not a changed `<style>`. The page is drawn under all of it
/// (`BLITZ_SAFE_AREA=0`), so the theme's background reaches every edge.
/// Re-measured when the window resizes, rescales or redraws: the safe area
/// moves without a resize when the phone turns.
fn use_safe_area() -> Signal<String> {
    let window = dioxus_native::use_window();
    let first = window.clone();
    let mut css = use_signal(move || safe_area_css(&*first));
    dioxus_native::use_window_event(move |event, _| {
        use dioxus_native::winit::event::WindowEvent;
        if matches!(
            event,
            WindowEvent::SurfaceResized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::RedrawRequested
        ) {
            let now = safe_area_css(&*window);
            if *css.peek() != now {
                css.set(now);
            }
        }
    });
    css
}

fn safe_area_css(window: &dyn dioxus_native::winit::window::Window) -> String {
    let scale = window.scale_factor().max(0.1);
    let safe = window.safe_area();
    let px = |v: u32| f64::from(v) / scale;
    format!(
        "--safe-t: {}px; --safe-r: {}px; --safe-b: {}px; --safe-l: {}px;",
        px(safe.top),
        px(safe.right),
        px(safe.bottom),
        px(safe.left)
    )
}

/// Where the engine keeps its config (the address book) and the UI its
/// preferences on a phone: the app's own Application Support, which the
/// sandbox lets it write.
#[cfg(target_os = "ios")]
fn engine_config() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support/fts/patchbay/patchbay.styx"),
    )
}

fn main() {
    // The page is the whole window; Patchbay keeps its bars clear of the
    // safe area itself (`use_safe_area`).
    // SAFETY: first thing in main, before any thread starts.
    unsafe { std::env::set_var("BLITZ_SAFE_AREA", "0") };
    // The engine on a phone: loopback, any free port, so it is neither
    // advertised on the LAN (`peers::start` advertises only a LAN bind)
    // nor joined to whatever holds Patchbay's port (the Simulator shares
    // the Mac's network). Its config in the app's container.
    // SAFETY: first thing in main, before any thread starts.
    #[cfg(target_os = "ios")]
    unsafe {
        if std::env::var_os("PATCHBAY_ADDR").is_none() {
            std::env::set_var("PATCHBAY_ADDR", "127.0.0.1:0");
        }
        if std::env::var_os("PATCHBAY_CONFIG").is_none()
            && let Some(config) = engine_config()
        {
            std::env::set_var("PATCHBAY_CONFIG", config);
        }
    }
    // Console logs (the Simulator's stderr, a device's console), as
    // `fts-patchbay`'s; OTLP only when its endpoint is set, which nothing
    // on a phone sets.
    let _telemetry = architect_telemetry::init_tracing_full("patchbay-ios", "info");
    // The family extensions' views, so another machine's console draws
    // here; their families never run on a phone (`patchbay::family`'s gate,
    // and this build has only their `ui` side).
    patchbay_yamaha_tf::register();
    #[cfg(target_os = "ios")]
    app_shell::ios_scene::register(app_shell::ios_scene::Opening::AsHeld);
    let window = WindowAttributes::default().with_title(NAME);
    launch_cfg(
        App,
        vec![],
        vec![Box::new(Config::new().with_window_attributes(window))],
    );
}
