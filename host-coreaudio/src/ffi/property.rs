//! Typed reads of HAL object properties.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty,
    AudioObjectPropertyAddress, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput,
    kAudioObjectPropertyScopeWildcard,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamBasicDescription};
use objc2_core_foundation::{CFRetained, CFString};
use patchbay_host::HostError;

use super::{ObjectId, check};

/// Property scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Global,
    /// Device input (capture) side.
    Input,
    /// Device output (playback) side.
    Output,
    /// Any scope (listener registration only).
    Wildcard,
}

impl Scope {
    const fn raw(self) -> u32 {
        match self {
            Self::Global => kAudioObjectPropertyScopeGlobal,
            Self::Input => kAudioObjectPropertyScopeInput,
            Self::Output => kAudioObjectPropertyScopeOutput,
            Self::Wildcard => kAudioObjectPropertyScopeWildcard,
        }
    }
}

/// A property address on the main element.
pub(crate) const fn address(selector: u32, scope: Scope) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope.raw(),
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Plain-old-data property values: any bit pattern is valid and the type
/// has no drop glue, so the HAL may write straight into one.
///
/// # Safety
/// Implement only for `Copy` C-layout types valid for every bit pattern.
pub(crate) unsafe trait Pod: Copy + Default {}
// SAFETY: integers and floats are valid for every bit pattern.
unsafe impl Pod for u32 {}
// SAFETY: as above.
unsafe impl Pod for i32 {}
// SAFETY: as above.
unsafe impl Pod for f64 {}

/// Whether `object` has the property.
pub(crate) fn has(object: ObjectId, selector: u32, scope: Scope) -> bool {
    let addr = address(selector, scope);
    // SAFETY: `addr` is a valid, live property address for the call.
    unsafe { AudioObjectHasProperty(object, NonNull::from(&addr)) }
}

/// Byte size of a property value, with an optional qualifier.
fn data_size(object: ObjectId, selector: u32, scope: Scope, what: &str) -> Result<u32, HostError> {
    let addr = address(selector, scope);
    let mut size = 0_u32;
    // SAFETY: `addr` and `size` outlive the call; no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    check(what, status)?;
    Ok(size)
}

/// Read a fixed-size [`Pod`] property, optionally qualified by another
/// [`Pod`] (e.g. a pid for `TranslatePIDToProcessObject`).
pub(crate) fn get<T: Pod, Q: Pod>(
    object: ObjectId,
    selector: u32,
    scope: Scope,
    qualifier: Option<&Q>,
) -> Result<T, HostError> {
    let what = format!(
        "get property '{}' on object {object}",
        super::fourcc(selector)
    );
    let addr = address(selector, scope);
    let mut value = T::default();
    let mut size =
        u32::try_from(size_of::<T>()).map_err(|_| HostError::InvalidSpec(what.clone()))?;
    let (q_size, q_ptr) = match qualifier {
        Some(q) => (
            u32::try_from(size_of::<Q>()).map_err(|_| HostError::InvalidSpec(what.clone()))?,
            std::ptr::from_ref(q).cast::<c_void>(),
        ),
        None => (0, std::ptr::null()),
    };
    // SAFETY: `value` is a `Pod` (valid for any bytes the HAL writes) of
    // exactly `size` bytes; `addr`, `size` and the qualifier outlive the
    // call, and the qualifier is `q_size` bytes of plain data.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            q_size,
            q_ptr,
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    check(&what, status)?;
    Ok(value)
}

/// Read a fixed-size property with no qualifier.
pub(crate) fn get_plain<T: Pod>(
    object: ObjectId,
    selector: u32,
    scope: Scope,
) -> Result<T, HostError> {
    get::<T, u32>(object, selector, scope, None)
}

/// Read an array-of-`AudioObjectID` property (device list, process list).
pub(crate) fn get_object_list(
    object: ObjectId,
    selector: u32,
    scope: Scope,
) -> Result<Vec<ObjectId>, HostError> {
    let what = format!("get list '{}' on object {object}", super::fourcc(selector));
    let size = data_size(object, selector, scope, &what)?;
    let count = usize::try_from(size)
        .map_err(|_| HostError::InvalidSpec(what.clone()))?
        .checked_div(size_of::<ObjectId>())
        .unwrap_or(0);
    let mut ids: Vec<ObjectId> = vec![0; count];
    let mut size = u32::try_from(ids.len().saturating_mul(size_of::<ObjectId>()))
        .map_err(|_| HostError::InvalidSpec(what.clone()))?;
    if ids.is_empty() {
        return Ok(ids);
    }
    let addr = address(selector, scope);
    // SAFETY: `ids` owns `size` writable bytes of `u32`s (valid for any
    // bit pattern); `addr` and `size` outlive the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr())
                .ok_or_else(|| HostError::InvalidSpec(what.clone()))?
                .cast(),
        )
    };
    check(&what, status)?;
    // The list may have shrunk between the two calls.
    ids.truncate(
        usize::try_from(size)
            .unwrap_or(0)
            .checked_div(size_of::<ObjectId>())
            .unwrap_or(0),
    );
    Ok(ids)
}

