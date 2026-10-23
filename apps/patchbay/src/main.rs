//! FTS Patchbay — the `PipeWire` studio-routing app.
//!
//! One crate, two platforms: `desktop` (the default) runs the engine
//! in-process, serves it at `ws://0.0.0.0:4046/vox` and opens the
//! window; `web` is the browser remote the engine serves to any device on
//! the LAN (`dx build --platform web` turns it on and the defaults off).

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "web")]
mod web;

#[cfg(feature = "desktop")]
fn main() -> std::process::ExitCode {
    desktop::main()
}

#[cfg(all(feature = "web", not(feature = "desktop")))]
fn main() {
    web::main();
}

#[cfg(not(any(feature = "desktop", feature = "web")))]
compile_error!("fts-patchbay needs a platform: the `desktop` feature (the default) or `web`");
