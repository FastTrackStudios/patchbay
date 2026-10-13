//! Process taps (macOS 14.2+) and private aggregate devices.

use std::ptr::NonNull;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, CATapDescription,
    CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};
use patchbay_host::HostError;

use super::{ObjectId, check, property};

/// What the tapped process hears while tapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mute {
    /// Process keeps playing to its device (patchbay's default).
    Unmuted,
    /// Process is silenced on its device while the tap exists.
    Muted,
    /// Silenced only while a client is actually reading the tap.
    MutedWhenTapped,
}

/// A process tap; destroyed on drop.
pub(crate) struct ProcessTap {
    id: ObjectId,
    uid: String,
}

impl Mute {
    /// The tap behaviour a [`TapMute`](crate::config::TapMute) asks for.
    pub(crate) const fn for_tap(m: crate::config::TapMute) -> Self {
        match m {
            crate::config::TapMute::Unmuted => Self::Unmuted,
            crate::config::TapMute::Muted => Self::Muted,
            crate::config::TapMute::MutedWhenTapped => Self::MutedWhenTapped,
        }
    }
}

impl ProcessTap {
    /// Create a **private**, stereo-mixdown tap of `processes` (HAL
    /// process object ids).
    pub(crate) fn create(
        processes: &[ObjectId],
        mute: Mute,
        name: &str,
    ) -> Result<Self, HostError> {
        let numbers: Vec<Retained<NSNumber>> =
            processes.iter().map(|p| NSNumber::new_u32(*p)).collect();
        let array = NSArray::from_retained_slice(&numbers);
        // SAFETY: `alloc` + designated initializer, as documented for
        // `CATapDescription`; `array` holds `NSNumber`s of process object
        // ids, which is the documented element type.
        let desc = unsafe {
            CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &array)
        };
        Self::create_from(&desc, mute, name)
    }

    /// Create a **private**, non-mixdown tap of what `processes` send to
    /// output stream `stream` of device `device_uid` — every channel of
    /// that stream, in the stream's own format (so a DAW's outputs 33–34
    /// on a 64-channel interface stay channels 33–34).
    pub(crate) fn create_device_stream(
        processes: &[ObjectId],
        device_uid: &str,
        stream: usize,
        mute: Mute,
        name: &str,
    ) -> Result<Self, HostError> {
        let numbers: Vec<Retained<NSNumber>> =
            processes.iter().map(|p| NSNumber::new_u32(*p)).collect();
        let array = NSArray::from_retained_slice(&numbers);
        let stream = objc2_foundation::NSInteger::try_from(stream)
            .map_err(|_| HostError::InvalidSpec(format!("stream index {stream} out of range")))?;
        // SAFETY: `alloc` + designated initializer; `array` holds
        // `NSNumber`s of process object ids to include, `device_uid` a
        // device UID string and `stream` an output stream index — the
        // documented argument types.
        let desc = unsafe {
            CATapDescription::initWithProcesses_andDeviceUID_withStream(
                CATapDescription::alloc(),
                &array,
                &NSString::from_str(device_uid),
                stream,
            )
        };
        Self::create_from(&desc, mute, name)
    }

    /// Create a **private**, stereo-mixdown *global* tap of every process
    /// except `excluded` (HAL process object ids). Used by the permission
    /// probe: creating and starting it is what makes macOS show the
    /// "System Audio Recording" prompt.
    pub(crate) fn create_global(
        excluded: &[ObjectId],
        mute: Mute,
        name: &str,
    ) -> Result<Self, HostError> {
        let numbers: Vec<Retained<NSNumber>> =
            excluded.iter().map(|p| NSNumber::new_u32(*p)).collect();
        let array = NSArray::from_retained_slice(&numbers);
        // SAFETY: `alloc` + designated initializer; `array` holds
        // `NSNumber`s of process object ids to exclude (may be empty), the
        // documented element type.
        let desc = unsafe {
            CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &array,
            )
        };
        Self::create_from(&desc, mute, name)
    }

    fn create_from(desc: &CATapDescription, mute: Mute, name: &str) -> Result<Self, HostError> {
        let behavior = match mute {
            Mute::Unmuted => CATapMuteBehavior::Unmuted,
            Mute::Muted => CATapMuteBehavior::Muted,
            Mute::MutedWhenTapped => CATapMuteBehavior::MutedWhenTapped,
        };
        // SAFETY: plain property setters on a live description.
        unsafe {
            desc.setPrivate(true);
            desc.setMuteBehavior(behavior);
            desc.setName(&NSString::from_str(name));
        }
        // SAFETY: getter on a live description; the UUID string is what the
        // aggregate's tap list refers to (`kAudioSubTapUIDKey`).
        let uid = unsafe { desc.UUID() }.UUIDString().to_string();
        let mut id: ObjectId = 0;
        // SAFETY: `desc` is a fully initialised description and `id` a
        // valid out slot. May block while TCC prompts — callers run this
        // off the UI thread with a timeout.
        let status = unsafe { AudioHardwareCreateProcessTap(Some(desc), &raw mut id) };
        check("AudioHardwareCreateProcessTap", status)?;
        if id == 0 {
            return Err(HostError::Os {
                op: "AudioHardwareCreateProcessTap".to_owned(),
                status: "returned kAudioObjectUnknown".to_owned(),
            });
        }
        Ok(Self { id, uid })
    }

    /// HAL object id of the tap.
    pub(crate) const fn id(&self) -> ObjectId {
        self.id
    }

    /// Tap UID (the description's UUID).
    pub(crate) fn uid(&self) -> &str {
        &self.uid
    }

    /// The format the tap delivers (`kAudioTapPropertyFormat`).
    pub(crate) fn format(&self) -> Result<AudioStreamBasicDescription, HostError> {
        property::get_format(self.id, kAudioTapPropertyFormat)
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        // SAFETY: `id` is a tap this value created and hasn't destroyed.
        let status = unsafe { AudioHardwareDestroyProcessTap(self.id) };
        if status != 0 {
            tracing::warn!(
                tap = self.id,
                status = super::status_string(status),
                "destroy process tap failed"
            );
        }
    }
}

