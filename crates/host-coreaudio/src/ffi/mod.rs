//! The only module allowed to use `unsafe`: thin, safe wrappers over the
//! Core Audio HAL (`objc2-core-audio`), Core Foundation and libc.
//!
//! Every `unsafe` block carries a `SAFETY:` comment. Everything outside
//! `ffi` is `#![deny(unsafe_code)]` (see `lib.rs`).
//!
//! Submodules:
//! - [`hal`]      — HAL facts (devices, processes) as plain data.
//! - [`property`] — typed `AudioObjectGetPropertyData` reads.
//! - [`listener`] — RAII property listeners.
//! - [`tap`]      — process taps (`CATapDescription`) + private aggregates.
//! - [`ioproc`]   — RAII `IOProc` with a safe buffer view for the render
//!   callback.
//! - [`permission`] — microphone authorization, System Settings, `NSAlert`.
//! - [`sys`]      — libc / TCC odds and ends.
#![allow(unsafe_code)]

pub(crate) mod hal;
pub(crate) mod ioproc;
pub(crate) mod listener;
pub(crate) mod permission;
pub(crate) mod property;
pub(crate) mod sys;
pub(crate) mod tap;

use patchbay_host::HostError;

/// A Core Audio object id (`AudioObjectID`).
pub(crate) type ObjectId = u32;

/// `kAudioObjectSystemObject` (the crate types it as `c_int`).
pub(crate) const SYSTEM_OBJECT: ObjectId = 1;

/// Render an `OSStatus` the way Apple documents them: a four-char code
/// when all four bytes are printable, else the decimal value.
pub(crate) fn status_string(status: i32) -> String {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("'{}' ({status})", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

/// `Ok(())` for `noErr`, else [`HostError::Os`] naming `op`.
pub(crate) fn check(op: &str, status: i32) -> Result<(), HostError> {
    if status == 0 {
        Ok(())
    } else {
        Err(HostError::Os {
            op: op.to_owned(),
            status: status_string(status),
        })
    }
}

/// A four-char code as text (`'grup'`), for selectors / transport types.
pub(crate) fn fourcc(code: u32) -> String {
    let bytes = code.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("0x{code:08x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_rendering() {
        assert_eq!(status_string(0x216f_626a), "'!obj' (560947818)");
        assert_eq!(status_string(-50), "-50");
        assert_eq!(fourcc(0x6772_7570), "grup");
        assert!(check("x", 0).is_ok());
        assert!(check("x", -50).is_err());
    }
}
