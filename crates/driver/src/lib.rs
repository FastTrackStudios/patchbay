#![allow(unsafe_code)]
//! Patchbay's AudioServerPlugIn: loopback virtual devices managed at
//! runtime (see the crate README).
//!
//! Forked from MARS `mars-hal` (MIT, Copyright (c) 2026 JacobLinCool; see
//! `LICENSE.MARS`). Unsafe operations are concentrated in this crate.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROPERTY_DESIRED_STATE: &str = "app.fasttrackstudio.patchbay.desired_state";
pub const PROPERTY_APPLIED_STATE: &str = "app.fasttrackstudio.patchbay.applied_state";
pub const PROPERTY_RUNTIME_STATS: &str = "app.fasttrackstudio.patchbay.runtime_stats";

/// coreaudiod storage key holding the applied state (JSON), so devices
/// survive coreaudiod restarts without Patchbay.app running.
pub const STORAGE_KEY_APPLIED_STATE: &str = "applied_state";

/// Device kind published for every Patchbay device.
pub const KIND_LOOPBACK: &str = "loopback";

pub mod coreaudio_types;
pub mod plugin;

/// Tests touch process-global driver state; they take this lock.
#[cfg(test)]
pub(crate) static TEST_LOCK: once_cell::sync::Lazy<parking_lot::Mutex<()>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesiredState {
    pub driver_version: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: u32,
    pub devices: Vec<HalDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppliedState {
    pub driver_version: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: u32,
    pub devices: Vec<HalDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeStats {
    pub underrun_count: u64,
    pub overrun_count: u64,
    pub xrun_count: u64,
    pub last_callback_ns: u64,
    /// Diagnostic: StartIO invocations in this plugin instance.
    #[serde(default)]
    pub start_io_count: u64,
    /// Diagnostic: GetZeroTimeStamp invocations in this plugin instance.
    #[serde(default)]
    pub zero_timestamp_count: u64,
    /// Diagnostic: DoIOOperation invocations in this plugin instance.
    #[serde(default)]
    pub do_io_count: u64,
    /// Diagnostic: StopIO invocations.
    #[serde(default)]
    pub stop_io_count: u64,
    /// Diagnostic: loopback WriteMix operations.
    #[serde(default)]
    pub write_count: u64,
    /// Diagnostic: loopback ReadInput operations.
    #[serde(default)]
    pub read_count: u64,
    /// Diagnostic: ReadInput operations answered with silence (nothing
    /// written recently, or muted).
    #[serde(default)]
    pub silent_read_count: u64,
    /// Diagnostic: output sample time of the last WriteMix.
    #[serde(default)]
    pub last_write_time: f64,
    /// Diagnostic: input sample time of the last ReadInput.
    #[serde(default)]
    pub last_read_time: f64,
    /// Diagnostic: `WillDoIOOperation` asked about ReadInput.
    #[serde(default)]
    pub will_read_count: u64,
    /// Diagnostic: `WillDoIOOperation` asked about WriteMix.
    #[serde(default)]
    pub will_write_count: u64,
    /// Diagnostic: `WillDoIOOperation` asked about anything else.
    #[serde(default)]
    pub will_other_count: u64,
    /// Diagnostic: the last operation id `WillDoIOOperation` was asked
    /// about (four-char code as a number).
    #[serde(default)]
    pub last_will_op: u64,
    /// Diagnostic: `BeginIOOperation` calls.
    #[serde(default)]
    pub begin_io_count: u64,
    /// Diagnostic: `AddDeviceClient` calls.
    #[serde(default)]
    pub add_client_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HalDevice {
    /// Stable id (Patchbay's config key).
    pub id: String,
    /// Core Audio device UID apps and configs refer to.
    pub uid: String,
    /// Display name (renaming keeps the UID, so apps keep their selection).
    pub name: String,
    /// Always [`KIND_LOOPBACK`] (kept for wire compatibility).
    #[serde(default = "loopback_kind")]
    pub kind: String,
    pub channels: u16,
    #[serde(default)]
    pub hidden: bool,
}

fn loopback_kind() -> String {
    KIND_LOOPBACK.to_owned()
}

impl HalDevice {
    /// A visible loopback device.
    #[must_use]
    pub fn loopback(id: &str, uid: &str, name: &str, channels: u16) -> Self {
        Self {
            id: id.to_owned(),
            uid: uid.to_owned(),
            name: name.to_owned(),
            kind: loopback_kind(),
            channels,
            hidden: false,
        }
    }
}

/// What a fresh install publishes before Patchbay.app says otherwise:
/// **Patchbay** (16 ch; apps output into it) and **Broadcast** (2 ch;
/// apps take it as a microphone). UIDs match the earlier BlackHole-based
/// drivers so saved mixes keep working.
#[must_use]
pub fn default_desired_state() -> DesiredState {
    DesiredState {
        driver_version: env!("CARGO_PKG_VERSION").to_string(),
        sample_rate: 48_000,
        channels: 2,
        buffer_frames: 512,
        devices: vec![
            HalDevice::loopback("patchbay", "Patchbay_UID", "Patchbay", 16),
            HalDevice::loopback("broadcast", "Broadcast_UID", "Broadcast", 2),
        ],
    }
}

/// The applied state as a `DesiredState` (what to persist and restore).
#[must_use]
pub fn applied_as_desired() -> DesiredState {
    let state = DRIVER_STATE.lock();
    let a = &state.applied_state;
    DesiredState {
        driver_version: a.driver_version.clone(),
        sample_rate: a.sample_rate,
        channels: a.channels,
        buffer_frames: a.buffer_frames,
        devices: a.devices.clone(),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigChangeKind {
    CreateDevice,
    UpdateDevice,
    RemoveDevice,
    UpdateAudioConfig,
    NoOp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigChange {
    pub kind: ConfigChangeKind,
    pub target: String,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingConfigurationChange {
    pub generation: u64,
    pub created_at_ms: u64,
    pub changes: Vec<ConfigChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigurationSummary {
    pub current_generation: u64,
    pub request_count: u64,
    pub perform_count: u64,
    pub applied_device_count: usize,
    pub pending: Option<PendingConfigurationChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigChangeResult {
    pub applied: bool,
    pub generation: u64,
    pub changes: Vec<ConfigChange>,
}

#[derive(Debug, Error)]
pub enum HalError {
    #[error("invalid desired state json: {0}")]
    InvalidDesiredState(serde_json::Error),
    #[error("serialize failed: {0}")]
    Serialize(serde_json::Error),
    #[error("no desired state staged")]
    NoDesiredState,
    #[error("no pending configuration change")]
    NoPendingConfigurationChange,
    #[error("configuration generation mismatch: expected {expected}, got {actual}")]
    GenerationMismatch { expected: u64, actual: u64 },
    #[error("invalid generation value: {0}")]
    InvalidGeneration(i64),
}

#[derive(Debug, Clone)]
struct PendingChangeInternal {
    generation: u64,
    created_at_ms: u64,
    changes: Vec<ConfigChange>,
    desired_state: DesiredState,
}

#[derive(Debug)]
pub(crate) struct DriverState {
    pub(crate) desired_state: Option<DesiredState>,
    pub(crate) applied_state: AppliedState,
    pub(crate) pending_change: Option<PendingChangeInternal>,
    pub(crate) current_generation: u64,
    pub(crate) request_count: u64,
    pub(crate) perform_count: u64,
}

/// Lock-free runtime counters updated from the realtime IO callback.
///
/// These live outside `DRIVER_STATE` so the realtime thread never blocks on
/// the configuration mutex (which non-RT paths hold across serialization and
/// syscalls). Monotonic counters need no lock; readers assemble a
/// [`RuntimeStats`] snapshot from relaxed loads.
#[derive(Debug)]
pub(crate) struct AtomicRuntimeStats {
    pub(crate) underrun_count: AtomicU64,
    pub(crate) overrun_count: AtomicU64,
    pub(crate) xrun_count: AtomicU64,
    pub(crate) last_callback_ns: AtomicU64,
    pub(crate) start_io_count: AtomicU64,
    pub(crate) zero_timestamp_count: AtomicU64,
    pub(crate) do_io_count: AtomicU64,
    pub(crate) stop_io_count: AtomicU64,
    pub(crate) write_count: AtomicU64,
    pub(crate) read_count: AtomicU64,
    pub(crate) silent_read_count: AtomicU64,
    pub(crate) last_write_time_bits: AtomicU64,
    pub(crate) last_read_time_bits: AtomicU64,
    pub(crate) will_read_count: AtomicU64,
    pub(crate) will_write_count: AtomicU64,
    pub(crate) will_other_count: AtomicU64,
    pub(crate) last_will_op: AtomicU64,
    pub(crate) begin_io_count: AtomicU64,
    pub(crate) add_client_count: AtomicU64,
}

impl AtomicRuntimeStats {
    pub(crate) fn snapshot(&self) -> RuntimeStats {
        RuntimeStats {
            underrun_count: self.underrun_count.load(Ordering::Relaxed),
            overrun_count: self.overrun_count.load(Ordering::Relaxed),
            xrun_count: self.xrun_count.load(Ordering::Relaxed),
            last_callback_ns: self.last_callback_ns.load(Ordering::Relaxed),
            start_io_count: self.start_io_count.load(Ordering::Relaxed),
            zero_timestamp_count: self.zero_timestamp_count.load(Ordering::Relaxed),
            do_io_count: self.do_io_count.load(Ordering::Relaxed),
            stop_io_count: self.stop_io_count.load(Ordering::Relaxed),
            write_count: self.write_count.load(Ordering::Relaxed),
            read_count: self.read_count.load(Ordering::Relaxed),
            silent_read_count: self.silent_read_count.load(Ordering::Relaxed),
            last_write_time: f64::from_bits(self.last_write_time_bits.load(Ordering::Relaxed)),
            last_read_time: f64::from_bits(self.last_read_time_bits.load(Ordering::Relaxed)),
            will_read_count: self.will_read_count.load(Ordering::Relaxed),
            will_write_count: self.will_write_count.load(Ordering::Relaxed),
            will_other_count: self.will_other_count.load(Ordering::Relaxed),
            last_will_op: self.last_will_op.load(Ordering::Relaxed),
            begin_io_count: self.begin_io_count.load(Ordering::Relaxed),
            add_client_count: self.add_client_count.load(Ordering::Relaxed),
        }
    }
}

pub(crate) static RUNTIME_STATS: AtomicRuntimeStats = AtomicRuntimeStats {
    underrun_count: AtomicU64::new(0),
    overrun_count: AtomicU64::new(0),
    xrun_count: AtomicU64::new(0),
    last_callback_ns: AtomicU64::new(0),
    start_io_count: AtomicU64::new(0),
    zero_timestamp_count: AtomicU64::new(0),
    do_io_count: AtomicU64::new(0),
    stop_io_count: AtomicU64::new(0),
    write_count: AtomicU64::new(0),
    read_count: AtomicU64::new(0),
    silent_read_count: AtomicU64::new(0),
    last_write_time_bits: AtomicU64::new(0),
    last_read_time_bits: AtomicU64::new(0),
    will_read_count: AtomicU64::new(0),
    will_write_count: AtomicU64::new(0),
    will_other_count: AtomicU64::new(0),
    last_will_op: AtomicU64::new(0),
    begin_io_count: AtomicU64::new(0),
    add_client_count: AtomicU64::new(0),
};

impl Default for DriverState {
    fn default() -> Self {
        Self {
            desired_state: None,
            applied_state: AppliedState {
                driver_version: env!("CARGO_PKG_VERSION").to_string(),
                sample_rate: 48_000,
                channels: 2,
                buffer_frames: 512,
                devices: Vec::new(),
            },
            pending_change: None,
            current_generation: 0,
            request_count: 0,
            perform_count: 0,
        }
    }
}

pub(crate) static DRIVER_STATE: Lazy<Mutex<DriverState>> =
    Lazy::new(|| Mutex::new(DriverState::default()));

pub fn set_desired_state_json(raw: &str) -> Result<(), HalError> {
    let desired =
        serde_json::from_str::<DesiredState>(raw).map_err(HalError::InvalidDesiredState)?;

    let mut state = DRIVER_STATE.lock();
    let changes = build_change_plan(&state.applied_state, &desired);
    state.desired_state = Some(desired.clone());

    if changes.len() == 1 && changes[0].kind == ConfigChangeKind::NoOp {
        state.pending_change = None;
    } else {
        let generation = state.current_generation.saturating_add(1);
        state.pending_change = Some(PendingChangeInternal {
            generation,
            created_at_ms: epoch_millis(),
            changes,
            desired_state: desired,
        });
    }

    Ok(())
}

pub fn request_device_configuration_change() -> Result<u64, HalError> {
    let mut state = DRIVER_STATE.lock();
    if state.desired_state.is_none() {
        return Err(HalError::NoDesiredState);
    }

    state.request_count = state.request_count.saturating_add(1);

    if let Some(pending) = state.pending_change.as_ref() {
        Ok(pending.generation)
    } else {
        // No pending diff: callers can treat current generation as a converged token.
        Ok(state.current_generation)
    }
}

pub fn perform_device_configuration_change(
    generation: u64,
) -> Result<ConfigChangeResult, HalError> {
    let mut state = DRIVER_STATE.lock();
    // Returning NoPendingConfigurationChange makes no-op requests explicit to callers.
    let pending = state
        .pending_change
        .clone()
        .ok_or(HalError::NoPendingConfigurationChange)?;

    if pending.generation != generation {
        return Err(HalError::GenerationMismatch {
            expected: pending.generation,
            actual: generation,
        });
    }

    state.applied_state = applied_from_desired(&pending.desired_state);
    state.pending_change = None;
    state.current_generation = pending.generation;
    state.perform_count = state.perform_count.saturating_add(1);

    Ok(ConfigChangeResult {
        applied: true,
        generation: pending.generation,
        changes: pending.changes,
    })
}

pub fn pending_change() -> Option<PendingConfigurationChange> {
    let state = DRIVER_STATE.lock();
    state.pending_change.as_ref().map(public_pending)
}

pub fn pending_change_json() -> Result<String, HalError> {
    serde_json::to_string(&pending_change()).map_err(HalError::Serialize)
}

pub fn configuration_summary() -> ConfigurationSummary {
    let state = DRIVER_STATE.lock();
    ConfigurationSummary {
        current_generation: state.current_generation,
        request_count: state.request_count,
        perform_count: state.perform_count,
        applied_device_count: state.applied_state.devices.len(),
        pending: state.pending_change.as_ref().map(public_pending),
    }
}

pub fn configuration_summary_json() -> Result<String, HalError> {
    serde_json::to_string(&configuration_summary()).map_err(HalError::Serialize)
}

pub fn applied_state_json() -> Result<String, HalError> {
    // Clone under the lock, serialize outside: the realtime IO callback must
    // never wait behind serde while this mutex is held.
    let applied = {
        let state = DRIVER_STATE.lock();
        state.applied_state.clone()
    };
    serde_json::to_string(&applied).map_err(HalError::Serialize)
}

pub fn runtime_stats_json() -> Result<String, HalError> {
    serde_json::to_string(&RUNTIME_STATS.snapshot()).map_err(HalError::Serialize)
}

pub fn applied_devices() -> Vec<HalDevice> {
    let state = DRIVER_STATE.lock();
    state.applied_state.devices.clone()
}

pub fn applied_device_count() -> usize {
    let state = DRIVER_STATE.lock();
    state.applied_state.devices.len()
}

fn build_change_plan(applied: &AppliedState, desired: &DesiredState) -> Vec<ConfigChange> {
    let mut changes = Vec::new();

    if applied.sample_rate != desired.sample_rate
        || applied.channels != desired.channels
        || applied.buffer_frames != desired.buffer_frames
    {
        changes.push(ConfigChange {
            kind: ConfigChangeKind::UpdateAudioConfig,
            target: "audio".to_string(),
            details: format!(
                "sample_rate {} -> {}, channels {} -> {}, buffer_frames {} -> {}",
                applied.sample_rate,
                desired.sample_rate,
                applied.channels,
                desired.channels,
                applied.buffer_frames,
                desired.buffer_frames
            ),
        });
    }

    let mut applied_by_uid = BTreeMap::<&str, &HalDevice>::new();
    for device in &applied.devices {
        applied_by_uid.insert(device.uid.as_str(), device);
    }

    let mut desired_by_uid = BTreeMap::<&str, &HalDevice>::new();
    for device in &desired.devices {
        desired_by_uid.insert(device.uid.as_str(), device);
    }

    for (uid, device) in &desired_by_uid {
        match applied_by_uid.get(uid) {
            None => changes.push(ConfigChange {
                kind: ConfigChangeKind::CreateDevice,
                target: (*uid).to_string(),
                details: format!("create {} ({})", device.name, normalize_kind(&device.kind)),
            }),
            Some(existing) => {
                if *existing != *device {
                    changes.push(ConfigChange {
                        kind: ConfigChangeKind::UpdateDevice,
                        target: (*uid).to_string(),
                        details: format!("update {} -> {}", existing.name, device.name),
                    });
                }
            }
        }
    }

    for (uid, existing) in &applied_by_uid {
        if !desired_by_uid.contains_key(uid) {
            changes.push(ConfigChange {
                kind: ConfigChangeKind::RemoveDevice,
                target: (*uid).to_string(),
                details: format!(
                    "remove {} ({})",
                    existing.name,
                    normalize_kind(&existing.kind)
                ),
            });
        }
    }

    if changes.is_empty() {
        changes.push(ConfigChange {
            kind: ConfigChangeKind::NoOp,
            target: "state".to_string(),
            details: "already converged".to_string(),
        });
    }

    changes
}

fn applied_from_desired(desired: &DesiredState) -> AppliedState {
    AppliedState {
        driver_version: desired.driver_version.clone(),
        sample_rate: desired.sample_rate,
        channels: desired.channels,
        buffer_frames: desired.buffer_frames,
        devices: desired.devices.clone(),
    }
}

fn public_pending(internal: &PendingChangeInternal) -> PendingConfigurationChange {
    PendingConfigurationChange {
        generation: internal.generation,
        created_at_ms: internal.created_at_ms,
        changes: internal.changes.clone(),
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn normalize_kind(kind: &str) -> String {
    kind.trim().to_lowercase().replace(' ', "_")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{
        HalError, applied_state_json, configuration_summary, pending_change,
        perform_device_configuration_change, request_device_configuration_change,
        set_desired_state_json,
    };

    use crate::TEST_LOCK;

    const SAMPLE_PAYLOAD: &str = r#"{
      "driver_version": "1.0.0",
      "sample_rate": 48000,
      "channels": 2,
      "buffer_frames": 256,
      "devices": [
        {
          "id": "mix-main",
          "uid": "Mix_UID",
          "name": "Mix Main",
          "kind": "loopback",
          "channels": 2
        }
      ]
    }"#;

    const EMPTY_PAYLOAD: &str = r#"{
      "driver_version": "1.0.0",
      "sample_rate": 48000,
      "channels": 2,
      "buffer_frames": 256,
      "devices": []
    }"#;

    fn reset_driver_state() {
        set_desired_state_json(EMPTY_PAYLOAD).expect("stage reset desired state");
        let generation = request_device_configuration_change().expect("request reset change");
        let _ = perform_device_configuration_change(generation);
    }

    #[test]
    fn staged_change_updates_applied_after_perform() {
        let _guard = TEST_LOCK.lock();
        reset_driver_state();
        set_desired_state_json(SAMPLE_PAYLOAD).expect("stage desired state");

        let generation = request_device_configuration_change().expect("request change");
        assert!(generation >= 1);
        assert!(pending_change().is_some());

        let result = perform_device_configuration_change(generation).expect("perform change");
        assert!(result.applied);
        assert!(result.generation >= 1);

        let applied = applied_state_json().expect("read applied state");
        assert!(applied.contains("mix-main"));
    }

    #[test]
    fn generation_mismatch_is_reported() {
        let _guard = TEST_LOCK.lock();
        reset_driver_state();
        set_desired_state_json(SAMPLE_PAYLOAD).expect("stage desired state");
        let generation = request_device_configuration_change().expect("request change");

        let err = perform_device_configuration_change(generation + 1).expect_err("must fail");
        assert!(matches!(err, HalError::GenerationMismatch { .. }));

        perform_device_configuration_change(generation).expect("apply with correct generation");
    }

    #[test]
    fn no_pending_change_returns_error() {
        let _guard = TEST_LOCK.lock();
        reset_driver_state();
        set_desired_state_json(EMPTY_PAYLOAD).expect("stage desired");
        let generation = request_device_configuration_change().expect("request change");

        // no pending change because desired == applied
        assert_eq!(generation, configuration_summary().current_generation);
        let err = perform_device_configuration_change(generation).expect_err("must fail");
        assert!(matches!(err, HalError::NoPendingConfigurationChange));
    }
}
