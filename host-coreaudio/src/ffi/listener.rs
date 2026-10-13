//! RAII property listeners (`AudioObjectAddPropertyListener`).

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectPropertyAddress, AudioObjectRemovePropertyListener,
};
use patchbay_host::HostError;

use super::ObjectId;
use super::property::{Scope, address};

/// Called on a HAL notification thread with `(object, selector)`. Keep it
/// short and non-blocking (send to a channel).
pub(crate) type Callback = Box<dyn Fn(ObjectId, u32) + Send + Sync>;

/// A registered listener; removed on drop.
pub(crate) struct PropertyListener {
    object: ObjectId,
    addr: AudioObjectPropertyAddress,
    /// Owned `Box<Callback>` handed to the HAL as client data.
    ctx: NonNull<Callback>,
}

// SAFETY: `ctx` points to a `Callback`, which is `Send + Sync`; the other
// fields are plain integers. The HAL may invoke the callback from any
// thread, which the `Send + Sync` bound already allows.
unsafe impl Send for PropertyListener {}
// SAFETY: `&PropertyListener` exposes nothing; see `Send`.
unsafe impl Sync for PropertyListener {}

impl PropertyListener {
    /// Register `callback` for `selector` on `object`.
    pub(crate) fn add(
        object: ObjectId,
        selector: u32,
        scope: Scope,
        callback: Callback,
    ) -> Result<Self, HostError> {
        let addr = address(selector, scope);
        let ctx = NonNull::from(Box::leak(Box::new(callback)));
        // SAFETY: `trampoline` matches `AudioObjectPropertyListenerProc`;
        // `ctx` stays valid until `Drop` has removed the listener.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&addr),
                Some(trampoline),
                ctx.as_ptr().cast(),
            )
        };
        if let Err(e) = super::check(
            &format!("add listener '{}' on {object}", super::fourcc(selector)),
            status,
        ) {
            // SAFETY: registration failed, so the HAL holds no copy of `ctx`;
            // reclaim the box we leaked above.
            drop(unsafe { Box::from_raw(ctx.as_ptr()) });
            return Err(e);
        }
        Ok(Self { object, addr, ctx })
    }
}

impl Drop for PropertyListener {
    fn drop(&mut self) {
        // SAFETY: same (object, address, proc, client data) tuple that was
        // registered. After removal returns, the HAL no longer calls the
        // trampoline with `ctx` (removal synchronises with in-flight
        // notifications), so freeing the box is sound.
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                self.object,
                NonNull::from(&self.addr),
                Some(trampoline),
                self.ctx.as_ptr().cast(),
            )
        };
        if status == 0 {
            // SAFETY: `ctx` came from `Box::leak` in `add` and the HAL has
            // dropped its reference.
            drop(unsafe { Box::from_raw(self.ctx.as_ptr()) });
        } else {
            // The object is already gone (device unplugged): the HAL has
            // forgotten the listener with it, but we cannot prove no call is
            // in flight, so leak the (small) box instead of risking a UAF.
            tracing::debug!(
                object = self.object,
                status,
                "listener removal failed; leaking callback box"
            );
        }
    }
}

/// C trampoline: fans the notified addresses out to the Rust callback.
unsafe extern "C-unwind" fn trampoline(
    object: ObjectId,
    count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    ctx: *mut c_void,
) -> i32 {
    let Some(ctx) = NonNull::new(ctx.cast::<Callback>()) else {
        return 0;
    };
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: the HAL passes `count` valid addresses.
    let addrs = unsafe { std::slice::from_raw_parts(addresses.as_ptr(), count) };
    // SAFETY: `ctx` is the live `Box<Callback>` registered in `add`
    // (removal in `Drop` happens before it is freed).
    let callback = unsafe { ctx.as_ref() };
    // Never unwind into the HAL.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        for addr in addrs {
            callback(object, addr.mSelector);
        }
    }));
    0
}
