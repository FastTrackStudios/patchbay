//! RAII `IOProc` registration with a safe buffer view.
//!
//! The render callback runs on the HAL's real-time IO thread: the
//! [`Render`] implementation must not allocate, lock or block.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use patchbay_host::HostError;

use super::{ObjectId, check};

/// Real-time render callback. Called once per HAL cycle with the device's
/// input buffers (read-only) and output buffers (to fill).
pub(crate) trait Render: Send + Sync + 'static {
    /// Must be wait-free: no allocation, locks or syscalls.
    fn render(&self, input: &Buffers<'_>, output: &mut BuffersMut<'_>);
}

/// One `f32` buffer: `channels` interleaved channels.
pub(crate) struct Buffer<'a> {
    pub channels: usize,
    pub samples: &'a [f32],
}

/// One writable `f32` buffer.
pub(crate) struct BufferMut<'a> {
    pub channels: usize,
    pub samples: &'a mut [f32],
}

/// Read-only view of an `AudioBufferList`.
pub(crate) struct Buffers<'a> {
    list: *const AudioBufferList,
    _life: PhantomData<&'a AudioBufferList>,
}

/// Writable view of an `AudioBufferList`.
pub(crate) struct BuffersMut<'a> {
    list: *mut AudioBufferList,
    _life: PhantomData<&'a mut AudioBufferList>,
}

/// `(count, pointer to buffer i)` helpers shared by both views.
///
/// # Safety
/// `list` must be null or point to a valid `AudioBufferList` whose
/// `mBuffers` flexible array really holds `mNumberBuffers` entries.
unsafe fn buffer_count(list: *const AudioBufferList) -> usize {
    if list.is_null() {
        return 0;
    }
    // SAFETY: per the function contract.
    usize::try_from(unsafe { (*list).mNumberBuffers }).unwrap_or(0)
}

/// # Safety
/// As [`buffer_count`], and `index < buffer_count(list)`.
const unsafe fn buffer_at(list: *const AudioBufferList, index: usize) -> *const AudioBuffer {
    // SAFETY: `index` is within the flexible array (contract).
    unsafe {
        std::ptr::addr_of!((*list).mBuffers)
            .cast::<AudioBuffer>()
            .add(index)
    }
}

/// Float slice parameters of one buffer, if it is usable.
fn float_view(buffer: &AudioBuffer) -> Option<(NonNull<f32>, usize, usize)> {
    let data = NonNull::new(buffer.mData.cast::<f32>())?;
    if !data.as_ptr().is_aligned() {
        return None;
    }
    let len = usize::try_from(buffer.mDataByteSize)
        .ok()?
        .checked_div(size_of::<f32>())?;
    let channels = usize::try_from(buffer.mNumberChannels).ok()?;
    Some((data, len, channels))
}

impl<'a> Buffers<'a> {
    /// Number of buffers.
    pub(crate) fn len(&self) -> usize {
        // SAFETY: `list` came from the HAL for this callback (see
        // `io_trampoline`).
        unsafe { buffer_count(self.list) }
    }

    /// Buffer `index`, if it exists and holds aligned `f32` data.
    pub(crate) fn get(&self, index: usize) -> Option<Buffer<'a>> {
        if index >= self.len() {
            return None;
        }
        // SAFETY: `index < len` and `list` is valid for this callback.
        let buffer = unsafe { &*buffer_at(self.list, index) };
        let (data, len, channels) = float_view(buffer)?;
        // SAFETY: the HAL guarantees `mData` holds `mDataByteSize` bytes of
        // the stream's (Float32, checked by the caller's format) samples,
        // valid and unaliased-for-write for the duration of the callback.
        let samples = unsafe { std::slice::from_raw_parts(data.as_ptr(), len) };
        Some(Buffer { channels, samples })
    }
}

