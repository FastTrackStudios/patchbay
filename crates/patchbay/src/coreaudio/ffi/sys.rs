//! libc odds and ends: process names, and the TCC preflight.

use std::ffi::{CStr, c_char, c_int, c_void};

use objc2_core_foundation::CFString;

/// `proc_name(pid)` — the short process name, if the pid is visible.
pub(crate) fn process_name(pid: i32) -> Option<String> {
    let mut buf = [0_u8; 256];
    let len = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is `len` writable bytes; `proc_name` NUL-terminates
    // within the buffer and returns the length written (0 on failure).
    let written = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), len) };
    if written <= 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buf)
        .ok()
        .map(|s| s.to_string_lossy().into_owned())
}

/// Result of the TCC preflight for "System Audio Recording"
/// (`kTCCServiceAudioCapture`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preflight {
    Granted,
    Denied,
    /// Not decided yet: the first tap (or [`audio_capture_request`]) prompts.
    NotDetermined,
    /// The SPI is missing or answered something unexpected.
    Unknown,
}

const TCC_FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC";
const AUDIO_CAPTURE_SERVICE: &str = "kTCCServiceAudioCapture";

/// Resolve `symbol` from the private TCC framework (`None` if missing).
fn tcc_symbol(symbol: &CStr) -> Option<std::ptr::NonNull<c_void>> {
    // SAFETY: plain `dlopen` of a system framework path; RTLD_LAZY.
    let handle = unsafe { libc::dlopen(TCC_FRAMEWORK.as_ptr(), libc::RTLD_LAZY) };
    if handle.is_null() {
        return None;
    }
    // Deliberately never `dlclose`d: TCC is a system framework already
    // mapped into every process, and unloading it is never useful.
    // SAFETY: `handle` is a live dlopen handle; `symbol` is NUL-terminated.
    std::ptr::NonNull::new(unsafe { libc::dlsym(handle, symbol.as_ptr().cast::<c_char>()) })
}

type PreflightFn = unsafe extern "C" fn(*const CFString, *const c_void) -> c_int;

/// Ask TCC whether this process (its *responsible* app — `Patchbay.app`
/// when bundled, the terminal for a `cargo run`) may capture audio,
/// without prompting.
///
/// Uses the **private** `TCCAccessPreflight` SPI via `dlopen`, as
/// insidegui/AudioCap does; read-only and best-effort — any failure is
/// [`Preflight::Unknown`].
pub(crate) fn audio_capture_preflight() -> Preflight {
    let Some(sym) = tcc_symbol(c"TCCAccessPreflight") else {
        return Preflight::Unknown;
    };
    // SAFETY: `TCCAccessPreflight(CFStringRef service, CFDictionaryRef
    // options) -> int` — the signature AudioCap and others call; `sym` is
    // non-null and resolved from TCC.
    let preflight: PreflightFn =
        unsafe { std::mem::transmute::<*mut c_void, PreflightFn>(sym.as_ptr()) };
    let service = CFString::from_static_str(AUDIO_CAPTURE_SERVICE);
    // SAFETY: `service` is a live CFString; NULL options are allowed.
    match unsafe { preflight(std::ptr::from_ref(&*service), std::ptr::null()) } {
        0 => Preflight::Granted,
        1 => Preflight::Denied,
        2 => Preflight::NotDetermined,
        _ => Preflight::Unknown,
    }
}

#[cfg(feature = "tcc-spi")]
type RequestFn =
    unsafe extern "C" fn(*const CFString, *const c_void, *const block2::DynBlock<dyn Fn(u8)>);

/// Show the "System Audio Recording" prompt through the **private**
/// `TCCAccessRequest` SPI (as `AudioCap` does). `done(granted)` runs on a
/// TCC queue once the user answers. Only a fallback for when the
/// supported path (starting a tap) did not prompt. Returns `false` if the
/// SPI is unavailable (then `done` is never called).
#[cfg(feature = "tcc-spi")]
pub(crate) fn audio_capture_request(done: impl Fn(bool) + Send + Sync + 'static) -> bool {
    let Some(sym) = tcc_symbol(c"TCCAccessRequest") else {
        return false;
    };
    // SAFETY: `TCCAccessRequest(CFStringRef service, CFDictionaryRef
    // options, void (^)(Boolean granted))` — the signature AudioCap uses;
    // `sym` is non-null and resolved from TCC.
    let request: RequestFn = unsafe { std::mem::transmute::<*mut c_void, RequestFn>(sym.as_ptr()) };
    let service = CFString::from_static_str(AUDIO_CAPTURE_SERVICE);
    let block = block2::RcBlock::new(move |granted: u8| done(granted != 0));
    // SAFETY: `service` is a live CFString, NULL options are allowed, and
    // TCC copies (retains) the block before returning.
    unsafe {
        request(
            std::ptr::from_ref(&*service),
            std::ptr::null(),
            &raw const *block,
        );
    }
    true
}