/// A private aggregate device; destroyed on drop.
pub(crate) struct AggregateDevice {
    id: ObjectId,
}

fn cf_key(key: &std::ffi::CStr) -> CFRetained<CFString> {
    CFString::from_str(&key.to_string_lossy())
}

fn as_type<T: AsRef<CFType>>(value: &T) -> &CFType {
    value.as_ref()
}

impl AggregateDevice {
    /// Create a **private** (invisible to other processes), unstacked
    /// aggregate containing `tap_uid`, clocked by `main_subdevice_uid`
    /// when given (which is then also its output side). Tap auto-start and
    /// drift compensation on.
    pub(crate) fn create_private(
        name: &str,
        uid: &str,
        main_subdevice_uid: Option<&str>,
        tap_uid: &str,
    ) -> Result<Self, HostError> {
        let subdevices: Vec<&str> = main_subdevice_uid.into_iter().collect();
        Self::create_private_multi(name, uid, &subdevices, &[tap_uid])
    }

    /// Create a **private**, unstacked aggregate of `subdevice_uids`
    /// (the first is the main subdevice — the clock) and `tap_uids`.
    /// Every other subdevice and every tap is drift compensated against
    /// the main one. The `IOProc` sees input buffers in subdevice order,
    /// then tap order; output buffers in subdevice order.
    pub(crate) fn create_private_multi(
        name: &str,
        uid: &str,
        subdevice_uids: &[&str],
        tap_uids: &[&str],
    ) -> Result<Self, HostError> {
        let id = Self::create_raw(name, uid, subdevice_uids, tap_uids, true)?;
        Ok(Self { id })
    }

    /// Create a **public**, persistent aggregate (listed to every app, like
    /// one made in Audio MIDI Setup) of `subdevice_uids` — the first is the
    /// clock, the rest drift compensated. Not destroyed on drop: returns
    /// the HAL object id; remove it with [`destroy_public`].
    pub(crate) fn create_public(
        name: &str,
        uid: &str,
        subdevice_uids: &[&str],
    ) -> Result<ObjectId, HostError> {
        Self::create_raw(name, uid, subdevice_uids, &[], false)
    }

