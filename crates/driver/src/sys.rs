//! The OS primitives the plug-in needs: `CoreFoundation` objects for the
//! host's property and storage calls, and the host clock.
//!
//! The plug-in only ever runs inside macOS's `coreaudiod`. Off macOS the
//! crate still builds so its logic (state machine, loopback ring, clock
//! arming, the IO entry points) is tested on every platform; there
//! [`host_ticks`] is a monotonic nanosecond clock and the `CoreFoundation`
//! calls are inert. They are only reached through an
//! `AudioServerPlugInHostRef`, and no host exists off macOS.

/// Converts nanoseconds to host ticks: `ticks = ns * denom / numer`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Timebase {
    pub numer: u32,
    pub denom: u32,
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    use super::Timebase;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external_representation: u8,
        ) -> *const c_void;

        fn CFDataCreate(alloc: *const c_void, bytes: *const u8, length: isize) -> *const c_void;

        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;

        fn CFDataGetLength(data: *const c_void) -> isize;

        fn CFRelease(cf: *const c_void);

        fn mach_absolute_time() -> u64;

        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    pub(crate) fn host_ticks() -> u64 {
        // SAFETY: mach_absolute_time is always safe to call on macOS.
        unsafe { mach_absolute_time() }
    }

    pub(crate) fn timebase() -> Timebase {
        let mut tb = Timebase::default();
        // SAFETY: mach_timebase_info with a valid out pointer is always safe.
        let _ = unsafe { mach_timebase_info(&mut tb) };
        tb
    }

    pub(crate) fn cfstring_create(s: &str) -> *const c_void {
        // SAFETY: FFI call to CoreFoundation with valid UTF-8 bytes.
        unsafe {
            CFStringCreateWithBytes(
                core::ptr::null(),
                s.as_ptr(),
                s.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
                0,
            )
        }
    }

    pub(crate) fn cfdata_create(bytes: &[u8]) -> *const c_void {
        // SAFETY: FFI call to CoreFoundation with valid byte slice.
        unsafe { CFDataCreate(core::ptr::null(), bytes.as_ptr(), bytes.len() as isize) }
    }

    /// The bytes of a `CFData`, copied out.
    ///
    /// # Safety
    /// `data` must be a valid `CFDataRef`.
    pub(crate) unsafe fn cfdata_bytes(data: *const c_void) -> Option<Vec<u8>> {
        // SAFETY: the caller guarantees `data` is a valid CFDataRef;
        // CFDataGetBytePtr returns `len` contiguous bytes.
        unsafe {
            let ptr = CFDataGetBytePtr(data);
            let len = CFDataGetLength(data);
            if ptr.is_null() || len <= 0 {
                return None;
            }
            Some(core::slice::from_raw_parts(ptr, len as usize).to_vec())
        }
    }

    /// # Safety
    /// `cf` must be a `CoreFoundation` object the caller owns (+1).
    pub(crate) unsafe fn cf_release(cf: *const c_void) {
        // SAFETY: per the caller's contract.
        unsafe { CFRelease(cf) }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::ffi::c_void;
    use std::sync::LazyLock;
    use std::time::Instant;

    use super::Timebase;

    static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

    /// Nanoseconds on a monotonic clock (the timebase is 1/1). Starts at 1
    /// so it never reads as the "not armed" anchor 0.
    pub(crate) fn host_ticks() -> u64 {
        u64::try_from(EPOCH.elapsed().as_nanos())
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }

    pub(crate) const fn timebase() -> Timebase {
        Timebase { numer: 1, denom: 1 }
    }

    pub(crate) const fn cfstring_create(_: &str) -> *const c_void {
        core::ptr::null()
    }

    pub(crate) const fn cfdata_create(_: &[u8]) -> *const c_void {
        core::ptr::null()
    }

    /// # Safety
    /// None needed; kept `unsafe` to match the macOS signature.
    pub(crate) const unsafe fn cfdata_bytes(_: *const c_void) -> Option<Vec<u8>> {
        None
    }

    /// # Safety
    /// None needed; kept `unsafe` to match the macOS signature.
    pub(crate) const unsafe fn cf_release(_: *const c_void) {}
}

pub(crate) use imp::*;