impl BuffersMut<'_> {
    /// Number of buffers.
    pub(crate) fn len(&self) -> usize {
        // SAFETY: as `Buffers::len`.
        unsafe { buffer_count(self.list) }
    }

    /// Buffer `index` for writing (borrowing `self` mutably, so at most one
    /// buffer is writable at a time).
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<BufferMut<'_>> {
        if index >= self.len() {
            return None;
        }
        // SAFETY: `index < len` and `list` is valid for this callback.
        let buffer = unsafe { &*buffer_at(self.list, index) };
        let (data, len, channels) = float_view(buffer)?;
        // SAFETY: output `mData` is `mDataByteSize` writable bytes owned by
        // this callback; the `&mut self` borrow keeps the slice unique.
        let samples = unsafe { std::slice::from_raw_parts_mut(data.as_ptr(), len) };
        Some(BufferMut { channels, samples })
    }
}

/// An `IOProc` registered on `device`; stopped and destroyed on drop.
pub(crate) struct IoProc<R: Render> {
    device: ObjectId,
    proc_id: AudioDeviceIOProcID,
    /// `Arc::into_raw` of the render state; reclaimed in `Drop`.
    state: NonNull<R>,
    running: bool,
}

// SAFETY: `R: Send + Sync` and the other fields are plain ids / a function
// pointer; the HAL may run the proc on its own thread regardless.
unsafe impl<R: Render> Send for IoProc<R> {}

impl<R: Render> IoProc<R> {
    /// Register `state` as the render callback of `device` (not started).
    pub(crate) fn create(device: ObjectId, state: Arc<R>) -> Result<Self, HostError> {
        let state = NonNull::new(Arc::into_raw(state).cast_mut()).ok_or(HostError::Closed)?;
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `io_trampoline::<R>` matches `AudioDeviceIOProc`; the
        // client data is the `Arc<R>` pointer, alive until `Drop` has
        // destroyed the proc.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                Some(io_trampoline::<R>),
                state.as_ptr().cast(),
                NonNull::from(&mut proc_id),
            )
        };
        if let Err(e) = check("AudioDeviceCreateIOProcID", status) {
            // SAFETY: registration failed; reclaim the Arc we leaked.
            drop(unsafe { Arc::from_raw(state.as_ptr()) });
            return Err(e);
        }
        Ok(Self {
            device,
            proc_id,
            state,
            running: false,
        })
    }

    /// Start IO.
    pub(crate) fn start(&mut self) -> Result<(), HostError> {
        // SAFETY: `proc_id` is registered on `device`.
        check("AudioDeviceStart", unsafe {
            AudioDeviceStart(self.device, self.proc_id)
        })?;
        self.running = true;
        Ok(())
    }
}

impl<R: Render> Drop for IoProc<R> {
    fn drop(&mut self) {
        if self.running {
            // SAFETY: `proc_id` is registered and started on `device`.
            let status = unsafe { AudioDeviceStop(self.device, self.proc_id) };
            if status != 0 {
                tracing::warn!(
                    status = super::status_string(status),
                    "AudioDeviceStop failed"
                );
            }
        }
        // SAFETY: `proc_id` is registered on `device`. Destroying it
        // synchronises with the IO thread: once it returns the trampoline
        // is not running and will not run again.
        let status = unsafe { AudioDeviceDestroyIOProcID(self.device, self.proc_id) };
        if status == 0 {
            // SAFETY: the pointer came from `Arc::into_raw` in `create` and
            // the HAL no longer uses it.
            drop(unsafe { Arc::from_raw(self.state.as_ptr()) });
        } else {
            // Device already gone; leak the state rather than risk a UAF.
            tracing::warn!(
                status = super::status_string(status),
                "AudioDeviceDestroyIOProcID failed; leaking state"
            );
        }
    }
}

/// C trampoline for the HAL IO thread.
unsafe extern "C-unwind" fn io_trampoline<R: Render>(
    _device: ObjectId,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    ctx: *mut c_void,
) -> i32 {
    let Some(state) = NonNull::new(ctx.cast::<R>()) else {
        return 0;
    };
    // SAFETY: `ctx` is the live `Arc<R>` pointer registered in `create`.
    let state = unsafe { state.as_ref() };
    let input = Buffers {
        list: input.as_ptr(),
        _life: PhantomData,
    };
    let mut output = BuffersMut {
        list: output.as_ptr(),
        _life: PhantomData,
    };
    // Never unwind into the HAL. (`catch_unwind` itself doesn't allocate.)
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.render(&input, &mut output);
    }));
    0
}
