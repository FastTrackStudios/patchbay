//! Raw HAL facts as plain Rust data (no graph semantics — that is
//! `enumerate.rs`). Everything here is safe code over [`super::property`].

use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceIsRunningSomewhere,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeAggregate, kAudioDeviceTransportTypeVirtual,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioHardwarePropertyProcessObjectList,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyManufacturer,
    kAudioObjectPropertyName, kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunning,
    kAudioProcessPropertyIsRunningInput, kAudioProcessPropertyIsRunningOutput,
    kAudioProcessPropertyPID,
};
use patchbay_host::HostError;

use super::property::{self, Scope};
use super::{ObjectId, SYSTEM_OBJECT};

/// Selectors the backend listens to. Re-exported here so no other module
/// names `objc2_core_audio` items.
pub(crate) mod selector {
    pub(crate) use objc2_core_audio::{
        kAudioDevicePropertyDeviceIsAlive as DEVICE_IS_ALIVE,
        kAudioDevicePropertyNominalSampleRate as NOMINAL_SAMPLE_RATE,
        kAudioDevicePropertyStreamConfiguration as STREAM_CONFIGURATION,
        kAudioHardwarePropertyDefaultInputDevice as DEFAULT_INPUT,
        kAudioHardwarePropertyDefaultOutputDevice as DEFAULT_OUTPUT,
        kAudioHardwarePropertyDevices as DEVICES,
        kAudioHardwarePropertyProcessObjectList as PROCESSES,
        kAudioProcessPropertyIsRunning as PROCESS_IS_RUNNING,
        kAudioProcessPropertyIsRunningOutput as PROCESS_IS_RUNNING_OUTPUT,
    };
}

/// Coarse device class from `kAudioDevicePropertyTransportType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    Aggregate,
    Virtual,
    Other,
}

/// One audio device.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeviceFacts {
    pub object: ObjectId,
    pub uid: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub transport: Transport,
    /// Four-char transport code (`bltn`, `usb `, `virt`, `grup`, …).
    pub transport_code: String,
    pub sample_rate: Option<f64>,
    /// Capture channels (device perspective), summed over streams.
    pub input_channels: u32,
    /// Playback channels (device perspective), summed over streams.
    pub output_channels: u32,
    /// Buffers (streams) on the input side.
    pub input_streams: usize,
    pub alive: bool,
    pub running_somewhere: bool,
}

/// One HAL process object (an audio client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessFacts {
    pub object: ObjectId,
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: String,
    pub running: bool,
    pub running_output: bool,
    pub running_input: bool,
}

/// All device object ids.
pub(crate) fn device_ids() -> Result<Vec<ObjectId>, HostError> {
    property::get_object_list(SYSTEM_OBJECT, kAudioHardwarePropertyDevices, Scope::Global)
}

/// All process object ids (macOS 14.2+).
pub(crate) fn process_ids() -> Result<Vec<ObjectId>, HostError> {
    property::get_object_list(
        SYSTEM_OBJECT,
        kAudioHardwarePropertyProcessObjectList,
        Scope::Global,
    )
}

/// Process object for `pid`, `None` if the process isn't an audio client.
pub(crate) fn process_for_pid(pid: i32) -> Result<Option<ObjectId>, HostError> {
    let object: u32 = property::get(
        SYSTEM_OBJECT,
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        Scope::Global,
        Some(&pid),
    )?;
    Ok((object != 0).then_some(object))
}

/// Default output (`true`) or input device object.
pub(crate) fn default_device(output: bool) -> Result<Option<ObjectId>, HostError> {
    let selector = if output {
        kAudioHardwarePropertyDefaultOutputDevice
    } else {
        kAudioHardwarePropertyDefaultInputDevice
    };
    let object: u32 = property::get_plain(SYSTEM_OBJECT, selector, Scope::Global)?;
    Ok((object != 0).then_some(object))
}

fn flag(object: ObjectId, selector: u32) -> bool {
    property::get_plain::<u32>(object, selector, Scope::Global).is_ok_and(|v| v != 0)
}

fn channels(object: ObjectId, scope: Scope) -> (u32, usize) {
    property::stream_channels(object, kAudioDevicePropertyStreamConfiguration, scope)
        .map(|per_buffer| {
            (
                per_buffer
                    .iter()
                    .fold(0_u32, |acc, c| acc.saturating_add(*c)),
                per_buffer.len(),
            )
        })
        .unwrap_or((0, 0))
}

