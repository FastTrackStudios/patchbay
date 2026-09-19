//! Patchbay's virtual audio devices, managed at runtime.
//!
//! `Patchbay.driver` creates, renames and removes them without a
//! coreaudiod restart. Each device is a loopback: apps that play into it
//! can be heard by apps that record from it (and by Patchbay mixes).

use patchbay_host::HostError;
use serde::{Deserialize, Serialize};

use crate::ffi::driver;

/// One virtual device as the driver publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDevice {
    /// Stable id (config key).
    pub id: String,
    /// Core Audio device UID — what apps and mixes refer to; survives renames.
    pub uid: String,
    /// Display name.
    pub name: String,
    pub channels: u16,
    #[serde(default)]
    pub hidden: bool,
}

/// The driver's state document (see `patchbay-driver`'s `DesiredState`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DriverState {
    driver_version: String,
    sample_rate: u32,
    channels: u16,
    buffer_frames: u32,
    devices: Vec<DriverDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DriverDevice {
    id: String,
    uid: String,
    name: String,
    #[serde(default = "loopback")]
    kind: String,
    channels: u16,
    #[serde(default)]
    hidden: bool,
}

fn loopback() -> String {
    "loopback".to_owned()
}

fn plugin() -> Result<u32, HostError> {
    driver::plugin_object()?.ok_or_else(|| {
        HostError::NotFound(format!(
            "Patchbay.driver ({}) is not installed — run packaging/macos/install-driver.sh",
            driver::DRIVER_BUNDLE_ID
        ))
    })
}

/// Whether the driver is installed and loaded.
#[must_use]
pub fn driver_loaded() -> bool {
    driver::plugin_object().is_ok_and(|o| o.is_some())
}

fn read_state(plugin: u32) -> Result<DriverState, HostError> {
    let json = driver::applied_state(plugin)?;
    serde_json::from_str(&json)
        .map_err(|e| HostError::InvalidSpec(format!("driver applied state: {e}")))
}

/// The devices the driver publishes now.
///
/// # Errors
/// [`HostError::NotFound`] when the driver isn't installed.
pub fn list() -> Result<Vec<VirtualDevice>, HostError> {
    let state = read_state(plugin()?)?;
    Ok(state
        .devices
        .into_iter()
        .map(|d| VirtualDevice {
            id: d.id,
            uid: d.uid,
            name: d.name,
            channels: d.channels,
            hidden: d.hidden,
        })
        .collect())
}

/// Make the driver publish exactly `devices` (it diffs, then adds,
/// renames and removes at runtime). Returns once the driver has accepted
/// the request; the change lands within a HAL cycle or two.
///
/// # Errors
/// [`HostError::InvalidSpec`] for an empty/duplicate uid or name or a
/// channel count outside 1..=64; [`HostError::NotFound`] without the
/// driver.
pub fn apply(devices: &[VirtualDevice]) -> Result<(), HostError> {
    let mut uids = std::collections::BTreeSet::new();
    for d in devices {
        if d.uid.trim().is_empty() || d.name.trim().is_empty() {
            return Err(HostError::InvalidSpec(
                "virtual device needs a uid and a name".into(),
            ));
        }
        if !(1..=64).contains(&d.channels) {
            return Err(HostError::InvalidSpec(format!(
                "`{}`: channel count {} outside 1..=64",
                d.name, d.channels
            )));
        }
        if !uids.insert(d.uid.as_str()) {
            return Err(HostError::InvalidSpec(format!("duplicate uid `{}`", d.uid)));
        }
    }
    let plugin = plugin()?;
    let current = read_state(plugin)?;
    let desired = DriverState {
        devices: devices
            .iter()
            .map(|d| DriverDevice {
                id: d.id.clone(),
                uid: d.uid.clone(),
                name: d.name.clone(),
                kind: loopback(),
                channels: d.channels,
                hidden: d.hidden,
            })
            .collect(),
        ..current
    };
    let json = serde_json::to_string(&desired)
        .map_err(|e| HostError::InvalidSpec(format!("serialize desired state: {e}")))?;
    driver::set_desired_state(plugin, &json)
}

/// The driver's IO counters as JSON (diagnostics).
///
/// # Errors
/// [`HostError::NotFound`] without the driver.
pub fn runtime_stats() -> Result<String, HostError> {
    driver::runtime_stats(plugin()?)
}

/// A UID for a new device named `name` (`Patchbay-<slug>_UID`).
#[must_use]
pub fn uid_for(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    format!("Patchbay-{slug}_UID")
}

/// Add a device (or replace the one with the same uid).
///
/// # Errors
/// As [`apply`].
pub fn create(name: &str, channels: u16) -> Result<VirtualDevice, HostError> {
    let mut devices = list()?;
    let uid = uid_for(name);
    let device = VirtualDevice {
        id: uid.trim_end_matches("_UID").to_lowercase(),
        uid: uid.clone(),
        name: name.to_owned(),
        channels,
        hidden: false,
    };
    devices.retain(|d| d.uid != uid);
    devices.push(device.clone());
    apply(&devices)?;
    Ok(device)
}

