//! The Yamaha TF family as an Architect extension (studio#417).
//!
//! Linked into a distribution beside the Patchbay core, its `build` adds
//! the family and its console view to this process ([`crate::register`]).
//! It declares no actions or views of its own: the core's
//! `PATCHBAY_DEVICE_*` and Route/Mix pages reach a console like any
//! device.

use architect_extensions::linked::{Builder, Extension};

/// The extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct YamahaTf;

impl Extension for YamahaTf {
    fn manifest(&self) -> &'static str {
        include_str!("../extension.toml")
    }

    fn build(&self, _ext: &mut Builder) {
        crate::register();
    }
}