/// Per-buffer channel counts of `object` on the input (`true`) or output
/// side — the layout its `IOProc` will see.
pub(crate) fn stream_layout(object: ObjectId, input: bool) -> Vec<u32> {
    let scope = if input { Scope::Input } else { Scope::Output };
    property::stream_channels(object, kAudioDevicePropertyStreamConfiguration, scope)
        .unwrap_or_default()
}

/// Facts about one device. Only the UID is mandatory.
pub(crate) fn device(object: ObjectId) -> Result<DeviceFacts, HostError> {
    let uid = property::get_string(object, kAudioDevicePropertyDeviceUID, Scope::Global)?;
    let name = property::get_string(object, kAudioObjectPropertyName, Scope::Global)
        .unwrap_or_else(|_| uid.clone());
    let manufacturer =
        property::get_string(object, kAudioObjectPropertyManufacturer, Scope::Global)
            .ok()
            .filter(|m| !m.is_empty());
    let code: u32 =
        property::get_plain(object, kAudioDevicePropertyTransportType, Scope::Global).unwrap_or(0);
    let transport = if code == kAudioDeviceTransportTypeAggregate {
        Transport::Aggregate
    } else if code == kAudioDeviceTransportTypeVirtual {
        Transport::Virtual
    } else {
        Transport::Other
    };
    let sample_rate =
        property::get_plain::<f64>(object, kAudioDevicePropertyNominalSampleRate, Scope::Global)
            .ok();
    let (input_channels, input_streams) = channels(object, Scope::Input);
    let (output_channels, _) = channels(object, Scope::Output);
    Ok(DeviceFacts {
        object,
        uid,
        name,
        manufacturer,
        transport,
        transport_code: super::fourcc(code),
        sample_rate,
        input_channels,
        output_channels,
        input_streams,
        alive: flag(object, kAudioDevicePropertyDeviceIsAlive),
        running_somewhere: flag(object, kAudioDevicePropertyDeviceIsRunningSomewhere),
    })
}

/// Facts about one process object.
pub(crate) fn process(object: ObjectId) -> Result<ProcessFacts, HostError> {
    let pid: i32 = property::get_plain(object, kAudioProcessPropertyPID, Scope::Global)?;
    let bundle_id = property::get_string(object, kAudioProcessPropertyBundleID, Scope::Global)
        .ok()
        .filter(|b| !b.is_empty());
    let name = super::sys::process_name(pid)
        .or_else(|| bundle_id.clone())
        .unwrap_or_else(|| format!("pid {pid}"));
    Ok(ProcessFacts {
        object,
        pid,
        bundle_id,
        name,
        running: flag(object, kAudioProcessPropertyIsRunning),
        running_output: flag(object, kAudioProcessPropertyIsRunningOutput),
        running_input: flag(object, kAudioProcessPropertyIsRunningInput),
    })
}

/// Device object with `uid`, if present.
pub(crate) fn device_by_uid(uid: &str) -> Result<Option<DeviceFacts>, HostError> {
    Ok(device_ids()?
        .into_iter()
        .filter_map(|id| device(id).ok())
        .find(|d| d.uid == uid))
}

/// Whether a (listener) property exists on `object`.
pub(crate) fn has_property(object: ObjectId, selector: u32) -> bool {
    property::has(object, selector, Scope::Global)
}

/// Tap / aggregate stream sample format summary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Format {
    pub sample_rate: f64,
    pub channels: u32,
    pub float32: bool,
    pub interleaved: bool,
}

/// Summarise an `AudioStreamBasicDescription` (Linear PCM float flags).
pub(crate) const fn summarize(
    asbd: &objc2_core_audio_types::AudioStreamBasicDescription,
) -> Format {
    const LPCM: u32 = 0x6c70_636d; // 'lpcm'
    const IS_FLOAT: u32 = 1; // kAudioFormatFlagIsFloat
    const NON_INTERLEAVED: u32 = 1 << 5; // kAudioFormatFlagIsNonInterleaved
    Format {
        sample_rate: asbd.mSampleRate,
        channels: asbd.mChannelsPerFrame,
        float32: asbd.mFormatID == LPCM
            && asbd.mFormatFlags & IS_FLOAT != 0
            && asbd.mBitsPerChannel == 32,
        interleaved: asbd.mFormatFlags & NON_INTERLEAVED == 0,
    }
}