/// Rename the device with `uid` (its uid, and so every app's selection,
/// stays the same).
///
/// # Errors
/// [`HostError::NotFound`] for an unknown uid; as [`apply`].
pub fn rename(uid: &str, name: &str) -> Result<(), HostError> {
    let mut devices = list()?;
    let d = devices
        .iter_mut()
        .find(|d| d.uid == uid)
        .ok_or_else(|| HostError::NotFound(format!("virtual device `{uid}`")))?;
    name.clone_into(&mut d.name);
    apply(&devices)
}

/// Remove the device with `uid`.
///
/// # Errors
/// [`HostError::NotFound`] for an unknown uid; as [`apply`].
pub fn remove(uid: &str) -> Result<(), HostError> {
    let mut devices = list()?;
    let before = devices.len();
    devices.retain(|d| d.uid != uid);
    if devices.len() == before {
        return Err(HostError::NotFound(format!("virtual device `{uid}`")));
    }
    apply(&devices)
}

/// Prefix of the uids of aggregates Patchbay creates.
pub const AGGREGATE_UID_PREFIX: &str = "patchbay.aggregate.";

/// A public aggregate device Patchbay made (e.g. "REAPER I/O" =
/// Galaxy32 + Patchbay, so a DAW that opens one device gets both).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateInfo {
    pub uid: String,
    pub name: String,
    pub input_channels: u32,
    pub output_channels: u32,
}

/// Aggregates Patchbay created (by uid prefix).
#[must_use]
pub fn aggregates() -> Vec<AggregateInfo> {
    crate::ffi::hal::device_ids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| crate::ffi::hal::device(o).ok())
        .filter(|d| d.uid.starts_with(AGGREGATE_UID_PREFIX))
        .map(|d| AggregateInfo {
            uid: d.uid,
            name: d.name,
            input_channels: d.input_channels,
            output_channels: d.output_channels,
        })
        .collect()
}

/// Create a public aggregate `name` of `device_uids` (the first clocks it;
/// the rest are drift compensated). Persists like an Audio MIDI Setup
/// aggregate until removed.
///
/// # Errors
/// [`HostError::NotFound`] for a missing device; [`HostError::Os`].
pub fn create_aggregate(name: &str, device_uids: &[String]) -> Result<AggregateInfo, HostError> {
    if device_uids.len() < 2 {
        return Err(HostError::InvalidSpec(
            "an aggregate needs at least two devices".into(),
        ));
    }
    for uid in device_uids {
        if crate::ffi::hal::device_by_uid(uid)?.is_none() {
            return Err(HostError::NotFound(format!("device `{uid}`")));
        }
    }
    let uid = format!(
        "{AGGREGATE_UID_PREFIX}{}",
        uid_for(name)
            .trim_start_matches("Patchbay-")
            .trim_end_matches("_UID")
    );
    if aggregates().iter().any(|a| a.uid == uid) {
        return Err(HostError::InvalidSpec(format!(
            "an aggregate named `{name}` exists"
        )));
    }
    let subs: Vec<&str> = device_uids.iter().map(String::as_str).collect();
    crate::ffi::tap::AggregateDevice::create_public(name, &uid, &subs)?;
    // The HAL publishes the new device asynchronously: wait up to ~2 s.
    for _ in 0..40 {
        if let Some(a) = aggregates().into_iter().find(|a| a.uid == uid) {
            return Ok(a);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(HostError::NotFound(format!(
        "aggregate `{uid}` after creation"
    )))
}

/// Remove an aggregate Patchbay created.
///
/// # Errors
/// [`HostError::NotFound`] when `uid` isn't a Patchbay aggregate.
pub fn remove_aggregate(uid: &str) -> Result<(), HostError> {
    if !uid.starts_with(AGGREGATE_UID_PREFIX) {
        return Err(HostError::InvalidSpec(format!(
            "`{uid}` wasn't made by Patchbay (only `{AGGREGATE_UID_PREFIX}*` can be removed)"
        )));
    }
    let d = crate::ffi::hal::device_by_uid(uid)?
        .ok_or_else(|| HostError::NotFound(format!("aggregate `{uid}`")))?;
    crate::ffi::tap::destroy_public(d.object)
}

#[cfg(test)]
mod tests {
    use super::uid_for;

    #[test]
    fn uids_are_stable_slugs() {
        assert_eq!(uid_for("Discord Mix"), "Patchbay-Discord-Mix_UID");
        assert_eq!(uid_for("  OBS / Stream  "), "Patchbay-OBS---Stream_UID");
    }
}
