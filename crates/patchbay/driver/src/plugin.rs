//! AudioServerPlugIn COM-style driver implementation (forked from MARS
//! `mars-hal`, MIT).
//!
//! Config updates arrive via `SetPropertyData` on the custom property
//! `'pbds'` and flow through the `DRIVER_STATE` machinery in `crate::lib`.
//! Every device is a **loopback**: an output stream apps play into and an
//! input stream that returns it, joined by a sample-time-indexed ring
//! (BlackHole's scheme). The applied state is persisted in coreaudiod's
//! plug-in storage and restored at `Initialize`.

use std::cell::UnsafeCell;
use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use arc_swap::ArcSwap;
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::coreaudio_types::*;
use crate::{
    DRIVER_STATE, RUNTIME_STATS, STORAGE_KEY_APPLIED_STATE, applied_as_desired, applied_state_json,
    configuration_summary_json, default_desired_state, perform_device_configuration_change,
    request_device_configuration_change, runtime_stats_json, set_desired_state_json,
};

// ===========================================================================
// Global state
// ===========================================================================

struct PatchbayDriverPlugin {
    ref_count: AtomicU32,
    plugin_object_id: AtomicU32,
    host: Mutex<Option<AudioServerPlugInHostRef>>,
    object_registry: Mutex<ObjectRegistry>,
}

// `PatchbayDriverPlugin` contains atomics, a `Mutex`, and a raw pointer behind `Mutex`.
// The `Mutex` guard ensures exclusive access to the host pointer; the `AtomicU32`
// is inherently `Send + Sync`.  The host pointer is only stored/read under the
// lock and is valid for the plugin's lifetime inside coreaudiod.
unsafe impl Send for PatchbayDriverPlugin {}
unsafe impl Sync for PatchbayDriverPlugin {}

#[derive(Debug)]
struct ObjectRegistry {
    next_id: AudioObjectID,
    devices: BTreeMap<String, DeviceObjectInfo>,
}

#[derive(Debug, Clone)]
struct DeviceObjectInfo {
    device_id: AudioObjectID,
    /// The input stream (what apps record from).
    input_stream_id: AudioObjectID,
    /// The output stream (what apps play into).
    output_stream_id: AudioObjectID,
    volume_control_id: Option<AudioObjectID>,
    uid: String,
    name: String,
    #[allow(dead_code)]
    kind: String,
    channels: u16,
    hidden: bool,
    /// Clients with IO running (StartIO/StopIO are per client).
    io_clients: u32,
    /// Realtime-shared state; the same `Arc` instance is published into
    /// [`RT_DEVICES`] so realtime callbacks observe mutations made through the
    /// registry without taking any lock.
    rt: Arc<RtDeviceState>,
}

impl DeviceObjectInfo {
    #[allow(clippy::too_many_arguments)]
    fn new(
        device_id: AudioObjectID,
        input_stream_id: AudioObjectID,
        output_stream_id: AudioObjectID,
        volume_control_id: Option<AudioObjectID>,
        uid: String,
        name: String,
        kind: String,
        channels: u16,
        hidden: bool,
    ) -> Self {
        let rt = Arc::new(RtDeviceState::new(channels));
        Self {
            device_id,
            input_stream_id,
            output_stream_id,
            volume_control_id,
            uid,
            name,
            kind,
            channels,
            hidden,
            io_clients: 0,
            rt,
        }
    }

    /// Whether `stream_id` is this device's input stream.
    fn is_input_stream(&self, stream_id: AudioObjectID) -> bool {
        stream_id == self.input_stream_id
    }

    fn volume_scalar(&self) -> Float32 {
        self.rt.volume_scalar()
    }
}

/// Per-device state shared with the realtime IO path.
///
/// `plugin_do_io_operation` and `plugin_get_zero_time_stamp` run on
/// coreaudiod's realtime thread and must not take blocking locks or allocate.
/// They resolve the device through the lock-free [`RT_DEVICES`] snapshot and
/// touch only the atomics (and the cached ring handle) below. Non-RT paths
/// mutate the same `Arc`'d instance through the object registry.
/// Frames in each device's loopback ring (~1.4 s at 48 kHz).
const RING_FRAMES: usize = 65_536;

/// Zero-timestamp period in frames (BlackHole's `kDevice_RingBufferSize`).
const ZERO_TS_PERIOD_FRAMES: u64 = 16_384;

/// A device's loopback ring: interleaved `f32`, indexed by sample time.
///
/// Written only by `WriteMix` and read only by `ReadInput`, both on the
/// device's single IO thread — the HAL serialises a device's IO
/// operations — so plain (unsynchronised) access through the cell is
/// race-free; the atomics carry state across cycles.
struct LoopbackRing {
    samples: UnsafeCell<Box<[f32]>>,
    channels: usize,
    /// `f64` bits: output sample time just past the last `WriteMix`.
    last_output_end: AtomicU64,
    /// The ring holds only silence.
    clear: AtomicBool,
}

// SAFETY: see the type docs — the cell is only touched from the device's
// IO thread, one operation at a time.
unsafe impl Sync for LoopbackRing {}
// SAFETY: owns plain data.
unsafe impl Send for LoopbackRing {}

impl std::fmt::Debug for LoopbackRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackRing")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl LoopbackRing {
    fn new(channels: u16) -> Self {
        let channels = usize::from(channels.max(1));
        Self {
            samples: UnsafeCell::new(vec![0.0; RING_FRAMES * channels].into_boxed_slice()),
            channels,
            last_output_end: AtomicU64::new(0.0_f64.to_bits()),
            clear: AtomicBool::new(true),
        }
    }

    /// `WriteMix`: copy `buffer` into the ring at output time `sample_time`.
    ///
    /// # Safety
    /// Called only from the device's IO thread (see the type docs).
    unsafe fn write(&self, sample_time: f64, buffer: &[f32], frames: usize) {
        // SAFETY: exclusive by the IO-thread contract.
        let ring = unsafe { &mut *self.samples.get() };
        let ch = self.channels;
        let start = (sample_time.max(0.0) as u64 % RING_FRAMES as u64) as usize;
        let first = frames.min(RING_FRAMES - start);
        let second = frames - first;
        ring[start * ch..(start + first) * ch].copy_from_slice(&buffer[..first * ch]);
        ring[..second * ch].copy_from_slice(&buffer[first * ch..frames * ch]);
        self.last_output_end
            .store((sample_time + frames as f64).to_bits(), Ordering::Relaxed);
        self.clear.store(false, Ordering::Relaxed);
        RUNTIME_STATS.write_count.fetch_add(1, Ordering::Relaxed);
        RUNTIME_STATS
            .last_write_time_bits
            .store(sample_time.to_bits(), Ordering::Relaxed);
    }

    /// `ReadInput`: fill `buffer` from the ring at input time
    /// `sample_time`, or with silence when nothing was written recently
    /// (clearing the ring once so stale audio never loops).
    ///
    /// # Safety
    /// Called only from the device's IO thread (see the type docs).
    unsafe fn read(
        &self,
        sample_time: f64,
        buffer: &mut [f32],
        frames: usize,
        gain: f32,
        muted: bool,
    ) {
        // SAFETY: exclusive by the IO-thread contract.
        let ring = unsafe { &mut *self.samples.get() };
        let ch = self.channels;
        RUNTIME_STATS.read_count.fetch_add(1, Ordering::Relaxed);
        RUNTIME_STATS
            .last_read_time_bits
            .store(sample_time.to_bits(), Ordering::Relaxed);
        let last = f64::from_bits(self.last_output_end.load(Ordering::Relaxed));
        if muted || last - (frames as f64) < sample_time {
            RUNTIME_STATS
                .silent_read_count
                .fetch_add(1, Ordering::Relaxed);
            buffer[..frames * ch].fill(0.0);
            if !self.clear.swap(true, Ordering::Relaxed) {
                ring.fill(0.0);
            }
            return;
        }
        let start = (sample_time.max(0.0) as u64 % RING_FRAMES as u64) as usize;
        let first = frames.min(RING_FRAMES - start);
        let second = frames - first;
        buffer[..first * ch].copy_from_slice(&ring[start * ch..(start + first) * ch]);
        buffer[first * ch..frames * ch].copy_from_slice(&ring[..second * ch]);
        if gain != 1.0 {
            for s in &mut buffer[..frames * ch] {
                *s *= gain;
            }
        }
    }
}

/// Per-device state shared with the realtime IO path.
///
/// `plugin_do_io_operation` and `plugin_get_zero_time_stamp` run on
/// coreaudiod's realtime thread and must not take blocking locks or
/// allocate. They resolve the device through the lock-free [`RT_DEVICES`]
/// snapshot and touch only the atomics and the ring below.
#[derive(Debug)]
struct RtDeviceState {
    channels: u16,
    /// f32 bits of the device volume scalar (applied on the input side).
    volume_scalar_bits: AtomicU32,
    muted: AtomicBool,
    zero_ts_seed: AtomicU64,
    /// Host-clock anchor captured at StartIO; the zero timestamp is derived
    /// from it so the device acts as a proper CoreAudio clock source.
    anchor_host_time: AtomicU64,
    /// Host ticks per zero-timestamp period (precomputed off the RT path).
    host_ticks_per_period: AtomicU64,
    /// Frames per zero-timestamp period.
    frames_per_period: AtomicU64,
    ring: LoopbackRing,
}

impl RtDeviceState {
    fn new(channels: u16) -> Self {
        Self {
            channels,
            volume_scalar_bits: AtomicU32::new(1.0_f32.to_bits()),
            muted: AtomicBool::new(false),
            zero_ts_seed: AtomicU64::new(0),
            anchor_host_time: AtomicU64::new(0),
            host_ticks_per_period: AtomicU64::new(0),
            frames_per_period: AtomicU64::new(0),
            ring: LoopbackRing::new(channels),
        }
    }

    fn volume_scalar(&self) -> Float32 {
        Float32::from_bits(self.volume_scalar_bits.load(Ordering::Relaxed))
    }

    fn set_volume_scalar(&self, scalar: Float32) {
        self.volume_scalar_bits
            .store(scalar.to_bits(), Ordering::Relaxed);
    }
}

/// Lock-free realtime view of the device registry, rebuilt by non-RT paths
/// whenever the device set changes.
static RT_DEVICES: Lazy<ArcSwap<HashMap<AudioObjectID, Arc<RtDeviceState>>>> =
    Lazy::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Publish the realtime snapshot from the current registry contents. Callers
/// must hold the `object_registry` lock so rebuilds cannot interleave.
fn publish_rt_snapshot(reg: &ObjectRegistry) {
    let map: HashMap<AudioObjectID, Arc<RtDeviceState>> = reg
        .devices
        .values()
        .map(|device| (device.device_id, device.rt.clone()))
        .collect();
    RT_DEVICES.store(Arc::new(map));
}

impl Default for ObjectRegistry {
    fn default() -> Self {
        Self {
            next_id: 2, // 1 = plugin object
            devices: BTreeMap::new(),
        }
    }
}

