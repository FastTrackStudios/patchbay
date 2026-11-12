//! The Patchbay distribution of Architect (`docs/architect-distribution.md`
//! is the Studio Architect one; this follows it).
//!
//! | | Patchbay | the Architect app |
//! | --- | --- | --- |
//! | name | `Patchbay` | `Architect` |
//! | app (control socket, `actions`) | `patchbay` | `architect-desktop` |
//! | config folder | `patchbay` | `architect` |
//! | bundle id | `app.fasttrackstudio.patchbay` | `app.fasttrackstudio.architect` |
//!
//! Not the Architect app renamed: `Distribution::new` has none of its
//! first-party extensions (Projects, Tasks, Calendar…), only the
//! `patchbay` bundle, with the store kept as a registry so the device
//! families can be installed at run time. The engine's own config stays
//! at `<config>/fts/patchbay/patchbay.styx`; this folder is Architect's.

use architect_desktop::{Bundle, Distribution, Icon};

/// The distribution's name.
pub const NAME: &str = "Patchbay";

/// Its app name (the control socket's: `patchbay-app actions …` reaches
/// app `patchbay`) and its folder under the platform's config dir.
pub const FOLDER: &str = "patchbay";

/// Its bundle id: the one Patchbay.app has always had, so macOS keeps the
/// grants (System Audio Recording, Microphone) it gave Patchbay.
pub const BUNDLE_ID: &str = "app.fasttrackstudio.patchbay";

/// Its icon.
pub const ICON: &str = include_str!("../assets/icon.svg");

/// Patchbay's extensions, compiled in together: the core
/// (`patchbay::ext::Patchbay`) and the device families that are
/// extensions of their own (the Yamaha TF); the rest join as they become
/// one (studio#410).
#[must_use]
pub fn bundle() -> Bundle {
    Bundle::new("patchbay", "Patchbay")
        .member(&patchbay::ext::Patchbay)
        .member(&patchbay_yamaha_tf::ext::YamahaTf)
}

/// Patchbay: Architect's window with [`bundle`] compiled in.
#[must_use]
pub fn distribution() -> Distribution {
    Distribution::new(NAME)
        .app(FOLDER)
        .folder(FOLDER)
        .bundle_id(BUNDLE_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .icon(Icon::svg(ICON).tray("cable.connector", "audio-card"))
        .bundle(bundle())
}
