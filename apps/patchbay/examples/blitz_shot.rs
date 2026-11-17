//! The Patchbay views, drawn by Blitz into pictures: no window, no
//! compositor. The same renderer Architect's window uses (`blitz_paint`
//! into `anyrender_vello`), so a shot here and the view in Patchbay's
//! distribution are one renderer's output. Pattern from
//! `features/rigs/guitar/examples/rig_shot.rs`.
//!
//! ```sh
//! # a scratch engine (free port, scratch config, devices and PipeWire off)
//! nix develop -c cargo run -p fts-patchbay --features native --example blitz_shot -- /tmp/pb 1480 940
//! # this machine's running engine, read only (never starts one)
//! nix develop -c cargo run -p fts-patchbay --features native --example blitz_shot -- /tmp/pb 1480 940 --live ws://127.0.0.1:4046/vox
//! ```
//!
//! Writes `<dir>/route.png`, `mix.png`, `scenes.png`, `settings.png`
//! (`PATCHBAY_SHOT_VIEWS=route,mix` picks). The page is native (Blitz's
//! sheet on) and saves nothing, so a shot never writes preferences.
//!
//! `--window` opens the first view in a Blitz window of exactly W×H
//! instead (not resizable), to click and drag through the dropdowns and
//! sliders by hand. A scratch engine gets one mix, saved disabled (so it
//! never runs: no taps, no devices), whose strips have both.

// An example that takes pictures: a failure to set one up is a panic
// with its reason, and the arguments are taken as given.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::option_if_let_else,
    clippy::single_match_else
)]

use anyrender::ImageRenderer as _;
use anyrender_vello::VelloImageRenderer;
use blitz_dom::{Document as _, DocumentConfig};
use blitz_traits::shell::{ColorScheme, Viewport};
use dioxus::prelude::*;
use dioxus_native_dom::DioxusDocument;
use patchbay::proto::PatchbayServiceClient;
use patchbay::proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay::server::{Engine, EngineOptions};
use patchbay::ui::{EngineLink, Page, PatchbayApp, Shell, View};

/// The clients the views read, made before the first render.
static CLIENTS: std::sync::OnceLock<(PatchbayServiceClient, PatchbayServiceStreamClient)> =
    std::sync::OnceLock::new();

thread_local! {
    /// The view the current document opens on.
    static VIEW: std::cell::Cell<View> = const { std::cell::Cell::new(View::Route) };
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_or(47_999, |a| a.port())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .first()
        .cloned()
        .unwrap_or_else(|| "pb-shots".to_owned());
    let w: u32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(1480);
    let h: u32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(940);
    let live = args
        .iter()
        .position(|a| a == "--live")
        .and_then(|i| args.get(i + 1).cloned());
    let window = args.iter().any(|a| a == "--window");
    std::fs::create_dir_all(&dir).expect("the output folder");

    let runtime = patchbay::server::runtime().expect("the engine runtime");
    let clients = match live {
        // Read only: dial the running engine, never start one.
        Some(url) => runtime
            .block_on(patchbay::server::dial(&url))
            .unwrap_or_else(|e| panic!("no Patchbay engine at {url}: {e}")),
        None => {
            let scratch = std::env::temp_dir().join(format!("pb-shot-{}", std::process::id()));
            // SAFETY: before any engine thread exists.
            unsafe {
                std::env::set_var("PATCHBAY_CONFIG", scratch.join("patchbay.styx"));
                std::env::set_var("PATCHBAY_DEVICES", "off");
                std::env::set_var("PATCHBAY_PIPEWIRE", "off");
            }
            let mut opts = EngineOptions::new(format!("127.0.0.1:{}", free_port()));
            opts.discover_peers = false;
            let engine = Engine::start_or_attach_blocking(opts).expect("a scratch engine");
            let clients = (engine.client().clone(), engine.stream_client().clone());
            let client = clients.0.clone();
            runtime
                .block_on(client.save_mix(shot_mix()))
                .expect("the shot's mix");
            // Kept for the life of the process: it serves the views.
            std::mem::forget(engine);
            clients
        }
    };
    let _ = CLIENTS.set(clients);
    // The views run with no tokio runtime on their thread, as in
    // Architect's window: entering one here would hide a view that needs
    // it (a tokio timer did, until it panicked in the distribution).

    let wanted = std::env::var("PATCHBAY_SHOT_VIEWS").unwrap_or_default();
    if window {
        let view = View::ALL
            .into_iter()
            .find(|v| wanted.split(',').next() == Some(&v.label().to_lowercase()))
            .unwrap_or(View::Route);
        VIEW.with(|v| v.set(view));
        open_window(w, h);
        return;
    }
    for view in View::ALL {
        let name = view.label().to_lowercase();
        if !wanted.is_empty() && !wanted.split(',').any(|v| v.trim() == name) {
            continue;
        }
        VIEW.with(|v| v.set(view));
        let out = format!("{dir}/{name}.png");
        shoot(&out, w, h);
        println!("wrote {out} — {w}x{h}");
    }
}