impl ObjectRegistry {
    fn allocate_id(&mut self) -> AudioObjectID {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    fn find_device_by_object(&self, object_id: AudioObjectID) -> Option<&DeviceObjectInfo> {
        self.devices.values().find(|d| d.device_id == object_id)
    }

    fn find_device_by_object_mut(
        &mut self,
        object_id: AudioObjectID,
    ) -> Option<&mut DeviceObjectInfo> {
        self.devices.values_mut().find(|d| d.device_id == object_id)
    }

    fn find_device_by_stream(&self, stream_id: AudioObjectID) -> Option<&DeviceObjectInfo> {
        self.devices
            .values()
            .find(|d| d.input_stream_id == stream_id || d.output_stream_id == stream_id)
    }

    fn find_device_by_control(&self, control_id: AudioObjectID) -> Option<&DeviceObjectInfo> {
        self.devices
            .values()
            .find(|d| d.volume_control_id == Some(control_id))
    }

    fn find_device_by_control_mut(
        &mut self,
        control_id: AudioObjectID,
    ) -> Option<&mut DeviceObjectInfo> {
        self.devices
            .values_mut()
            .find(|d| d.volume_control_id == Some(control_id))
    }

    fn all_device_ids(&self) -> Vec<AudioObjectID> {
        self.devices.values().map(|d| d.device_id).collect()
    }
}

static PLUGIN: Lazy<PatchbayDriverPlugin> = Lazy::new(|| PatchbayDriverPlugin {
    ref_count: AtomicU32::new(1),
    plugin_object_id: AtomicU32::new(0),
    host: Mutex::new(None),
    object_registry: Mutex::new(ObjectRegistry::default()),
});

// ===========================================================================
// COM interface (static)
// ===========================================================================

static INTERFACE: AudioServerPlugInDriverInterface = AudioServerPlugInDriverInterface {
    _reserved: core::ptr::null_mut(),
    query_interface: plugin_query_interface,
    add_ref: plugin_add_ref,
    release: plugin_release,
    initialize: plugin_initialize,
    create_device: plugin_create_device,
    destroy_device: plugin_destroy_device,
    add_device_client: plugin_add_device_client,
    remove_device_client: plugin_remove_device_client,
    perform_device_configuration_change: plugin_perform_device_configuration_change,
    abort_device_configuration_change: plugin_abort_device_configuration_change,
    has_property: plugin_has_property,
    is_property_settable: plugin_is_property_settable,
    get_property_data_size: plugin_get_property_data_size,
    get_property_data: plugin_get_property_data,
    set_property_data: plugin_set_property_data,
    start_io: plugin_start_io,
    stop_io: plugin_stop_io,
    get_zero_time_stamp: plugin_get_zero_time_stamp,
    will_do_io_operation: plugin_will_do_io_operation,
    begin_io_operation: plugin_begin_io_operation,
    do_io_operation: plugin_do_io_operation,
    end_io_operation: plugin_end_io_operation,
};

/// Wrapper for a raw pointer that is `Sync + Send`.
///
/// The interface pointer is to a `static` and lives for the entire process — it
/// is safe to share across threads.
struct SyncInterfacePtr(*const AudioServerPlugInDriverInterface);
unsafe impl Sync for SyncInterfacePtr {}
unsafe impl Send for SyncInterfacePtr {}

static INTERFACE_PTR: SyncInterfacePtr = SyncInterfacePtr(&INTERFACE);

// ===========================================================================
// Factory function — the single exported symbol for CoreAudio host
// ===========================================================================

/// CoreAudio host calls this to create the driver. Returns an
/// `AudioServerPlugInDriverRef`, which is a pointer to a pointer to the driver
/// interface struct.
///
/// # Safety
/// Must only be called by the CoreAudio host with valid CFAllocatorRef and CFUUID parameters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PatchbayAudioServerPlugInFactory(
    _allocator: *const c_void,
    _requested_type_uuid: *const c_void,
) -> *mut c_void {
    // Force lazy init.
    let _ = &*PLUGIN;
    (&INTERFACE_PTR.0 as *const *const AudioServerPlugInDriverInterface)
        .cast_mut()
        .cast::<c_void>()
}

// ===========================================================================
// COM / IUnknown
// ===========================================================================

unsafe extern "C" fn plugin_query_interface(
    _driver: *mut c_void,
    iid: REFIID,
    interface: *mut *mut c_void,
) -> HRESULT {
    if interface.is_null() {
        return E_NOINTERFACE;
    }

    if iid == IID_IUNKNOWN || iid == IID_AUDIO_SERVER_PLUGIN_DRIVER {
        PLUGIN.ref_count.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `interface` is non-null, checked above.
        unsafe {
            *interface = (&INTERFACE_PTR.0 as *const *const AudioServerPlugInDriverInterface)
                .cast_mut()
                .cast::<c_void>();
        }
        return S_OK;
    }

    E_NOINTERFACE
}