/// Read a `CFStringRef` property (names, UIDs, bundle ids). The HAL
/// returns the string retained (+1); we take ownership.
pub(crate) fn get_string(
    object: ObjectId,
    selector: u32,
    scope: Scope,
) -> Result<String, HostError> {
    let what = format!(
        "get string '{}' on object {object}",
        super::fourcc(selector)
    );
    let addr = address(selector, scope);
    let mut raw: *const CFString = std::ptr::null();
    let mut size = u32::try_from(size_of::<*const CFString>())
        .map_err(|_| HostError::InvalidSpec(what.clone()))?;
    // SAFETY: `raw` is a pointer-sized out slot for the `CFStringRef`;
    // `addr` and `size` outlive the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut raw).cast(),
        )
    };
    check(&what, status)?;
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Ok(String::new());
    };
    // SAFETY: CFString-typed HAL properties follow the Create rule: the
    // caller owns one retain, which `CFRetained` releases on drop.
    let string = unsafe { CFRetained::from_raw(ptr) };
    Ok(string.to_string())
}

/// Per-buffer channel counts of a device's stream configuration on one
/// side (`kAudioDevicePropertyStreamConfiguration`, an
/// `AudioBufferList`).
pub(crate) fn stream_channels(
    object: ObjectId,
    selector: u32,
    scope: Scope,
) -> Result<Vec<u32>, HostError> {
    let what = format!("get stream config on object {object}");
    let size = data_size(object, selector, scope, &what)?;
    let bytes = usize::try_from(size).map_err(|_| HostError::InvalidSpec(what.clone()))?;
    if bytes < size_of::<u32>() {
        return Ok(Vec::new());
    }
    // `u64` backing store: `AudioBufferList` holds pointers, so it needs
    // 8-byte alignment that a `Vec<u8>` would not guarantee.
    let words = bytes.div_ceil(size_of::<u64>());
    let mut storage: Vec<u64> = vec![0; words];
    let mut size = size;
    let addr = address(selector, scope);
    // SAFETY: `storage` provides at least `size` writable, 8-aligned bytes;
    // `addr` and `size` outlive the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(storage.as_mut_ptr())
                .ok_or_else(|| HostError::InvalidSpec(what.clone()))?
                .cast(),
        )
    };
    check(&what, status)?;
    let list = storage.as_ptr().cast::<AudioBufferList>();
    let written = usize::try_from(size).unwrap_or(0);
    // SAFETY: the HAL wrote an `AudioBufferList` at the start of the
    // 8-aligned storage (at least 4 bytes, checked above).
    let count = unsafe { (*list).mNumberBuffers };
    let count = usize::try_from(count).unwrap_or(0);
    let header = std::mem::offset_of!(AudioBufferList, mBuffers);
    let fits = count
        .checked_mul(size_of::<AudioBuffer>())
        .and_then(|b| b.checked_add(header))
        .is_some_and(|b| b <= written);
    if !fits {
        return Err(HostError::Os {
            op: what,
            status: "short AudioBufferList".to_owned(),
        });
    }
    // SAFETY: `mBuffers` is a flexible array of `count` `AudioBuffer`s,
    // all within the `written` bytes (checked above), properly aligned.
    let buffers = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(),
            count,
        )
    };
    Ok(buffers.iter().map(|b| b.mNumberChannels).collect())
}

/// Stream format property (`AudioStreamBasicDescription`), e.g.
/// `kAudioTapPropertyFormat`.
pub(crate) fn get_format(
    object: ObjectId,
    selector: u32,
) -> Result<AudioStreamBasicDescription, HostError> {
    let what = format!(
        "get format '{}' on object {object}",
        super::fourcc(selector)
    );
    let addr = address(selector, Scope::Global);
    let mut asbd = AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: 0,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 0,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 0,
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut size = u32::try_from(size_of::<AudioStreamBasicDescription>())
        .map_err(|_| HostError::InvalidSpec(what.clone()))?;
    // SAFETY: `asbd` is a C-layout struct of plain integers/floats of
    // exactly `size` bytes; `addr` and `size` outlive the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut asbd).cast(),
        )
    };
    check(&what, status)?;
    Ok(asbd)
}