/// A mix with one source and one output, saved disabled: the Mix page
/// shows its strips (faders, channel pickers) and nothing ever runs it.
fn shot_mix() -> patchbay::proto::MixConfig {
    use patchbay::proto::{MixConfig, MixOutputConfig, MixSourceConfig, source_kind};
    MixConfig {
        name: "Stream".to_owned(),
        channels: 2,
        sources: vec![MixSourceConfig {
            kind: source_kind::SYSTEM.to_owned(),
            target: String::new(),
            device: String::new(),
            map: "0:0,1:1".to_owned(),
            gain_db: -6.0,
            muted: false,
            exclusive: false,
        }],
        outputs: vec![MixOutputConfig::new("BuiltInSpeakerDevice")],
        enabled: Some(false),
    }
}

/// The views in a Blitz window of exactly `w`×`h`, not resizable.
fn open_window(w: u32, h: u32) {
    use dioxus_native::{Config, LogicalSize, WindowAttributes, launch_cfg};
    let window = WindowAttributes::default()
        .with_title("Patchbay (Blitz)")
        .with_surface_size(LogicalSize::new(f64::from(w), f64::from(h)))
        .with_resizable(false);
    let configs: Vec<Box<dyn std::any::Any>> =
        vec![Box::new(Config::new().with_window_attributes(window))];
    launch_cfg(Shot, vec![], configs);
}

/// Build the document, let the views fetch, paint it into `out`.
fn shoot(out: &str, w: u32, h: u32) {
    let mut document = DioxusDocument::new(
        VirtualDom::new(Shot),
        DocumentConfig {
            viewport: Some(Viewport::new(w, h, 1.0, ColorScheme::Dark)),
            ..Default::default()
        },
    );
    document.initial_build();
    // The views fetch on mount and the answers come back on the engine's
    // runtime: poll until they have landed.
    for _ in 0..60 {
        document.poll(None);
        document.inner_mut().resolve(0.0);
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let mut image = VelloImageRenderer::new(w, h);
    let mut buffer = Vec::new();
    image.render_to_vec(
        |painter| {
            let mut inner = document.inner_mut();
            blitz_paint::paint_scene(painter, &mut inner, 1.0, w, h, 0, 0);
        },
        &mut buffer,
    );
    image::RgbaImage::from_raw(w, h, buffer)
        .expect("a buffer of the right size")
        .save(out)
        .expect("write the picture");
}

#[component]
fn Shot() -> Element {
    use_hook(|| {
        if let Some((client, stream)) = CLIENTS.get() {
            provide_context(Shell {
                home: EngineLink::new(client.clone(), stream.clone()),
                dial: None,
                on_home_lost: Callback::new(|()| {}),
                // Native, as in Architect's window, and saving nothing.
                page: Page {
                    native: true,
                    ..Page::default()
                },
            });
        }
        patchbay::ui::show(VIEW.with(std::cell::Cell::get));
    });
    rsx! {
        // Real `<style>` nodes: `document::Style` routes through the
        // document's head, and a headless document has none.
        style { {"html,body{margin:0;padding:0;height:100%;overflow:hidden;}*{box-sizing:border-box;}"} }
        style { {patchbay::ui::THEME_CSS} }
        style { {patchbay::ui::THEME_NATIVE_CSS} }
        div { style: "width: 100vw; height: 100vh; position: relative;",
            PatchbayApp {}
        }
    }
}
