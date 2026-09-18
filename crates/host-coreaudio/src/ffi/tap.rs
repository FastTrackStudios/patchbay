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
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
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
        let yes = CFBoolean::new(true);
        let no = CFBoolean::new(false);

        let tap_uid_key = cf_key(kAudioSubTapUIDKey);
        let drift_key = cf_key(kAudioSubTapDriftCompensationKey);
        let tap_uid_cf = CFString::from_str(tap_uid);
        let tap_entry = CFDictionary::<CFString, CFType>::from_slices(
            &[&*tap_uid_key, &*drift_key],
            &[as_type(&tap_uid_cf), as_type(&yes)],
        );
        let taps = CFArray::<CFDictionary<CFString, CFType>>::from_objects(&[&*tap_entry]);

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
        let mut values: Vec<&CFType> = vec![
            as_type(&name_cf),
            as_type(&uid_cf),
            as_type(&yes),
            as_type(&no),
            as_type(&yes),
            as_type(&taps),
        ];

        let sub_uid_cf = main_subdevice_uid.map(CFString::from_str);
        let sub_key = cf_key(kAudioSubDeviceUIDKey);
        let sub_entry = sub_uid_cf
            .as_ref()
            .map(|s| CFDictionary::<CFString, CFType>::from_slices(&[&*sub_key], &[as_type(s)]));
        let subs = sub_entry
            .as_ref()
            .map(|e| CFArray::<CFDictionary<CFString, CFType>>::from_objects(&[&**e]));
        if let (Some(sub_uid), Some(subs)) = (sub_uid_cf.as_ref(), subs.as_ref()) {
            keys.push(cf_key(kAudioAggregateDeviceMainSubDeviceKey));
            values.push(as_type(sub_uid));
            keys.push(cf_key(kAudioAggregateDeviceSubDeviceListKey));
            values.push(as_type(subs));
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
        Ok(Self { id })
    }

    /// HAL object id of the aggregate.
    pub(crate) const fn id(&self) -> ObjectId {
        self.id
    }
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
