//! What is on this host right now: the audio processes and the audio
//! devices, with enough detail to show rather than merely enumerate.
//!
//! One pass over the HAL per call. The defaults are read once and folded
//! into the device list, so a caller never has to correlate object ids
//! itself.

use serde::Serialize;

#[cfg(target_os = "macos")]
use crate::ffi::hal::{self, Transport};

/// One process that has opened audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessInfo {
    pub pid: i32,
    /// `None` for a plain binary (`say`, `ffmpeg`) — those have no bundle.
    pub bundle_id: Option<String>,
    pub name: String,
    /// Playing right now.
    pub running_output: bool,
    /// Recording right now.
    pub running_input: bool,
    /// UIDs of the devices it plays to.
    pub output_devices: Vec<String>,
    /// UIDs of the devices it records from.
    pub input_devices: Vec<String>,
}

/// How a device came to exist, as far as the HAL reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Real hardware.
    Hardware,
    /// A driver-published device (`Patchbay.driver`, `BlackHole`, …).
    Virtual,
    /// Several devices clocked together.
    Aggregate,
}

/// Which of the system's defaults a device currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultRole {
    #[default]
    None,
    Output,
    Input,
    /// Both, which is normal for a duplex interface.
    Both,
}

impl DefaultRole {
    const fn of(output: bool, input: bool) -> Self {
        match (output, input) {
            (true, true) => Self::Both,
            (true, false) => Self::Output,
            (false, true) => Self::Input,
            (false, false) => Self::None,
        }
    }

    /// The system plays here by default.
    #[must_use]
    pub const fn is_output(self) -> bool {
        matches!(self, Self::Output | Self::Both)
    }

    /// The system records from here by default.
    #[must_use]
    pub const fn is_input(self) -> bool {
        matches!(self, Self::Input | Self::Both)
    }
}

/// One audio device, as a dashboard needs it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub uid: String,
    pub name: String,
    pub kind: DeviceKind,
    pub input_channels: u32,
    pub output_channels: u32,
    /// Four-char transport code (`bltn`, `usb `, `virt`, `grup`, …).
    pub transport: String,
    /// Nominal rate in Hz, 0 when the device doesn't report one.
    pub sample_rate: f64,
    /// Whether the system plays or records here by default.
    pub default_role: DefaultRole,
    /// Present and answering.
    pub alive: bool,
    /// Some process has it running.
    pub in_use: bool,
}

/// The host, in one read.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Survey {
    pub processes: Vec<ProcessInfo>,
    pub devices: Vec<DeviceInfo>,
}

/// UID prefixes of the aggregates Patchbay builds for its own plumbing.
/// They are real devices, but showing them would be like listing a
/// program's temporary files: `patchbay.mix.*` and `patchbay.monitor.*`
/// exist only while a mix runs.
///
/// `patchbay.aggregate.*` is deliberately NOT in here — those are the
/// ones the user asked for and has to be able to see.
#[cfg(any(target_os = "macos", test))]
const INTERNAL_UID_PREFIXES: [&str; 2] = ["patchbay.mix.", "patchbay.monitor."];

#[cfg(any(target_os = "macos", test))]
fn internal(uid: &str) -> bool {
    INTERNAL_UID_PREFIXES.iter().any(|p| uid.starts_with(p))
}

/// Every audio process except our own, and every device except
/// Patchbay's internal plumbing.
#[must_use]
pub fn survey() -> Survey {
    // Off macOS there is no Core Audio to survey; the caller shows an
    // empty host rather than an error.
    #[cfg(not(target_os = "macos"))]
    return Survey::default();
    #[cfg(target_os = "macos")]
    Survey {
        processes: processes(),
        devices: devices(),
    }
}

/// Audio processes right now (ours excluded — we tap, we don't play).
#[cfg(target_os = "macos")]
#[must_use]
pub fn processes() -> Vec<ProcessInfo> {
    let own = i32::try_from(std::process::id()).unwrap_or(-1);
    hal::process_ids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| hal::process(o).ok())
        .filter(|p| p.pid != own)
        .map(|p| ProcessInfo {
            pid: p.pid,
            bundle_id: p.bundle_id,
            name: p.name,
            running_output: p.running_output,
            running_input: p.running_input,
            output_devices: p.output_devices,
            input_devices: p.input_devices,
        })
        .collect()
}

/// Audio devices right now, Patchbay's internal aggregates excluded.
#[cfg(target_os = "macos")]
#[must_use]
pub fn devices() -> Vec<DeviceInfo> {
    let default_out = hal::default_device(true).ok().flatten();
    let default_in = hal::default_device(false).ok().flatten();
    hal::device_ids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| hal::device(o).ok())
        .filter(|d| !internal(&d.uid))
        .map(|d| DeviceInfo {
            kind: match d.transport {
                Transport::Aggregate => DeviceKind::Aggregate,
                Transport::Virtual => DeviceKind::Virtual,
                Transport::Other => DeviceKind::Hardware,
            },
            default_role: DefaultRole::of(
                default_out == Some(d.object),
                default_in == Some(d.object),
            ),
            uid: d.uid,
            name: d.name,
            input_channels: d.input_channels,
            output_channels: d.output_channels,
            transport: d.transport_code,
            sample_rate: d.sample_rate.unwrap_or(0.0),
            alive: d.alive,
            in_use: d.running_somewhere,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::internal;

    #[test]
    fn only_patchbays_own_plumbing_is_hidden() {
        assert!(internal("patchbay.mix.tap-1.4242"));
        assert!(internal("patchbay.monitor.abc"));
        // The user's own aggregate, and the driver's devices, are theirs.
        assert!(!internal("patchbay.aggregate.REAPER-IO"));
        assert!(!internal("Patchbay-Broadcast_UID"));
        assert!(!internal("AppleHDAEngineOutput:1B,0,1,2:0"));
    }
}
