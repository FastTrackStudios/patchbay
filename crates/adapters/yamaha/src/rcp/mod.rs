//! The RCP wire format: pure, synchronous, unit-tested.
//!
//! - [`codec`]: LF line framing across TCP reads.
//! - [`token`]: tokenizer (quoted strings, backslash escapes) and quoting.
//! - [`command`]: command encoding.
//! - [`reply`]: `OK` / `OKm` / `NOTIFY` / `ERROR` and `prminfo` parsing.
//!
//! Grammar per `docs/yamaha-tf-rcp.md` §1 (verified on a TF1 V4.55).

pub mod codec;
pub mod command;
pub mod reply;
pub mod token;