    fn create_raw(
        name: &str,
        uid: &str,
        subdevice_uids: &[&str],
        tap_uids: &[&str],
        private: bool,
    ) -> Result<ObjectId, HostError> {
        let yes = CFBoolean::new(true);
        let no = CFBoolean::new(false);

        let tap_uid_key = cf_key(kAudioSubTapUIDKey);
        let drift_key = cf_key(kAudioSubTapDriftCompensationKey);
        let tap_uid_cfs: Vec<CFRetained<CFString>> =
            tap_uids.iter().map(|t| CFString::from_str(t)).collect();
        let tap_entries: Vec<CFRetained<CFDictionary<CFString, CFType>>> = tap_uid_cfs
            .iter()
            .map(|t| {
                CFDictionary::<CFString, CFType>::from_slices(
                    &[&*tap_uid_key, &*drift_key],
                    &[as_type(t), as_type(&yes)],
                )
            })
            .collect();
        let tap_refs: Vec<&CFDictionary<CFString, CFType>> =
            tap_entries.iter().map(|e| &**e).collect();
        let taps = CFArray::<CFDictionary<CFString, CFType>>::from_objects(&tap_refs);

        let mut keys: Vec<CFRetained<CFString>> = vec![
            cf_key(kAudioAggregateDeviceNameKey),
            cf_key(kAudioAggregateDeviceUIDKey),
            cf_key(kAudioAggregateDeviceIsPrivateKey),
            cf_key(kAudioAggregateDeviceIsStackedKey),
            cf_key(kAudioAggregateDeviceTapAutoStartKey),
            cf_key(kAudioAggregateDeviceTapListKey),
        ];
        let name_cf = CFString::from_str(name);
        let uid_cf = CFString::from_str(uid);
        let private_cf = if private { &yes } else { &no };
        let mut values: Vec<&CFType> = vec![
            as_type(&name_cf),
            as_type(&uid_cf),
            as_type(private_cf),
            as_type(&no),
            as_type(&yes),
            as_type(&taps),
        ];

        let sub_key = cf_key(kAudioSubDeviceUIDKey);
        let sub_drift_key = cf_key(kAudioSubDeviceDriftCompensationKey);
        let sub_uid_cfs: Vec<CFRetained<CFString>> = subdevice_uids
            .iter()
            .map(|s| CFString::from_str(s))
            .collect();
        let sub_entries: Vec<CFRetained<CFDictionary<CFString, CFType>>> = sub_uid_cfs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                // The main subdevice (first) is the clock; everything else
                // is resampled onto it.
                let drift = if i == 0 { &no } else { &yes };
                CFDictionary::<CFString, CFType>::from_slices(
                    &[&*sub_key, &*sub_drift_key],
                    &[as_type(s), as_type(drift)],
                )
            })
            .collect();
        let sub_refs: Vec<&CFDictionary<CFString, CFType>> =
            sub_entries.iter().map(|e| &**e).collect();
        let subs = CFArray::<CFDictionary<CFString, CFType>>::from_objects(&sub_refs);
        if let Some(main) = sub_uid_cfs.first() {
            keys.push(cf_key(kAudioAggregateDeviceMainSubDeviceKey));
            values.push(as_type(main));
            keys.push(cf_key(kAudioAggregateDeviceSubDeviceListKey));
            values.push(as_type(&subs));
        }

        let key_refs: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
        let description = CFDictionary::<CFString, CFType>::from_slices(&key_refs, &values);
        let mut id: ObjectId = 0;
        // SAFETY: `description` is a live CFDictionary with the documented
        // aggregate keys/value types; `id` is a valid out slot.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(description.as_opaque(), NonNull::from(&mut id))
        };
        check("AudioHardwareCreateAggregateDevice", status)?;
        Ok(id)
    }

    /// HAL object id of the aggregate.
    pub(crate) const fn id(&self) -> ObjectId {
        self.id
    }
}

/// Destroy a public aggregate made by [`AggregateDevice::create_public`].
pub(crate) fn destroy_public(id: ObjectId) -> Result<(), HostError> {
    // SAFETY: `id` is an aggregate device object id.
    let status = unsafe { AudioHardwareDestroyAggregateDevice(id) };
    check("AudioHardwareDestroyAggregateDevice", status)
}

impl Drop for AggregateDevice {
    fn drop(&mut self) {
        // SAFETY: `id` is an aggregate this value created and hasn't
        // destroyed.
        let status = unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
        if status != 0 {
            tracing::warn!(
                device = self.id,
                status = super::status_string(status),
                "destroy aggregate failed"
            );
        }
    }
}
