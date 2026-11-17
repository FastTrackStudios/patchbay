//! `patchbay-app`: Patchbay as a distribution of Architect (studio#414).
//!
//! Architect's window with the Patchbay extension linked in, under
//! Patchbay's own name, folder and bundle id ([`distribution`]). Beside
//! `fts-patchbay` (the webview app) until that one becomes the
//! distribution; both reach this machine's one engine.

mod distribution;

fn main() -> std::process::ExitCode {
    architect_desktop::run(distribution::distribution())
}
