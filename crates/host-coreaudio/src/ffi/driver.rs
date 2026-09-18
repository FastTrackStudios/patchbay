//! Talking to `Patchbay.driver` (crates/driver) through its custom
//! plug-in properties: `'pbds'` (desired state, write) and `'pbas'`
//! (applied state, read), both JSON in a `CFData`.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    kAudioHardwarePropertyTranslateBundleIDToPlugIn, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use objc2_core_foundation::{CFData, CFRetained, CFString};
use patchbay_host::HostError;

use super::{ObjectId, check};

/// The driver's bundle id.
pub(crate) const DRIVER_BUNDLE_ID: &str = "app.fasttrackstudio.patchbay.driver";

const fn fourcc(code: [u8; 4]) -> u32 {
    u32::from_be_bytes(code)
}

/// Desired state (JSON), settable.
const PROPERTY_DESIRED_STATE: u32 = fourcc(*b"pbds");
/// Applied state (JSON), read-only.
const PROPERTY_APPLIED_STATE: u32 = fourcc(*b"pbas");
/// Runtime counters (JSON), read-only.
const PROPERTY_RUNTIME_STATS: u32 = fourcc(*b"pbrs");

const fn global(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// The driver's plug-in object, `None` when it isn't installed/loaded.
pub(crate) fn plugin_object() -> Result<Option<ObjectId>, HostError> {
    let addr = global(kAudioHardwarePropertyTranslateBundleIDToPlugIn);
    let bundle = CFString::from_str(DRIVER_BUNDLE_ID);
    let bundle_ref: *const CFString = &raw const *bundle;
    let mut object: ObjectId = 0;
    let mut size = u32::try_from(size_of::<ObjectId>()).unwrap_or(4);
    let q_size = u32::try_from(size_of::<*const CFString>()).unwrap_or(8);
    // SAFETY: the qualifier is a `CFStringRef` (pointer-sized, live for the
    // call) as `TranslateBundleIDToPlugIn` documents; `object` is a
    // `size`-byte out slot; `addr` outlives the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            ObjectId::try_from(kAudioObjectSystemObject).unwrap_or(1),
            NonNull::from(&addr),
            q_size,
            (&raw const bundle_ref).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut object).cast(),
        )
    };
    check("translate driver bundle id", status)?;
    Ok((object != 0).then_some(object))
}

/// Write the desired-state JSON; the driver applies it asynchronously.
pub(crate) fn set_desired_state(plugin: ObjectId, json: &str) -> Result<(), HostError> {
    let addr = global(PROPERTY_DESIRED_STATE);
    let data = CFData::from_bytes(json.as_bytes());
    let data_ref: *const CFData = &raw const *data;
    let size = u32::try_from(size_of::<*const CFData>()).unwrap_or(8);
    // SAFETY: the value is a `CFDataRef` (pointer-sized) that stays alive
    // for the call; the driver copies the bytes out before returning.
    let status = unsafe {
        AudioObjectSetPropertyData(
            plugin,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            size,
            NonNull::from(&data_ref).cast(),
        )
    };
    check("set Patchbay driver desired state", status)
}

/// Read the applied-state JSON.
pub(crate) fn applied_state(plugin: ObjectId) -> Result<String, HostError> {
    read_json(plugin, PROPERTY_APPLIED_STATE)
}

/// Read the runtime-stats JSON (IO counters; diagnostics).
pub(crate) fn runtime_stats(plugin: ObjectId) -> Result<String, HostError> {
    read_json(plugin, PROPERTY_RUNTIME_STATS)
}

fn read_json(plugin: ObjectId, selector: u32) -> Result<String, HostError> {
    let addr = global(selector);
    let mut raw: *const CFData = std::ptr::null();
    let mut size = u32::try_from(size_of::<*const CFData>()).unwrap_or(8);
    // SAFETY: `raw` is a pointer-sized out slot for the `CFDataRef`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            plugin,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut raw).cast(),
        )
    };
    check("get Patchbay driver property", status)?;
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Ok(String::new());
    };
    // SAFETY: the driver returns a +1 `CFData` (Create rule).
    let data = unsafe { CFRetained::from_raw(ptr) };
    Ok(String::from_utf8_lossy(&data.to_vec()).into_owned())
}