unsafe extern "C" fn plugin_add_ref(_driver: *mut c_void) -> ULONG {
    PLUGIN.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "C" fn plugin_release(_driver: *mut c_void) -> ULONG {
    let prev = PLUGIN.ref_count.fetch_sub(1, Ordering::Relaxed);
    prev.saturating_sub(1)
}

// ===========================================================================
// Lifecycle
// ===========================================================================

unsafe extern "C" fn plugin_initialize(
    _driver: AudioServerPlugInDriverRef,
    host: AudioServerPlugInHostRef,
) -> OSStatus {
    *PLUGIN.host.lock() = Some(host);
    // Publish what was applied last time (coreaudiod storage), or the
    // default Patchbay + Broadcast pair on a fresh install. No device
    // exists yet, so this applies synchronously.
    let restored = load_persisted_state();
    let json = restored
        .unwrap_or_else(|| serde_json::to_string(&default_desired_state()).unwrap_or_default());
    apply_desired_json(&json)
}

unsafe extern "C" fn plugin_create_device(
    _driver: AudioServerPlugInDriverRef,
    _description: CFDictionaryRef,
    _client_info: *const AudioServerPlugInClientInfo,
    _device_object_id: *mut AudioObjectID,
) -> OSStatus {
    K_AUDIO_HARDWARE_UNSUPPORTED_OPERATION_ERROR
}

unsafe extern "C" fn plugin_destroy_device(
    _driver: AudioServerPlugInDriverRef,
    device_object_id: AudioObjectID,
) -> OSStatus {
    let mut reg = PLUGIN.object_registry.lock();
    let uid_to_remove = reg
        .devices
        .iter()
        .find(|(_, info)| info.device_id == device_object_id)
        .map(|(uid, _)| uid.clone());
    if let Some(uid) = uid_to_remove {
        reg.devices.remove(&uid);
        publish_rt_snapshot(&reg);
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_add_device_client(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    _client_info: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    RUNTIME_STATS
        .add_client_count
        .fetch_add(1, Ordering::Relaxed);
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_remove_device_client(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    _client_info: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    K_AUDIO_HARDWARE_NO_ERROR
}

// ===========================================================================
// Configuration change
// ===========================================================================

unsafe extern "C" fn plugin_perform_device_configuration_change(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    change_action: u64,
    _change_info: *const c_void,
) -> OSStatus {
    // `change_action` is the generation token from `request_device_configuration_change()`.
    let result = perform_device_configuration_change(change_action);
    if result.is_err() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    // Sync object registry with newly applied state.
    sync_object_registry();
    persist_applied_state();

    // Notify host that device list may have changed.
    notify_device_list_changed();

    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_abort_device_configuration_change(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    _change_action: u64,
    _change_info: *const c_void,
) -> OSStatus {
    // Clear pending change in DRIVER_STATE.
    let mut state = DRIVER_STATE.lock();
    state.pending_change = None;
    K_AUDIO_HARDWARE_NO_ERROR
}

// ===========================================================================
// Property dispatch helpers
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectType {
    Plugin,
    Device,
    Stream,
    Control,
}

const VOLUME_MIN_DECIBELS: Float32 = -96.0;
const VOLUME_MAX_DECIBELS: Float32 = 0.0;

fn classify_object(object_id: AudioObjectID) -> Option<ObjectType> {
    let runtime_plugin_object_id = runtime_plugin_object_id();
    if object_id == K_AUDIO_OBJECT_PLUGIN_OBJECT || object_id == runtime_plugin_object_id {
        return Some(ObjectType::Plugin);
    }
    let reg = PLUGIN.object_registry.lock();
    if reg.find_device_by_object(object_id).is_some() {
        return Some(ObjectType::Device);
    }
    if reg.find_device_by_stream(object_id).is_some() {
        return Some(ObjectType::Stream);
    }
    if reg.find_device_by_control(object_id).is_some() {
        return Some(ObjectType::Control);
    }
    None
}

fn remember_plugin_object_id(object_id: AudioObjectID) {
    if object_id != 0 {
        PLUGIN.plugin_object_id.store(object_id, Ordering::Relaxed);
    }
}

fn runtime_plugin_object_id() -> AudioObjectID {
    match PLUGIN.plugin_object_id.load(Ordering::Relaxed) {
        0 => K_AUDIO_OBJECT_PLUGIN_OBJECT,
        object_id => object_id,
    }
}

fn class_matches_qualifier(object_class: UInt32, qualifier_class: UInt32) -> bool {
    match object_class {
        K_AUDIO_OBJECT_CLASS_ID => qualifier_class == K_AUDIO_OBJECT_CLASS_ID,
        K_AUDIO_PLUG_IN_CLASS_ID => {
            matches!(
                qualifier_class,
                K_AUDIO_PLUG_IN_CLASS_ID | K_AUDIO_OBJECT_CLASS_ID
            )
        }
        K_AUDIO_DEVICE_CLASS_ID => {
            matches!(
                qualifier_class,
                K_AUDIO_DEVICE_CLASS_ID | K_AUDIO_OBJECT_CLASS_ID
            )
        }
        K_AUDIO_STREAM_CLASS_ID => {
            matches!(
                qualifier_class,
                K_AUDIO_STREAM_CLASS_ID | K_AUDIO_OBJECT_CLASS_ID
            )
        }
        K_AUDIO_CONTROL_CLASS_ID => {
            matches!(
                qualifier_class,
                K_AUDIO_CONTROL_CLASS_ID | K_AUDIO_OBJECT_CLASS_ID
            )
        }
        K_AUDIO_LEVEL_CONTROL_CLASS_ID => matches!(
            qualifier_class,
            K_AUDIO_LEVEL_CONTROL_CLASS_ID | K_AUDIO_CONTROL_CLASS_ID | K_AUDIO_OBJECT_CLASS_ID
        ),
        K_AUDIO_VOLUME_CONTROL_CLASS_ID => matches!(
            qualifier_class,
            K_AUDIO_VOLUME_CONTROL_CLASS_ID
                | K_AUDIO_LEVEL_CONTROL_CLASS_ID
                | K_AUDIO_CONTROL_CLASS_ID
                | K_AUDIO_OBJECT_CLASS_ID
        ),
        _ => qualifier_class == object_class || qualifier_class == K_AUDIO_OBJECT_CLASS_ID,
    }
}

fn qualifier_allows_class(
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
    object_class: UInt32,
) -> bool {
    if qualifier_data_size == 0 {
        return true;
    }
    if qualifier_data.is_null()
        || !(qualifier_data_size as usize).is_multiple_of(size_of::<UInt32>())
    {
        return false;
    }

    let class_count = (qualifier_data_size as usize) / size_of::<UInt32>();
    // SAFETY: null is rejected above and the host guarantees the qualifier buffer
    // is valid for `qualifier_data_size` bytes.
    let qualifier_classes =
        unsafe { core::slice::from_raw_parts(qualifier_data.cast::<UInt32>(), class_count) };

    qualifier_classes
        .iter()
        .copied()
        .any(|qualifier_class| class_matches_qualifier(object_class, qualifier_class))
}

/// The device's streams visible in `scope` (input, output or both).
fn device_streams_in_scope(dev: &DeviceObjectInfo, scope: UInt32) -> Vec<AudioObjectID> {
    match scope {
        K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL => vec![dev.input_stream_id, dev.output_stream_id],
        K_AUDIO_OBJECT_PROPERTY_SCOPE_INPUT => vec![dev.input_stream_id],
        K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT => vec![dev.output_stream_id],
        _ => Vec::new(),
    }
}

fn device_supports_volume(_dev: &DeviceObjectInfo) -> bool {
    true
}

fn device_volume_element_matches(dev: &DeviceObjectInfo, element: UInt32) -> bool {
    element == K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN
        || (1..=UInt32::from(dev.channels)).contains(&element)
}

fn device_volume_address_matches(
    dev: &DeviceObjectInfo,
    addr: &AudioObjectPropertyAddress,
) -> bool {
    device_supports_volume(dev)
        && matches!(
            addr.m_scope,
            K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL | K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT
        )
        && device_volume_element_matches(dev, addr.m_element)
}

fn volume_scalar_to_decibels(volume_scalar: Float32) -> Float32 {
    if volume_scalar <= 0.0 {
        VOLUME_MIN_DECIBELS
    } else {
        (20.0 * volume_scalar.log10()).clamp(VOLUME_MIN_DECIBELS, VOLUME_MAX_DECIBELS)
    }
}

fn volume_decibels_to_scalar(volume_db: Float32) -> Float32 {
    if volume_db <= VOLUME_MIN_DECIBELS {
        0.0
    } else {
        10.0_f32
            .powf(volume_db.clamp(VOLUME_MIN_DECIBELS, VOLUME_MAX_DECIBELS) / 20.0)
            .clamp(0.0, 1.0)
    }
}

fn clamp_volume_scalar(volume_scalar: Float32) -> Float32 {
    volume_scalar.clamp(0.0, 1.0)
}

fn write_audio_object_ids(
    ids: &[AudioObjectID],
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let byte_len = size_of_val(ids);
    if byte_len == 0 {
        // SAFETY: `out_data_size` is guaranteed by the caller.
        unsafe { *out_data_size = 0 };
        return K_AUDIO_HARDWARE_NO_ERROR;
    }
    if (data_size as usize) < byte_len {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: buffer bounds checked above and `ids` is a contiguous slice.
    unsafe {
        core::ptr::copy_nonoverlapping(ids.as_ptr(), data.cast::<AudioObjectID>(), ids.len());
        *out_data_size = byte_len as UInt32;
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

fn plugin_has_property_for(selector: UInt32) -> bool {
    matches!(
        selector,
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS
            | K_AUDIO_OBJECT_PROPERTY_CLASS
            | K_AUDIO_OBJECT_PROPERTY_OWNER
            | K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS
            | K_AUDIO_PLUG_IN_PROPERTY_BUNDLE_ID
            | K_AUDIO_PLUG_IN_PROPERTY_DEVICE_LIST
            | K_AUDIO_PLUG_IN_PROPERTY_RESOURCE_BUNDLE
            | K_AUDIO_OBJECT_PROPERTY_CUSTOM_PROPERTY_INFO_LIST
            | K_PB_PROPERTY_DESIRED_STATE
            | K_PB_PROPERTY_APPLIED_STATE
            | K_PB_PROPERTY_RUNTIME_STATS
            | K_PB_PROPERTY_CONFIG_SUMMARY
    )
}

fn device_has_property_for(object_id: AudioObjectID, addr: &AudioObjectPropertyAddress) -> bool {
    let volume_selector_matches = matches!(
        addr.m_selector,
        K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR
            | K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS
            | K_AUDIO_DEVICE_PROPERTY_VOLUME_RANGE_DECIBELS
            | K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR_TO_DECIBELS
            | K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS_TO_SCALAR
    );
    if volume_selector_matches {
        let reg = PLUGIN.object_registry.lock();
        let Some(dev) = reg.find_device_by_object(object_id) else {
            return false;
        };
        return device_volume_address_matches(dev, addr);
    }

    matches!(
        addr.m_selector,
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS
            | K_AUDIO_OBJECT_PROPERTY_CLASS
            | K_AUDIO_OBJECT_PROPERTY_OWNER
            | K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS
            | K_AUDIO_OBJECT_PROPERTY_CONTROL_LIST
            | K_AUDIO_OBJECT_PROPERTY_NAME
            | K_AUDIO_OBJECT_PROPERTY_MANUFACTURER
            | K_AUDIO_DEVICE_PROPERTY_DEVICE_UID
            | K_AUDIO_DEVICE_PROPERTY_MODEL_UID
            | K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE
            | K_AUDIO_DEVICE_PROPERTY_DEVICE_CAN_BE_DEFAULT_DEVICE
            | K_AUDIO_DEVICE_PROPERTY_DEVICE_CAN_BE_DEFAULT_SYSTEM_DEVICE
            | K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_HIDDEN
            | K_AUDIO_DEVICE_PROPERTY_LATENCY
            | K_AUDIO_DEVICE_PROPERTY_STREAMS
            | K_AUDIO_DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE
            | K_AUDIO_DEVICE_PROPERTY_AVAILABLE_NOMINAL_SAMPLE_RATES
            | K_AUDIO_DEVICE_PROPERTY_ZERO_TIME_STAMP_PERIOD
            | K_AUDIO_DEVICE_PROPERTY_SAFETY_OFFSET
            | K_AUDIO_DEVICE_PROPERTY_CLOCK_DOMAIN
            | K_AUDIO_DEVICE_PROPERTY_IS_ALIVE
            | K_AUDIO_DEVICE_PROPERTY_IS_RUNNING
            | K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE
            | K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE_RANGE
            | K_AUDIO_DEVICE_PROPERTY_PREFERRED_CHANNELS_FOR_STEREO
    )
}

fn stream_has_property_for(selector: UInt32) -> bool {
    matches!(
        selector,
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS
            | K_AUDIO_OBJECT_PROPERTY_CLASS
            | K_AUDIO_OBJECT_PROPERTY_OWNER
            | K_AUDIO_STREAM_PROPERTY_DIRECTION
            | K_AUDIO_STREAM_PROPERTY_TERMINAL_TYPE
            | K_AUDIO_STREAM_PROPERTY_START_CHANNEL
            | K_AUDIO_STREAM_PROPERTY_VIRTUAL_FORMAT
            | K_AUDIO_STREAM_PROPERTY_PHYSICAL_FORMAT
            | K_AUDIO_STREAM_PROPERTY_AVAILABLE_VIRTUAL_FORMATS
            | K_AUDIO_STREAM_PROPERTY_AVAILABLE_PHYSICAL_FORMATS
            | K_AUDIO_STREAM_PROPERTY_LATENCY
            | K_AUDIO_STREAM_PROPERTY_IS_ACTIVE
    )
}

fn control_has_property_for(selector: UInt32) -> bool {
    matches!(
        selector,
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS
            | K_AUDIO_OBJECT_PROPERTY_CLASS
            | K_AUDIO_OBJECT_PROPERTY_OWNER
            | K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS
            | K_AUDIO_OBJECT_PROPERTY_NAME
            | K_AUDIO_OBJECT_PROPERTY_MANUFACTURER
            | K_AUDIO_CONTROL_PROPERTY_SCOPE
            | K_AUDIO_CONTROL_PROPERTY_ELEMENT
            | K_AUDIO_LEVEL_CONTROL_PROPERTY_SCALAR_VALUE
            | K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_VALUE
            | K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_RANGE
            | K_AUDIO_LEVEL_CONTROL_PROPERTY_CONVERT_SCALAR_TO_DECIBELS
            | K_AUDIO_LEVEL_CONTROL_PROPERTY_CONVERT_DECIBELS_TO_SCALAR
    )
}

fn resolve_property_object(object_id: AudioObjectID, selector: UInt32) -> Option<ObjectType> {
    if let Some(object_type) = classify_object(object_id) {
        if object_type == ObjectType::Plugin {
            remember_plugin_object_id(object_id);
        }
        return Some(object_type);
    }
    if plugin_has_property_for(selector) {
        remember_plugin_object_id(object_id);
        return Some(ObjectType::Plugin);
    }
    None
}

// ===========================================================================
// Property operations
// ===========================================================================

unsafe extern "C" fn plugin_has_property(
    _driver: AudioServerPlugInDriverRef,
    object_id: AudioObjectID,
    _client_process_id: i32,
    address: *const AudioObjectPropertyAddress,
) -> Boolean {
    if address.is_null() {
        return 0;
    }
    // SAFETY: `address` is non-null, provided by the host.
    let addr = unsafe { &*address };
    let resolved = resolve_property_object(object_id, addr.m_selector);
    let has = match resolved {
        Some(ObjectType::Plugin) => plugin_has_property_for(addr.m_selector),
        Some(ObjectType::Device) => device_has_property_for(object_id, addr),
        Some(ObjectType::Stream) => stream_has_property_for(addr.m_selector),
        Some(ObjectType::Control) => control_has_property_for(addr.m_selector),
        None => false,
    };
    if has { 1 } else { 0 }
}

unsafe extern "C" fn plugin_is_property_settable(
    _driver: AudioServerPlugInDriverRef,
    object_id: AudioObjectID,
    _client_process_id: i32,
    address: *const AudioObjectPropertyAddress,
    is_settable: *mut Boolean,
) -> OSStatus {
    if address.is_null() || is_settable.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: `address` is non-null, provided by the host.
    let addr = unsafe { &*address };

    let resolved = resolve_property_object(object_id, addr.m_selector);
    let settable = match resolved {
        Some(ObjectType::Plugin) => addr.m_selector == K_PB_PROPERTY_DESIRED_STATE,
        Some(ObjectType::Device) => {
            addr.m_selector == K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE
                || addr.m_selector == K_AUDIO_DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE
                || (matches!(
                    addr.m_selector,
                    K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR | K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS
                ) && {
                    let reg = PLUGIN.object_registry.lock();
                    let Some(dev) = reg.find_device_by_object(object_id) else {
                        return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
                    };
                    device_volume_address_matches(dev, addr)
                })
        }
        Some(ObjectType::Stream) => matches!(
            addr.m_selector,
            K_AUDIO_STREAM_PROPERTY_VIRTUAL_FORMAT | K_AUDIO_STREAM_PROPERTY_PHYSICAL_FORMAT
        ),
        Some(ObjectType::Control) => matches!(
            addr.m_selector,
            K_AUDIO_LEVEL_CONTROL_PROPERTY_SCALAR_VALUE
                | K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_VALUE
        ),
        None => return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR,
    };

    // SAFETY: `is_settable` is non-null, checked above.
    unsafe { *is_settable = if settable { 1 } else { 0 } };
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_get_property_data_size(
    _driver: AudioServerPlugInDriverRef,
    object_id: AudioObjectID,
    _client_process_id: i32,
    address: *const AudioObjectPropertyAddress,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
    data_size: *mut UInt32,
) -> OSStatus {
    if address.is_null() || data_size.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: `address` is non-null, provided by the host.
    let addr = unsafe { &*address };

    let resolved = resolve_property_object(object_id, addr.m_selector);
    let size = match resolved {
        Some(ObjectType::Plugin) => {
            plugin_property_data_size(addr.m_selector, qualifier_data_size, qualifier_data)
        }
        Some(ObjectType::Device) => {
            device_property_data_size(object_id, addr, qualifier_data_size, qualifier_data)
        }
        Some(ObjectType::Stream) => stream_property_data_size(addr.m_selector),
        Some(ObjectType::Control) => control_property_data_size(addr.m_selector),
        None => return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR,
    };

    match size {
        Some(s) => {
            // SAFETY: `data_size` is non-null, checked above.
            unsafe { *data_size = s };
            K_AUDIO_HARDWARE_NO_ERROR
        }
        None => K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR,
    }
}

unsafe extern "C" fn plugin_get_property_data(
    _driver: AudioServerPlugInDriverRef,
    object_id: AudioObjectID,
    _client_process_id: i32,
    address: *const AudioObjectPropertyAddress,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    if address.is_null() || out_data_size.is_null() || data.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: `address` is non-null, provided by the host.
    let addr = unsafe { &*address };
    let resolved = resolve_property_object(object_id, addr.m_selector);

    // SAFETY: all pointer arguments have been validated above; callee contracts
    // are satisfied by the host-provided buffer.

    unsafe {
        match resolved {
            Some(ObjectType::Plugin) => plugin_get_property(
                addr.m_selector,
                qualifier_data_size,
                qualifier_data,
                data_size,
                out_data_size,
                data,
            ),
            Some(ObjectType::Device) => device_get_property(
                object_id,
                addr,
                qualifier_data_size,
                qualifier_data,
                data_size,
                out_data_size,
                data,
            ),
            Some(ObjectType::Stream) => {
                stream_get_property(object_id, addr, data_size, out_data_size, data)
            }
            Some(ObjectType::Control) => {
                control_get_property(object_id, addr, data_size, out_data_size, data)
            }
            None => K_AUDIO_HARDWARE_BAD_OBJECT_ERROR,
        }
    }
}

unsafe extern "C" fn plugin_set_property_data(
    _driver: AudioServerPlugInDriverRef,
    object_id: AudioObjectID,
    _client_process_id: i32,
    address: *const AudioObjectPropertyAddress,
    _qualifier_data_size: UInt32,
    _qualifier_data: *const c_void,
    data_size: UInt32,
    data: *const c_void,
) -> OSStatus {
    if address.is_null() || data.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: `address` is non-null, provided by the host.
    let addr = unsafe { &*address };

    let resolved = resolve_property_object(object_id, addr.m_selector);

    match resolved {
        Some(ObjectType::Plugin) => {
            if addr.m_selector == K_PB_PROPERTY_DESIRED_STATE {
                // SAFETY: `data` is non-null (checked above) and points to `data_size` bytes.
                unsafe { set_desired_state_from_raw(data, data_size) }
            } else {
                K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR
            }
        }
        Some(ObjectType::Device) => unsafe {
            device_set_property(object_id, addr, data_size, data)
        },
        Some(ObjectType::Control) => unsafe {
            control_set_property(object_id, addr, data_size, data)
        },
        Some(ObjectType::Stream) => K_AUDIO_HARDWARE_UNSUPPORTED_OPERATION_ERROR,
        None => K_AUDIO_HARDWARE_BAD_OBJECT_ERROR,
    }
}

fn notify_properties_changed_for_object(
    object_id: AudioObjectID,
    addresses: &[AudioObjectPropertyAddress],
) {
    let host_guard = PLUGIN.host.lock();
    let Some(host) = *host_guard else {
        return;
    };
    // SAFETY: `host` is a valid AudioServerPlugInHostRef from coreaudiod and the
    // address slice lives for the duration of the call.
    unsafe {
        ((*host).properties_changed)(
            host,
            object_id,
            addresses.len() as UInt32,
            addresses.as_ptr(),
        );
    }
}

fn notify_volume_changed(device_id: AudioObjectID, control_id: AudioObjectID, element: UInt32) {
    let mut device_addresses =
        Vec::with_capacity(if element == K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN {
            2
        } else {
            4
        });
    device_addresses.push(AudioObjectPropertyAddress {
        m_selector: K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR,
        m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
        m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    });
    device_addresses.push(AudioObjectPropertyAddress {
        m_selector: K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS,
        m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
        m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    });
    if element != K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN {
        device_addresses.push(AudioObjectPropertyAddress {
            m_selector: K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
            m_element: element,
        });
        device_addresses.push(AudioObjectPropertyAddress {
            m_selector: K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
            m_element: element,
        });
    }
    notify_properties_changed_for_object(device_id, &device_addresses);

    let control_addresses = [
        AudioObjectPropertyAddress {
            m_selector: K_AUDIO_LEVEL_CONTROL_PROPERTY_SCALAR_VALUE,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        },
        AudioObjectPropertyAddress {
            m_selector: K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_VALUE,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        },
    ];
    notify_properties_changed_for_object(control_id, &control_addresses);
}

fn set_device_volume_scalar(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    new_scalar: Float32,
) -> Result<(AudioObjectID, AudioObjectID), OSStatus> {
    let mut reg = PLUGIN.object_registry.lock();
    let Some(dev) = reg.find_device_by_object_mut(object_id) else {
        return Err(K_AUDIO_HARDWARE_BAD_OBJECT_ERROR);
    };
    if !device_volume_address_matches(dev, addr) {
        return Err(K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR);
    }
    let Some(control_id) = dev.volume_control_id else {
        return Err(K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR);
    };
    dev.rt.set_volume_scalar(clamp_volume_scalar(new_scalar));
    Ok((dev.device_id, control_id))
}

fn set_control_volume_scalar(
    object_id: AudioObjectID,
    new_scalar: Float32,
) -> Result<(AudioObjectID, AudioObjectID), OSStatus> {
    let mut reg = PLUGIN.object_registry.lock();
    let Some(dev) = reg.find_device_by_control_mut(object_id) else {
        return Err(K_AUDIO_HARDWARE_BAD_OBJECT_ERROR);
    };
    let Some(control_id) = dev.volume_control_id else {
        return Err(K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR);
    };
    dev.rt.set_volume_scalar(clamp_volume_scalar(new_scalar));
    Ok((dev.device_id, control_id))
}

unsafe fn device_set_property(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    data_size: UInt32,
    data: *const c_void,
) -> OSStatus {
    // Virtual devices advertise exactly one nominal sample rate (the applied
    // configuration), so a set is accepted only as a no-op for that rate.
    // External producers (issue #48) depend on clients being unable to move
    // the device to a different rate.
    if addr.m_selector == K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE {
        // Clients pick their own IO size; the ring is indexed by sample
        // time, so any size within the advertised range works.
        if (data_size as usize) < size_of::<UInt32>() {
            return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
        }
        // SAFETY: size checked above; caller guarantees readability.
        let requested = unsafe { core::ptr::read_unaligned(data.cast::<UInt32>()) };
        let max = {
            let state = DRIVER_STATE.lock();
            state.applied_state.buffer_frames.saturating_mul(4)
        };
        return if requested > 0 && requested <= max {
            K_AUDIO_HARDWARE_NO_ERROR
        } else {
            K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR
        };
    }
    if addr.m_selector == K_AUDIO_DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE {
        if (data_size as usize) < size_of::<Float64>() {
            return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
        }
        // SAFETY: size checked above and caller guarantees readability.
        let requested = unsafe { core::ptr::read_unaligned(data.cast::<Float64>()) };
        let applied = {
            let state = DRIVER_STATE.lock();
            f64::from(state.applied_state.sample_rate)
        };
        return if (requested - applied).abs() < 0.5 {
            K_AUDIO_HARDWARE_NO_ERROR
        } else {
            K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR
        };
    }

    if (data_size as usize) < size_of::<Float32>() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: size checked above and caller guarantees readability for `data_size` bytes.
    let input = unsafe { core::ptr::read_unaligned(data.cast::<Float32>()) };
    let set_result = match addr.m_selector {
        K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR => set_device_volume_scalar(object_id, addr, input),
        K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS => {
            set_device_volume_scalar(object_id, addr, volume_decibels_to_scalar(input))
        }
        _ => return K_AUDIO_HARDWARE_UNSUPPORTED_OPERATION_ERROR,
    };

    let (device_id, control_id) = match set_result {
        Ok(ids) => ids,
        Err(status) => return status,
    };
    notify_volume_changed(device_id, control_id, addr.m_element);
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe fn control_set_property(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    data_size: UInt32,
    data: *const c_void,
) -> OSStatus {
    if (data_size as usize) < size_of::<Float32>() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: size checked above and caller guarantees readability for `data_size` bytes.
    let input = unsafe { core::ptr::read_unaligned(data.cast::<Float32>()) };
    let set_result = match addr.m_selector {
        K_AUDIO_LEVEL_CONTROL_PROPERTY_SCALAR_VALUE => set_control_volume_scalar(object_id, input),
        K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_VALUE => {
            set_control_volume_scalar(object_id, volume_decibels_to_scalar(input))
        }
        _ => return K_AUDIO_HARDWARE_UNSUPPORTED_OPERATION_ERROR,
    };

    let (device_id, control_id) = match set_result {
        Ok(ids) => ids,
        Err(status) => return status,
    };
    notify_volume_changed(device_id, control_id, K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN);
    K_AUDIO_HARDWARE_NO_ERROR
}

// ===========================================================================
// IO operations
// ===========================================================================

unsafe extern "C" fn plugin_start_io(
    _driver: AudioServerPlugInDriverRef,
    device_object_id: AudioObjectID,
    _client_id: UInt32,
) -> OSStatus {
    RUNTIME_STATS.start_io_count.fetch_add(1, Ordering::Relaxed);
    let (rt, already_running) = {
        let reg = PLUGIN.object_registry.lock();
        let Some(dev) = reg.find_device_by_object(device_object_id) else {
            return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
        };
        (dev.rt.clone(), dev.io_clients > 0)
    };
    // StartIO/StopIO are per client. The clock is armed only when the
    // first client starts: re-anchoring under running clients would move
    // the device's timeline backwards and wedge the HAL's IO engine.
    if !already_running {
        let sample_rate = u64::from(DRIVER_STATE.lock().applied_state.sample_rate.max(1));
        // Arm the device clock: the zero timestamp must advance in real
        // time from a host-clock anchor (the device IS the clock source
        // for its IO graph).
        let mut timebase = MachTimebaseInfo::default();
        // SAFETY: mach_timebase_info with a valid out pointer is always safe.
        let _ = unsafe { mach_timebase_info(&mut timebase) };
        let period_ns = ZERO_TS_PERIOD_FRAMES.saturating_mul(1_000_000_000) / sample_rate;
        let ticks_per_period = if timebase.numer == 0 {
            period_ns
        } else {
            period_ns.saturating_mul(u64::from(timebase.denom)) / u64::from(timebase.numer.max(1))
        };
        rt.frames_per_period
            .store(ZERO_TS_PERIOD_FRAMES, Ordering::Relaxed);
        rt.host_ticks_per_period
            .store(ticks_per_period.max(1), Ordering::Relaxed);
        // SAFETY: mach_absolute_time is always safe to call on macOS.
        rt.anchor_host_time
            .store(unsafe { mach_absolute_time() }, Ordering::Relaxed);
        rt.zero_ts_seed.fetch_add(1, Ordering::Release);
    }
    let mut reg = PLUGIN.object_registry.lock();
    if let Some(dev) = reg.find_device_by_object_mut(device_object_id) {
        dev.io_clients = dev.io_clients.saturating_add(1);
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_stop_io(
    _driver: AudioServerPlugInDriverRef,
    device_object_id: AudioObjectID,
    _client_id: UInt32,
) -> OSStatus {
    RUNTIME_STATS.stop_io_count.fetch_add(1, Ordering::Relaxed);
    let mut reg = PLUGIN.object_registry.lock();
    if let Some(dev) = reg.find_device_by_object_mut(device_object_id) {
        dev.io_clients = dev.io_clients.saturating_sub(1);
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_get_zero_time_stamp(
    _driver: AudioServerPlugInDriverRef,
    device_object_id: AudioObjectID,
    _client_id: UInt32,
    out_sample_time: *mut Float64,
    out_host_time: *mut u64,
    out_seed: *mut u64,
) -> OSStatus {
    if out_sample_time.is_null() || out_host_time.is_null() || out_seed.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    // Realtime context: resolve through the lock-free snapshot; never block.
    RUNTIME_STATS
        .zero_timestamp_count
        .fetch_add(1, Ordering::Relaxed);
    let snapshot = RT_DEVICES.load();
    let Some(device) = snapshot.get(&device_object_id) else {
        return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
    };
    // Acquire on the seed pairs with the Release in `reset_timeline`: a reader
    // observing a new seed also observes the armed clock fields.
    let seed = device.zero_ts_seed.load(Ordering::Acquire);
    let anchor = device.anchor_host_time.load(Ordering::Relaxed);
    let ticks_per_period = device.host_ticks_per_period.load(Ordering::Relaxed);
    let frames_per_period = device.frames_per_period.load(Ordering::Relaxed);

    // The device is the clock source for its IO graph: report the most
    // recent period boundary as a (sample_time, host_time) pair derived from
    // the host clock. All loads are atomics — this path stays RT-safe.
    // SAFETY: mach_absolute_time is always safe to call on macOS.
    let now = unsafe { mach_absolute_time() };
    let (sample_time, host_time) = if anchor == 0 || ticks_per_period == 0 {
        (0.0, now)
    } else {
        let periods = now.saturating_sub(anchor) / ticks_per_period;
        (
            (periods.saturating_mul(frames_per_period)) as f64,
            anchor.saturating_add(periods.saturating_mul(ticks_per_period)),
        )
    };
    // SAFETY: all output pointers verified non-null above.
    unsafe {
        *out_sample_time = sample_time;
        *out_host_time = host_time;
        *out_seed = seed;
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_will_do_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    _client_id: UInt32,
    operation_id: UInt32,
    will_do: *mut Boolean,
    will_do_in_place: *mut Boolean,
) -> OSStatus {
    if will_do.is_null() || will_do_in_place.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    RUNTIME_STATS
        .last_will_op
        .store(u64::from(operation_id), Ordering::Relaxed);
    if operation_id == K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_WRITE_MIX {
        RUNTIME_STATS
            .will_write_count
            .fetch_add(1, Ordering::Relaxed);
    } else if operation_id == K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_READ_INPUT {
        RUNTIME_STATS
            .will_read_count
            .fetch_add(1, Ordering::Relaxed);
    } else {
        RUNTIME_STATS
            .will_other_count
            .fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: output pointers are non-null.
    unsafe {
        *will_do = if operation_id == K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_WRITE_MIX
            || operation_id == K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_READ_INPUT
        {
            1
        } else {
            0
        };
        *will_do_in_place = 1;
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_begin_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    _client_id: UInt32,
    _operation_id: UInt32,
    _io_buffer_frame_size: UInt32,
    _io_cycle_info: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    RUNTIME_STATS.begin_io_count.fetch_add(1, Ordering::Relaxed);
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_do_io_operation(
    _driver: AudioServerPlugInDriverRef,
    device_object_id: AudioObjectID,
    _stream_object_id: AudioObjectID,
    _client_id: UInt32,
    operation_id: UInt32,
    io_buffer_frame_size: UInt32,
    io_cycle_info: *const AudioServerPlugInIOCycleInfo,
    io_main_buffer: *mut c_void,
    _io_secondary_buffer: *mut c_void,
) -> OSStatus {
    // Realtime context: no blocking locks, no allocation.
    RUNTIME_STATS.do_io_count.fetch_add(1, Ordering::Relaxed);
    if io_cycle_info.is_null() || io_main_buffer.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    let snapshot = RT_DEVICES.load();
    let Some(dev) = snapshot.get(&device_object_id) else {
        return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
    };
    let frames = (io_buffer_frame_size as usize).min(RING_FRAMES);
    let channels = usize::from(dev.channels.max(1));
    // SAFETY: the host passes a valid cycle info for the duration of the call.
    let cycle = unsafe { &*io_cycle_info };
    // SAFETY: `io_main_buffer` holds `io_buffer_frame_size` frames of
    // `channels` interleaved f32 samples (our stream format).
    let buffer: &mut [f32] =
        unsafe { core::slice::from_raw_parts_mut(io_main_buffer.cast::<f32>(), frames * channels) };

    if operation_id == K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_WRITE_MIX {
        // SAFETY: DoIOOperation runs on this device's IO thread.
        unsafe {
            dev.ring
                .write(cycle.m_output_time.m_sample_time, buffer, frames)
        };
    } else if operation_id == K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_READ_INPUT {
        // SAFETY: DoIOOperation runs on this device's IO thread.
        unsafe {
            dev.ring.read(
                cycle.m_input_time.m_sample_time,
                buffer,
                frames,
                dev.volume_scalar(),
                dev.muted.load(Ordering::Relaxed),
            );
        }
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

unsafe extern "C" fn plugin_end_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _device_object_id: AudioObjectID,
    _client_id: UInt32,
    _operation_id: UInt32,
    _io_buffer_frame_size: UInt32,
    _io_cycle_info: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    K_AUDIO_HARDWARE_NO_ERROR
}

// ===========================================================================
// Property data — Plugin object
// ===========================================================================

fn plugin_property_data_size(
    selector: UInt32,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
) -> Option<UInt32> {
    Some(match selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS | K_AUDIO_OBJECT_PROPERTY_CLASS => {
            size_of::<UInt32>() as UInt32
        }
        K_AUDIO_OBJECT_PROPERTY_OWNER => size_of::<AudioObjectID>() as UInt32,
        K_AUDIO_PLUG_IN_PROPERTY_BUNDLE_ID | K_AUDIO_PLUG_IN_PROPERTY_RESOURCE_BUNDLE => {
            size_of::<*const c_void>() as UInt32 // CFStringRef
        }
        K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS => {
            let count = if qualifier_allows_class(
                qualifier_data_size,
                qualifier_data,
                K_AUDIO_DEVICE_CLASS_ID,
            ) {
                let reg = PLUGIN.object_registry.lock();
                reg.devices.len()
            } else {
                0
            };
            (count * size_of::<AudioObjectID>()) as UInt32
        }
        K_AUDIO_PLUG_IN_PROPERTY_DEVICE_LIST => {
            let reg = PLUGIN.object_registry.lock();
            (reg.devices.len() * size_of::<AudioObjectID>()) as UInt32
        }
        K_AUDIO_OBJECT_PROPERTY_CUSTOM_PROPERTY_INFO_LIST => {
            (4 * size_of::<AudioServerPlugInCustomPropertyInfo>()) as UInt32
        }
        K_PB_PROPERTY_DESIRED_STATE
        | K_PB_PROPERTY_APPLIED_STATE
        | K_PB_PROPERTY_RUNTIME_STATS
        | K_PB_PROPERTY_CONFIG_SUMMARY => {
            // Return a generous upper bound; actual size is written on GetPropertyData.
            64 * 1024
        }
        _ => return None,
    })
}

/// Write plugin property data into the host-provided buffer.
///
/// # Safety
/// `data` must point to a writable buffer of at least `data_size` bytes.
/// `out_data_size` must be a valid pointer.
unsafe fn plugin_get_property(
    selector: UInt32,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    match selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS => unsafe {
            write_val::<UInt32>(K_AUDIO_OBJECT_CLASS_ID, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_CLASS => unsafe {
            write_val::<UInt32>(K_AUDIO_PLUG_IN_CLASS_ID, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_OWNER => unsafe {
            write_val::<AudioObjectID>(K_AUDIO_OBJECT_SYSTEM_OBJECT, data_size, out_data_size, data)
        },
        K_AUDIO_PLUG_IN_PROPERTY_BUNDLE_ID => unsafe {
            write_cfstring(PATCHBAY_DRIVER_BUNDLE_ID, data_size, out_data_size, data)
        },
        K_AUDIO_PLUG_IN_PROPERTY_RESOURCE_BUNDLE => unsafe {
            write_cfstring("", data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS => {
            let ids = if qualifier_allows_class(
                qualifier_data_size,
                qualifier_data,
                K_AUDIO_DEVICE_CLASS_ID,
            ) {
                let reg = PLUGIN.object_registry.lock();
                reg.all_device_ids()
            } else {
                Vec::new()
            };
            write_audio_object_ids(&ids, data_size, out_data_size, data)
        }
        K_AUDIO_PLUG_IN_PROPERTY_DEVICE_LIST => {
            let reg = PLUGIN.object_registry.lock();
            let ids = reg.all_device_ids();
            write_audio_object_ids(&ids, data_size, out_data_size, data)
        }
        K_AUDIO_OBJECT_PROPERTY_CUSTOM_PROPERTY_INFO_LIST => {
            let infos = [
                AudioServerPlugInCustomPropertyInfo {
                    m_selector: K_PB_PROPERTY_DESIRED_STATE,
                    m_property_data_type:
                        K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_CFPROPERTYLIST,
                    m_qualifier_data_type: K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_NONE,
                },
                AudioServerPlugInCustomPropertyInfo {
                    m_selector: K_PB_PROPERTY_APPLIED_STATE,
                    m_property_data_type:
                        K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_CFPROPERTYLIST,
                    m_qualifier_data_type: K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_NONE,
                },
                AudioServerPlugInCustomPropertyInfo {
                    m_selector: K_PB_PROPERTY_RUNTIME_STATS,
                    m_property_data_type:
                        K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_CFPROPERTYLIST,
                    m_qualifier_data_type: K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_NONE,
                },
                AudioServerPlugInCustomPropertyInfo {
                    m_selector: K_PB_PROPERTY_CONFIG_SUMMARY,
                    m_property_data_type:
                        K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_CFPROPERTYLIST,
                    m_qualifier_data_type: K_AUDIO_SERVER_PLUG_IN_CUSTOM_PROPERTY_DATA_TYPE_NONE,
                },
            ];
            let byte_len = size_of_val(&infos);
            if (data_size as usize) < byte_len {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            // SAFETY: buffer is large enough, infos is repr(C) with known layout.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    infos.as_ptr().cast::<u8>(),
                    data.cast::<u8>(),
                    byte_len,
                );
                *out_data_size = byte_len as UInt32;
            }
            K_AUDIO_HARDWARE_NO_ERROR
        }
        K_PB_PROPERTY_APPLIED_STATE => {
            let json = match applied_state_json() {
                Ok(j) => j,
                Err(_) => return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR,
            };
            // SAFETY: caller guarantees buffer is writable for data_size bytes.
            unsafe { write_json_as_cfdata(&json, data_size, out_data_size, data) }
        }
        K_PB_PROPERTY_RUNTIME_STATS => {
            let json = match runtime_stats_json() {
                Ok(j) => j,
                Err(_) => return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR,
            };
            unsafe { write_json_as_cfdata(&json, data_size, out_data_size, data) }
        }
        K_PB_PROPERTY_CONFIG_SUMMARY => {
            let json = match configuration_summary_json() {
                Ok(j) => j,
                Err(_) => return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR,
            };
            unsafe { write_json_as_cfdata(&json, data_size, out_data_size, data) }
        }
        K_PB_PROPERTY_DESIRED_STATE => {
            let state = DRIVER_STATE.lock();
            let json = match &state.desired_state {
                Some(ds) => match serde_json::to_string(ds) {
                    Ok(j) => j,
                    Err(_) => return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR,
                },
                None => "null".to_string(),
            };
            drop(state);
            unsafe { write_json_as_cfdata(&json, data_size, out_data_size, data) }
        }
        _ => K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR,
    }
}

// ===========================================================================
// Property data — Device object
// ===========================================================================

fn device_owned_object_ids(
    dev: &DeviceObjectInfo,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
) -> Vec<AudioObjectID> {
    let mut ids = Vec::with_capacity(3);
    if qualifier_allows_class(qualifier_data_size, qualifier_data, K_AUDIO_STREAM_CLASS_ID) {
        ids.push(dev.input_stream_id);
        ids.push(dev.output_stream_id);
    }
    if let Some(control_id) = dev.volume_control_id.filter(|_| {
        qualifier_allows_class(
            qualifier_data_size,
            qualifier_data,
            K_AUDIO_VOLUME_CONTROL_CLASS_ID,
        )
    }) {
        ids.push(control_id);
    }
    ids
}

fn volume_control_name(dev: &DeviceObjectInfo) -> String {
    format!("{} Volume", dev.name)
}

fn device_property_data_size(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
) -> Option<UInt32> {
    Some(match addr.m_selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS | K_AUDIO_OBJECT_PROPERTY_CLASS => {
            size_of::<UInt32>() as UInt32
        }
        K_AUDIO_OBJECT_PROPERTY_OWNER => size_of::<AudioObjectID>() as UInt32,
        K_AUDIO_OBJECT_PROPERTY_NAME
        | K_AUDIO_OBJECT_PROPERTY_MANUFACTURER
        | K_AUDIO_DEVICE_PROPERTY_DEVICE_UID
        | K_AUDIO_DEVICE_PROPERTY_MODEL_UID => {
            size_of::<*const c_void>() as UInt32 // CFStringRef
        }
        K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE
        | K_AUDIO_DEVICE_PROPERTY_DEVICE_CAN_BE_DEFAULT_DEVICE
        | K_AUDIO_DEVICE_PROPERTY_DEVICE_CAN_BE_DEFAULT_SYSTEM_DEVICE
        | K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_HIDDEN
        | K_AUDIO_DEVICE_PROPERTY_LATENCY
        | K_AUDIO_DEVICE_PROPERTY_SAFETY_OFFSET
        | K_AUDIO_DEVICE_PROPERTY_CLOCK_DOMAIN
        | K_AUDIO_DEVICE_PROPERTY_IS_ALIVE
        | K_AUDIO_DEVICE_PROPERTY_IS_RUNNING
        | K_AUDIO_DEVICE_PROPERTY_ZERO_TIME_STAMP_PERIOD => size_of::<UInt32>() as UInt32,
        K_AUDIO_DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE => size_of::<Float64>() as UInt32,
        K_AUDIO_DEVICE_PROPERTY_AVAILABLE_NOMINAL_SAMPLE_RATES
        | K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE_RANGE => size_of::<AudioValueRange>() as UInt32,
        K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE => size_of::<UInt32>() as UInt32,
        K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR
        | K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS
        | K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR_TO_DECIBELS
        | K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS_TO_SCALAR => {
            let reg = PLUGIN.object_registry.lock();
            let Some(dev) = reg.find_device_by_object(object_id) else {
                return Some(0);
            };
            if device_volume_address_matches(dev, addr) {
                size_of::<Float32>() as UInt32
            } else {
                0
            }
        }
        K_AUDIO_DEVICE_PROPERTY_VOLUME_RANGE_DECIBELS => {
            let reg = PLUGIN.object_registry.lock();
            let Some(dev) = reg.find_device_by_object(object_id) else {
                return Some(0);
            };
            if device_volume_address_matches(dev, addr) {
                size_of::<AudioValueRange>() as UInt32
            } else {
                0
            }
        }
        K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS => {
            let reg = PLUGIN.object_registry.lock();
            let Some(dev) = reg.find_device_by_object(object_id) else {
                return Some(0);
            };
            (device_owned_object_ids(dev, qualifier_data_size, qualifier_data).len()
                * size_of::<AudioObjectID>()) as UInt32
        }
        K_AUDIO_DEVICE_PROPERTY_STREAMS => {
            let reg = PLUGIN.object_registry.lock();
            if let Some(dev) = reg.find_device_by_object(object_id) {
                (device_streams_in_scope(dev, addr.m_scope).len() * size_of::<AudioObjectID>())
                    as UInt32
            } else {
                0
            }
        }
        K_AUDIO_OBJECT_PROPERTY_CONTROL_LIST => {
            let reg = PLUGIN.object_registry.lock();
            let Some(dev) = reg.find_device_by_object(object_id) else {
                return Some(0);
            };
            if dev.volume_control_id.is_some() {
                size_of::<AudioObjectID>() as UInt32
            } else {
                0
            }
        }
        K_AUDIO_DEVICE_PROPERTY_PREFERRED_CHANNELS_FOR_STEREO => {
            (2 * size_of::<UInt32>()) as UInt32
        }
        _ => return None,
    })
}

/// Write device property data.
///
/// # Safety
/// `data` must be writable for `data_size` bytes.
unsafe fn device_get_property(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    qualifier_data_size: UInt32,
    qualifier_data: *const c_void,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let reg = PLUGIN.object_registry.lock();
    let Some(dev) = reg.find_device_by_object(object_id) else {
        return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
    };
    let dev = dev.clone();
    drop(reg);

    let state = DRIVER_STATE.lock();
    let sample_rate = state.applied_state.sample_rate as f64;
    let buffer_frames = state.applied_state.buffer_frames;
    drop(state);

    match addr.m_selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS => unsafe {
            write_val::<UInt32>(K_AUDIO_OBJECT_CLASS_ID, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_CLASS => unsafe {
            write_val::<UInt32>(K_AUDIO_DEVICE_CLASS_ID, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_OWNER => unsafe {
            write_val::<AudioObjectID>(runtime_plugin_object_id(), data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_NAME => unsafe {
            write_cfstring(&dev.name, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_MANUFACTURER => unsafe {
            write_cfstring("FastTrackStudio", data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_DEVICE_UID => unsafe {
            write_cfstring(&dev.uid, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_MODEL_UID => unsafe {
            write_cfstring("PatchbayLoopback", data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE => unsafe {
            write_val::<UInt32>(
                K_AUDIO_TRANSPORT_TYPE_VIRTUAL,
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_DEVICE_PROPERTY_DEVICE_CAN_BE_DEFAULT_DEVICE => unsafe {
            write_val::<UInt32>(1, data_size, out_data_size, data)
        },
        // Never the alert-sound device: system beeps don't belong in a mix.
        K_AUDIO_DEVICE_PROPERTY_DEVICE_CAN_BE_DEFAULT_SYSTEM_DEVICE => unsafe {
            write_val::<UInt32>(0, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_HIDDEN => unsafe {
            write_val::<UInt32>(
                if dev.hidden { 1 } else { 0 },
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_DEVICE_PROPERTY_LATENCY | K_AUDIO_DEVICE_PROPERTY_SAFETY_OFFSET => unsafe {
            write_val::<UInt32>(0, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_CLOCK_DOMAIN => unsafe {
            write_val::<UInt32>(0, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_IS_ALIVE => unsafe {
            write_val::<UInt32>(1, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_IS_RUNNING => unsafe {
            write_val::<UInt32>(
                if dev.io_clients > 0 { 1 } else { 0 },
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_DEVICE_PROPERTY_ZERO_TIME_STAMP_PERIOD => unsafe {
            // Must match the period the zero timestamp advances by.
            write_val::<UInt32>(
                ZERO_TS_PERIOD_FRAMES as UInt32,
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE => unsafe {
            write_val::<UInt32>(buffer_frames, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_BUFFER_FRAME_SIZE_RANGE => {
            // Clients may use any IO size up to a quarter of the ring (the
            // ring holds 8x the nominal buffer), so HAL-side reads can never
            // outrun the producer at a supported size. Without this property
            // CoreAudio synthesizes a bogus tiny range and standard clients
            // (cpal, AVAudioEngine) fail to open the stream.
            let range = AudioValueRange {
                m_minimum: 16.0,
                m_maximum: f64::from(buffer_frames.saturating_mul(4)),
            };
            unsafe { write_val::<AudioValueRange>(range, data_size, out_data_size, data) }
        }
        K_AUDIO_DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE => unsafe {
            write_val::<Float64>(sample_rate, data_size, out_data_size, data)
        },
        K_AUDIO_DEVICE_PROPERTY_AVAILABLE_NOMINAL_SAMPLE_RATES => {
            let range = AudioValueRange {
                m_minimum: sample_rate,
                m_maximum: sample_rate,
            };
            unsafe { write_val::<AudioValueRange>(range, data_size, out_data_size, data) }
        }
        K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR => {
            if !device_volume_address_matches(&dev, addr) {
                return K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR;
            }
            unsafe { write_val::<Float32>(dev.volume_scalar(), data_size, out_data_size, data) }
        }
        K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS => {
            if !device_volume_address_matches(&dev, addr) {
                return K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR;
            }
            unsafe {
                write_val::<Float32>(
                    volume_scalar_to_decibels(dev.volume_scalar()),
                    data_size,
                    out_data_size,
                    data,
                )
            }
        }
        K_AUDIO_DEVICE_PROPERTY_VOLUME_RANGE_DECIBELS => {
            if !device_volume_address_matches(&dev, addr) {
                return K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR;
            }
            let range = AudioValueRange {
                m_minimum: VOLUME_MIN_DECIBELS as Float64,
                m_maximum: VOLUME_MAX_DECIBELS as Float64,
            };
            unsafe { write_val::<AudioValueRange>(range, data_size, out_data_size, data) }
        }
        K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR_TO_DECIBELS => {
            if !device_volume_address_matches(&dev, addr) {
                return K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR;
            }
            if (data_size as usize) < size_of::<Float32>() {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            // SAFETY: buffer size checked above.
            let input = unsafe { core::ptr::read_unaligned(data.cast::<Float32>()) };
            unsafe {
                write_val::<Float32>(
                    volume_scalar_to_decibels(clamp_volume_scalar(input)),
                    data_size,
                    out_data_size,
                    data,
                )
            }
        }
        K_AUDIO_DEVICE_PROPERTY_VOLUME_DECIBELS_TO_SCALAR => {
            if !device_volume_address_matches(&dev, addr) {
                return K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR;
            }
            if (data_size as usize) < size_of::<Float32>() {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            // SAFETY: buffer size checked above.
            let input = unsafe { core::ptr::read_unaligned(data.cast::<Float32>()) };
            unsafe {
                write_val::<Float32>(
                    volume_decibels_to_scalar(input),
                    data_size,
                    out_data_size,
                    data,
                )
            }
        }
        K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS => {
            let ids = device_owned_object_ids(&dev, qualifier_data_size, qualifier_data);
            write_audio_object_ids(&ids, data_size, out_data_size, data)
        }
        K_AUDIO_DEVICE_PROPERTY_STREAMS => {
            let ids = device_streams_in_scope(&dev, addr.m_scope);
            write_audio_object_ids(&ids, data_size, out_data_size, data)
        }
        K_AUDIO_OBJECT_PROPERTY_CONTROL_LIST => {
            let ids = dev.volume_control_id.into_iter().collect::<Vec<_>>();
            write_audio_object_ids(&ids, data_size, out_data_size, data)
        }
        K_AUDIO_DEVICE_PROPERTY_PREFERRED_CHANNELS_FOR_STEREO => {
            let channels: [UInt32; 2] = [1, 2];
            let byte_len = size_of_val(&channels);
            if (data_size as usize) < byte_len {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            // SAFETY: buffer large enough, writing two UInt32s.
            unsafe {
                core::ptr::copy_nonoverlapping(channels.as_ptr(), data.cast::<UInt32>(), 2);
                *out_data_size = byte_len as UInt32;
            }
            K_AUDIO_HARDWARE_NO_ERROR
        }
        _ => K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR,
    }
}

fn control_property_data_size(selector: UInt32) -> Option<UInt32> {
    Some(match selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS
        | K_AUDIO_OBJECT_PROPERTY_CLASS
        | K_AUDIO_OBJECT_PROPERTY_OWNER
        | K_AUDIO_CONTROL_PROPERTY_SCOPE
        | K_AUDIO_CONTROL_PROPERTY_ELEMENT => size_of::<UInt32>() as UInt32,
        K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS => 0,
        K_AUDIO_OBJECT_PROPERTY_NAME | K_AUDIO_OBJECT_PROPERTY_MANUFACTURER => {
            size_of::<*const c_void>() as UInt32
        }
        K_AUDIO_LEVEL_CONTROL_PROPERTY_SCALAR_VALUE
        | K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_VALUE
        | K_AUDIO_LEVEL_CONTROL_PROPERTY_CONVERT_SCALAR_TO_DECIBELS
        | K_AUDIO_LEVEL_CONTROL_PROPERTY_CONVERT_DECIBELS_TO_SCALAR => {
            size_of::<Float32>() as UInt32
        }
        K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_RANGE => size_of::<AudioValueRange>() as UInt32,
        _ => return None,
    })
}

unsafe fn control_get_property(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let reg = PLUGIN.object_registry.lock();
    let Some(dev) = reg.find_device_by_control(object_id) else {
        return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
    };
    let dev = dev.clone();
    drop(reg);

    match addr.m_selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS => unsafe {
            write_val::<UInt32>(
                K_AUDIO_LEVEL_CONTROL_CLASS_ID,
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_OBJECT_PROPERTY_CLASS => unsafe {
            write_val::<UInt32>(
                K_AUDIO_VOLUME_CONTROL_CLASS_ID,
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_OBJECT_PROPERTY_OWNER => unsafe {
            write_val::<AudioObjectID>(dev.device_id, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_OWNED_OBJECTS => {
            write_audio_object_ids(&[], data_size, out_data_size, data)
        }
        K_AUDIO_OBJECT_PROPERTY_NAME => unsafe {
            write_cfstring(&volume_control_name(&dev), data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_MANUFACTURER => unsafe {
            write_cfstring("FastTrackStudio", data_size, out_data_size, data)
        },
        K_AUDIO_CONTROL_PROPERTY_SCOPE => unsafe {
            write_val::<UInt32>(
                K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_CONTROL_PROPERTY_ELEMENT => unsafe {
            write_val::<UInt32>(
                K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_LEVEL_CONTROL_PROPERTY_SCALAR_VALUE => unsafe {
            write_val::<Float32>(dev.volume_scalar(), data_size, out_data_size, data)
        },
        K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_VALUE => unsafe {
            write_val::<Float32>(
                volume_scalar_to_decibels(dev.volume_scalar()),
                data_size,
                out_data_size,
                data,
            )
        },
        K_AUDIO_LEVEL_CONTROL_PROPERTY_DECIBEL_RANGE => {
            let range = AudioValueRange {
                m_minimum: VOLUME_MIN_DECIBELS as Float64,
                m_maximum: VOLUME_MAX_DECIBELS as Float64,
            };
            unsafe { write_val::<AudioValueRange>(range, data_size, out_data_size, data) }
        }
        K_AUDIO_LEVEL_CONTROL_PROPERTY_CONVERT_SCALAR_TO_DECIBELS => {
            if (data_size as usize) < size_of::<Float32>() {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            // SAFETY: buffer size checked above.
            let input = unsafe { core::ptr::read_unaligned(data.cast::<Float32>()) };
            unsafe {
                write_val::<Float32>(
                    volume_scalar_to_decibels(clamp_volume_scalar(input)),
                    data_size,
                    out_data_size,
                    data,
                )
            }
        }
        K_AUDIO_LEVEL_CONTROL_PROPERTY_CONVERT_DECIBELS_TO_SCALAR => {
            if (data_size as usize) < size_of::<Float32>() {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            // SAFETY: buffer size checked above.
            let input = unsafe { core::ptr::read_unaligned(data.cast::<Float32>()) };
            unsafe {
                write_val::<Float32>(
                    volume_decibels_to_scalar(input),
                    data_size,
                    out_data_size,
                    data,
                )
            }
        }
        _ => K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR,
    }
}

// ===========================================================================
// Property data — Stream object
// ===========================================================================

fn stream_property_data_size(selector: UInt32) -> Option<UInt32> {
    Some(match selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS | K_AUDIO_OBJECT_PROPERTY_CLASS => {
            size_of::<UInt32>() as UInt32
        }
        K_AUDIO_OBJECT_PROPERTY_OWNER => size_of::<AudioObjectID>() as UInt32,
        K_AUDIO_STREAM_PROPERTY_DIRECTION
        | K_AUDIO_STREAM_PROPERTY_TERMINAL_TYPE
        | K_AUDIO_STREAM_PROPERTY_START_CHANNEL
        | K_AUDIO_STREAM_PROPERTY_LATENCY
        | K_AUDIO_STREAM_PROPERTY_IS_ACTIVE => size_of::<UInt32>() as UInt32,
        K_AUDIO_STREAM_PROPERTY_VIRTUAL_FORMAT | K_AUDIO_STREAM_PROPERTY_PHYSICAL_FORMAT => {
            size_of::<AudioStreamBasicDescription>() as UInt32
        }
        K_AUDIO_STREAM_PROPERTY_AVAILABLE_VIRTUAL_FORMATS
        | K_AUDIO_STREAM_PROPERTY_AVAILABLE_PHYSICAL_FORMATS => {
            size_of::<AudioStreamRangedDescription>() as UInt32
        }
        _ => return None,
    })
}

/// Write stream property data.
///
/// # Safety
/// `data` must be writable for `data_size` bytes.
unsafe fn stream_get_property(
    object_id: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let reg = PLUGIN.object_registry.lock();
    let Some(dev) = reg.find_device_by_stream(object_id) else {
        return K_AUDIO_HARDWARE_BAD_OBJECT_ERROR;
    };
    let dev = dev.clone();
    drop(reg);

    let state = DRIVER_STATE.lock();
    let sample_rate = state.applied_state.sample_rate as f64;
    drop(state);

    let channels = dev.channels as u32;
    let is_input = dev.is_input_stream(object_id);

    match addr.m_selector {
        K_AUDIO_OBJECT_PROPERTY_BASE_CLASS => unsafe {
            write_val::<UInt32>(K_AUDIO_OBJECT_CLASS_ID, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_CLASS => unsafe {
            write_val::<UInt32>(K_AUDIO_STREAM_CLASS_ID, data_size, out_data_size, data)
        },
        K_AUDIO_OBJECT_PROPERTY_OWNER => unsafe {
            write_val::<AudioObjectID>(dev.device_id, data_size, out_data_size, data)
        },
        K_AUDIO_STREAM_PROPERTY_DIRECTION => {
            let dir: UInt32 = if is_input { 1 } else { 0 };
            unsafe { write_val::<UInt32>(dir, data_size, out_data_size, data) }
        }
        K_AUDIO_STREAM_PROPERTY_TERMINAL_TYPE => {
            let term = if is_input {
                K_INPUT_TERMINAL
            } else {
                K_OUTPUT_TERMINAL
            };
            unsafe { write_val::<UInt32>(term, data_size, out_data_size, data) }
        }
        K_AUDIO_STREAM_PROPERTY_START_CHANNEL => unsafe {
            write_val::<UInt32>(1, data_size, out_data_size, data)
        },
        K_AUDIO_STREAM_PROPERTY_LATENCY => unsafe {
            write_val::<UInt32>(0, data_size, out_data_size, data)
        },
        K_AUDIO_STREAM_PROPERTY_IS_ACTIVE => unsafe {
            write_val::<UInt32>(1, data_size, out_data_size, data)
        },
        K_AUDIO_STREAM_PROPERTY_VIRTUAL_FORMAT | K_AUDIO_STREAM_PROPERTY_PHYSICAL_FORMAT => {
            let asbd = AudioStreamBasicDescription::float32_stereo(sample_rate, channels);
            unsafe {
                write_val::<AudioStreamBasicDescription>(asbd, data_size, out_data_size, data)
            }
        }
        K_AUDIO_STREAM_PROPERTY_AVAILABLE_VIRTUAL_FORMATS
        | K_AUDIO_STREAM_PROPERTY_AVAILABLE_PHYSICAL_FORMATS => {
            let asbd = AudioStreamBasicDescription::float32_stereo(sample_rate, channels);
            let ranged = AudioStreamRangedDescription {
                m_format: asbd,
                m_sample_rate_range: AudioValueRange {
                    m_minimum: sample_rate,
                    m_maximum: sample_rate,
                },
            };
            unsafe {
                write_val::<AudioStreamRangedDescription>(ranged, data_size, out_data_size, data)
            }
        }
        _ => K_AUDIO_HARDWARE_UNKNOWN_PROPERTY_ERROR,
    }
}

// ===========================================================================
// SetPropertyData — desired state
// ===========================================================================

/// Parse a CFDataRef from the host buffer and stage the desired state.
///
/// The host passes the property value as a pointer to a CFDataRef (the custom
/// property data type is `kAudioServerPlugInCustomPropertyDataTypeCFData`).
///
/// # Safety
/// `data` must point to at least `data_size` readable bytes containing a CFDataRef.
unsafe fn set_desired_state_from_raw(data: *const c_void, data_size: UInt32) -> OSStatus {
    // `data` points to a CFDataRef value (a pointer).
    if (data_size as usize) < size_of::<*const c_void>() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    // SAFETY: `data` is non-null (checked by caller) and large enough for a pointer.
    let cf_data: *const c_void = unsafe { core::ptr::read_unaligned(data.cast::<*const c_void>()) };
    if cf_data.is_null() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    // SAFETY: `cf_data` is a valid CFDataRef provided by the host via SetPropertyData.
    let byte_ptr = unsafe { CFDataGetBytePtr(cf_data) };
    let byte_len = unsafe { CFDataGetLength(cf_data) };
    if byte_ptr.is_null() || byte_len <= 0 {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    // SAFETY: CFDataGetBytePtr returns a pointer to `byte_len` contiguous bytes.
    let bytes = unsafe { core::slice::from_raw_parts(byte_ptr, byte_len as usize) };
    let json_str = match core::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR,
    };

    apply_desired_json(json_str)
}

/// Stage `json` as the desired state and apply it: immediately when no
/// device exists yet (startup), otherwise through the host's
/// device-configuration-change handshake.
fn apply_desired_json(json_str: &str) -> OSStatus {
    if set_desired_state_json(json_str).is_err() {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    let generation = match request_device_configuration_change() {
        Ok(g) => g,
        Err(_) => return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR,
    };
    if crate::pending_change().is_none() {
        // Already converged.
        return K_AUDIO_HARDWARE_NO_ERROR;
    }
    let change_target_device_id = first_registered_device_id();
    let host = *PLUGIN.host.lock();
    match (host, change_target_device_id) {
        (Some(host), Some(device_id)) => {
            // SAFETY: `host` is the AudioServerPlugInHostRef provided by coreaudiod.
            unsafe {
                ((*host).request_device_configuration_change)(
                    host,
                    device_id,
                    generation,
                    core::ptr::null(),
                )
            }
        }
        _ => {
            if perform_device_configuration_change(generation).is_err() {
                return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
            }
            sync_object_registry();
            persist_applied_state();
            notify_device_list_changed();
            K_AUDIO_HARDWARE_NO_ERROR
        }
    }
}

// ===========================================================================
// Persistence (coreaudiod per-plug-in storage)
// ===========================================================================

/// Save the applied state so it's restored at the next `Initialize`.
fn persist_applied_state() {
    let Some(host) = *PLUGIN.host.lock() else {
        return;
    };
    let Ok(json) = serde_json::to_string(&applied_as_desired()) else {
        return;
    };
    let key = cfstring_create(STORAGE_KEY_APPLIED_STATE);
    let data = cfdata_create(json.as_bytes());
    // SAFETY: `host` is valid; `key` / `data` are CF objects we own and
    // release after the call (the host copies what it keeps).
    unsafe {
        let _ = ((*host).write_to_storage)(host, key, data);
        CFRelease(data);
        CFRelease(key);
    }
}

/// The applied state saved by [`persist_applied_state`], if any.
fn load_persisted_state() -> Option<String> {
    let host = (*PLUGIN.host.lock())?;
    let key = cfstring_create(STORAGE_KEY_APPLIED_STATE);
    let mut out: CFPropertyListRef = core::ptr::null();
    // SAFETY: `host` is valid, `key` is a CFString we own, `out` receives a
    // +1 property list (or stays null).
    let status = unsafe { ((*host).copy_from_storage)(host, key, &mut out) };
    // SAFETY: we own `key`.
    unsafe { CFRelease(key) };
    if status != K_AUDIO_HARDWARE_NO_ERROR || out.is_null() {
        return None;
    }
    // SAFETY: we stored a CFData; `out` is +1 and released below.
    let json = unsafe {
        let ptr = CFDataGetBytePtr(out);
        let len = CFDataGetLength(out);
        let text = if ptr.is_null() || len <= 0 {
            None
        } else {
            std::str::from_utf8(core::slice::from_raw_parts(ptr, len as usize))
                .ok()
                .map(str::to_owned)
        };
        CFRelease(out);
        text
    };
    json.filter(|j| serde_json::from_str::<crate::DesiredState>(j).is_ok())
}

// ===========================================================================
// Object registry sync
// ===========================================================================

fn sync_object_registry() {
    let state = DRIVER_STATE.lock();
    let applied = &state.applied_state;
    let desired_uids: std::collections::BTreeSet<&str> =
        applied.devices.iter().map(|d| d.uid.as_str()).collect();

    let mut reg = PLUGIN.object_registry.lock();
    let mut renamed_device_ids = Vec::new();

    // Remove devices no longer in applied state.
    reg.devices
        .retain(|uid, _| desired_uids.contains(uid.as_str()));

    // Add new devices and refresh metadata for existing ones.
    for device in &applied.devices {
        let reshape = reg
            .devices
            .get(&device.uid)
            .is_some_and(|existing| existing.channels != device.channels);
        if reshape {
            // A new channel count needs new streams and a new ring: drop
            // and recreate (the host sees a remove + add).
            reg.devices.remove(&device.uid);
        }
        if !reg.devices.contains_key(&device.uid) {
            let device_id = reg.allocate_id();
            let input_stream_id = reg.allocate_id();
            let output_stream_id = reg.allocate_id();
            let volume_control_id = Some(reg.allocate_id());
            reg.devices.insert(
                device.uid.clone(),
                DeviceObjectInfo::new(
                    device_id,
                    input_stream_id,
                    output_stream_id,
                    volume_control_id,
                    device.uid.clone(),
                    device.name.clone(),
                    device.kind.clone(),
                    device.channels,
                    device.hidden,
                ),
            );
        } else if let Some(existing) = reg.devices.get_mut(&device.uid) {
            if existing.name != device.name || existing.hidden != device.hidden {
                renamed_device_ids.push(existing.device_id);
            }
            existing.name = device.name.clone();
            existing.hidden = device.hidden;
        }
    }

    publish_rt_snapshot(&reg);
    drop(reg);
    drop(state);

    for device_id in renamed_device_ids {
        notify_device_renamed(device_id);
    }
}

/// Tell the host a device's name / visibility changed (no new objects).
fn notify_device_renamed(device_id: AudioObjectID) {
    let addresses = [
        AudioObjectPropertyAddress {
            m_selector: K_AUDIO_OBJECT_PROPERTY_NAME,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        },
        AudioObjectPropertyAddress {
            m_selector: K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_HIDDEN,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        },
    ];
    notify_properties_changed_for_object(device_id, &addresses);
}

fn first_registered_device_id() -> Option<AudioObjectID> {
    let reg = PLUGIN.object_registry.lock();
    reg.devices.values().map(|device| device.device_id).next()
}

fn notify_device_list_changed() {
    let host_guard = PLUGIN.host.lock();
    if let Some(host) = *host_guard {
        let plugin_object_id = runtime_plugin_object_id();
        let addr = AudioObjectPropertyAddress {
            m_selector: K_AUDIO_PLUG_IN_PROPERTY_DEVICE_LIST,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        };
        // SAFETY: `host` is valid AudioServerPlugInHostRef from coreaudiod.
        unsafe {
            ((*host).properties_changed)(host, plugin_object_id, 1, &addr);
        }
    }
}

// ===========================================================================
// Helpers — write typed values into host buffers
// ===========================================================================

/// Write a `Copy` value into the host buffer.
///
/// # Safety
/// `data` must be writable for at least `data_size` bytes.
/// `out_data_size` must be a valid pointer.
unsafe fn write_val<T: Copy>(
    val: T,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let needed = size_of::<T>();
    if (data_size as usize) < needed {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }
    // SAFETY: buffer bounds checked.
    unsafe {
        core::ptr::write_unaligned(data.cast::<T>(), val);
        *out_data_size = needed as UInt32;
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

/// Write a Rust `&str` as a CFStringRef into the host buffer.
///
/// # Safety
/// `data` must be writable for at least `data_size` bytes (enough for a pointer).
/// `out_data_size` must be a valid pointer.
unsafe fn write_cfstring(
    s: &str,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let needed = size_of::<*const c_void>();
    if (data_size as usize) < needed {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    let cf_str = cfstring_create(s);
    // SAFETY: buffer bounds checked; writing a pointer value.
    unsafe {
        core::ptr::write_unaligned(data.cast::<*const c_void>(), cf_str);
        *out_data_size = needed as UInt32;
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

/// Write JSON bytes as a CFDataRef into the host buffer.
///
/// # Safety
/// Same as `write_cfstring`.
unsafe fn write_json_as_cfdata(
    json: &str,
    data_size: UInt32,
    out_data_size: *mut UInt32,
    data: *mut c_void,
) -> OSStatus {
    let needed = size_of::<*const c_void>();
    if (data_size as usize) < needed {
        return K_AUDIO_HARDWARE_ILLEGAL_OPERATION_ERROR;
    }

    let cf_data = cfdata_create(json.as_bytes());
    // SAFETY: buffer bounds checked.
    unsafe {
        core::ptr::write_unaligned(data.cast::<*const c_void>(), cf_data);
        *out_data_size = needed as UInt32;
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

// ===========================================================================
// CoreFoundation helpers (linked via build.rs)
// ===========================================================================

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

    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

fn cfstring_create(s: &str) -> *const c_void {
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

fn cfdata_create(bytes: &[u8]) -> *const c_void {
    // SAFETY: FFI call to CoreFoundation with valid byte slice.
    unsafe { CFDataCreate(core::ptr::null(), bytes.as_ptr(), bytes.len() as isize) }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::LoopbackRing;

    #[test]
    fn do_io_round_trips_through_the_plugin_entry_points() {
        let _guard = crate::TEST_LOCK.lock();
        use super::*;
        // No host: applies synchronously.
        let json = serde_json::to_string(&crate::default_desired_state()).expect("json");
        assert_eq!(apply_desired_json(&json), K_AUDIO_HARDWARE_NO_ERROR);
        let (device_id, channels) = {
            let reg = PLUGIN.object_registry.lock();
            let d = reg.devices.get("Broadcast_UID").expect("broadcast device");
            (d.device_id, usize::from(d.channels))
        };
        // SAFETY: test drives the plug-in entry points with valid pointers.
        unsafe {
            assert_eq!(plugin_start_io(core::ptr::null_mut(), device_id, 1), 0);
            let frames = 512_u32;
            let mut cycle: AudioServerPlugInIOCycleInfo = core::mem::zeroed();
            cycle.m_output_time.m_sample_time = 48_000.0;
            let mut out: Vec<f32> = (0..frames as usize * channels)
                .map(|i| (i % 7) as f32)
                .collect();
            let expect = out.clone();
            assert_eq!(
                plugin_do_io_operation(
                    core::ptr::null_mut(),
                    device_id,
                    0,
                    1,
                    K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_WRITE_MIX,
                    frames,
                    &cycle,
                    out.as_mut_ptr().cast(),
                    core::ptr::null_mut()
                ),
                0
            );
            // A reader one buffer behind the writer (as the HAL schedules).
            cycle.m_input_time.m_sample_time = 48_000.0;
            let mut inp = vec![0.0_f32; frames as usize * channels];
            assert_eq!(
                plugin_do_io_operation(
                    core::ptr::null_mut(),
                    device_id,
                    0,
                    1,
                    K_AUDIO_SERVER_PLUG_IN_IO_OPERATION_READ_INPUT,
                    frames,
                    &cycle,
                    inp.as_mut_ptr().cast(),
                    core::ptr::null_mut()
                ),
                0
            );
            assert_eq!(inp, expect, "ReadInput must return what WriteMix wrote");
        }
    }

    #[test]
    fn clock_is_not_re_anchored_while_clients_run() {
        let _guard = crate::TEST_LOCK.lock();
        use super::*;
        let json = serde_json::to_string(&crate::default_desired_state()).expect("json");
        assert_eq!(apply_desired_json(&json), K_AUDIO_HARDWARE_NO_ERROR);
        let (device_id, rt) = {
            let reg = PLUGIN.object_registry.lock();
            let d = reg.devices.get("Patchbay_UID").expect("patchbay device");
            (d.device_id, d.rt.clone())
        };
        // SAFETY: test drives the plug-in entry points directly.
        unsafe {
            plugin_start_io(core::ptr::null_mut(), device_id, 1);
            let anchor = rt.anchor_host_time.load(Ordering::Relaxed);
            let seed = rt.zero_ts_seed.load(Ordering::Relaxed);
            plugin_start_io(core::ptr::null_mut(), device_id, 2);
            plugin_stop_io(core::ptr::null_mut(), device_id, 1);
            plugin_start_io(core::ptr::null_mut(), device_id, 3);
            assert_eq!(rt.anchor_host_time.load(Ordering::Relaxed), anchor);
            assert_eq!(rt.zero_ts_seed.load(Ordering::Relaxed), seed);
            plugin_stop_io(core::ptr::null_mut(), device_id, 2);
            plugin_stop_io(core::ptr::null_mut(), device_id, 3);
            // All stopped: the next start re-arms (new seed).
            plugin_start_io(core::ptr::null_mut(), device_id, 4);
            assert_ne!(rt.zero_ts_seed.load(Ordering::Relaxed), seed);
            plugin_stop_io(core::ptr::null_mut(), device_id, 4);
        }
    }

    #[test]
    fn loopback_returns_what_was_written_and_silence_when_idle() {
        let ring = LoopbackRing::new(2);
        let written: Vec<f32> = (0..512).map(|i| i as f32).collect();
        let mut read = vec![0.0_f32; 512];
        // SAFETY: single-threaded test stands in for the IO thread.
        unsafe {
            ring.write(1000.0, &written, 256);
            ring.read(1000.0, &mut read, 256, 1.0, false);
        }
        assert_eq!(read, written);
        // Reading far past the last write: silence, ring cleared.
        let mut later = vec![1.0_f32; 512];
        // SAFETY: as above.
        unsafe { ring.read(100_000.0, &mut later, 256, 1.0, false) };
        assert!(later.iter().all(|s| *s == 0.0));
        // Wrap-around across the ring end.
        let t = (super::RING_FRAMES - 100) as f64;
        // SAFETY: as above.
        unsafe {
            ring.write(t, &written, 256);
            ring.read(t, &mut read, 256, 0.5, false);
        }
        assert!(
            read.iter()
                .zip(&written)
                .all(|(r, w)| (*r - *w * 0.5).abs() < 1e-6)
        );
    }
}
