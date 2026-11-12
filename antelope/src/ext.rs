//! The Antelope family as an Architect extension (studio#418).
//!
//! Linked into a distribution beside the Patchbay core, its `build` adds
//! the family and its console view to this process ([`crate::register`]).
//! It declares no actions or views of its own: the core's
//! `PATCHBAY_DEVICE_*` and Route/Mix pages reach a Galaxy32 like any
//! device.

use architect_extensions::linked::{Builder, Extension};

/// The extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Antelope;

impl Extension for Antelope {
    fn manifest(&self) -> &'static str {
        include_str!("../extension.toml")
    }

    fn build(&self, _ext: &mut Builder) {
        crate::register();
    }
}
