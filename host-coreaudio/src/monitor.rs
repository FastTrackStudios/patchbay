//! App → output-device monitoring through a process tap and a private
//! aggregate (no HAL driver needed).
//!
//! ```text
//!  app process ──(CATapDescription, stereo mixdown, private)──▶ tap
//!  private aggregate = [main subdevice: output device] + [tap]
//!  IOProc on the aggregate: tap input ──channel map × gain──▶ device output
//! ```
//!
//! The aggregate is clocked by the output device and the tap is drift
//! compensated, so input and output arrive in one callback, in sync.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use patchbay_host::{AppInfo, AppSelector, Gain, HostError};
use serde::Serialize;

use crate::config::{CapturePermission, TapMonitorConfig};
use crate::ffi::hal::{self, ProcessFacts};
use crate::ffi::ioproc::{Buffers, BuffersMut, IoProc, Render};
use crate::ffi::sys::{self, Preflight};
use crate::ffi::tap::{AggregateDevice, Mute, ProcessTap};

/// Ask TCC (without prompting) whether taps from this process will carry
/// audio. Best-effort: uses a private SPI; `Unknown` when unsure.
#[must_use]
pub fn capture_permission() -> CapturePermission {
    match sys::audio_capture_preflight() {
        Preflight::Granted => CapturePermission::Granted,
        Preflight::Denied => CapturePermission::Denied,
        Preflight::NotDetermined => CapturePermission::NotDetermined,
        Preflight::Unknown => CapturePermission::Unknown,
    }
}

/// What a running [`TapMonitor`] is made of (for logs / UIs).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TapMonitorInfo {
    /// The tapped processes.
    pub processes: Vec<AppInfo>,
    /// Tap UID.
    pub tap_uid: String,
    /// HAL object id of the tap.
    pub tap_object: u32,
    /// Private aggregate UID.
    pub aggregate_uid: String,
    /// HAL object id of the aggregate.
    pub aggregate_object: u32,
    /// Tap sample rate.
    pub tap_sample_rate: f64,
    /// Tap channel count (2 for a stereo mixdown).
    pub tap_channels: u32,
    /// Output device display name.
    pub output_device: String,
    /// Output device nominal rate.
    pub output_sample_rate: Option<f64>,
    /// Aggregate input side, channels per buffer (subdevice inputs first,
    /// then the tap).
    pub aggregate_input_layout: Vec<u32>,
    /// Aggregate output side, channels per buffer.
    pub aggregate_output_layout: Vec<u32>,
    /// Input buffers skipped before the tap (the output device's own
    /// capture streams).
    pub tap_buffer_offset: usize,
}

/// A running app → output monitor. Everything is torn down on drop, in
/// order: IO stopped, `IOProc` destroyed, aggregate destroyed, tap
/// destroyed.
pub struct TapMonitor {
    // Field order is drop order — keep the IOProc first. Held only for
    // its `Drop` (stop + destroy before the aggregate goes away).
    _io: IoProc<Router>,
    router: Arc<Router>,
    aggregate: AggregateDevice,
    tap: ProcessTap,
    info: TapMonitorInfo,
}

impl std::fmt::Debug for TapMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapMonitor")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Resolve an [`AppSelector`] to HAL process objects (never ourselves).
fn resolve(app: &AppSelector) -> Result<Vec<ProcessFacts>, HostError> {
    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    let found: Vec<ProcessFacts> = match app {
        AppSelector::Pid(pid) => hal::process_for_pid(*pid)?
            .and_then(|obj| hal::process(obj).ok())
            .into_iter()
            .collect(),
        AppSelector::BundleId(bundle) => hal::process_ids()?
            .into_iter()
            .filter_map(|obj| hal::process(obj).ok())
            .filter(|p| {
                p.bundle_id
                    .as_deref()
                    .is_some_and(|b| patchbay_host::bundle_matches(bundle, b))
            })
            .collect(),
    };
    let found: Vec<ProcessFacts> = found.into_iter().filter(|p| p.pid != own_pid).collect();
    if found.is_empty() {
        return Err(HostError::NotFound(format!(
            "no Core Audio client process matches {app:?} (the app must have opened audio at least once)"
        )));
    }
    Ok(found)
}

impl TapMonitor {
    /// Build and start the monitor. Blocking; may block for as long as a
    /// TCC prompt is up — prefer [`Self::start_with_timeout`].
    ///
    /// # Errors
    /// [`HostError::NotFound`] (app / device), [`HostError::InvalidSpec`]
    /// (channel map, gain, tap format), [`HostError::Os`].
    pub fn start(config: &TapMonitorConfig) -> Result<Self, HostError> {
        let gain = Gain::validate("monitor", config.gain)?;
        let processes = resolve(&config.app)?;
        let output = hal::device_by_uid(&config.output_device_uid)?.ok_or_else(|| {
            HostError::NotFound(format!("output device `{}`", config.output_device_uid))
        })?;
        if output.output_channels == 0 {
            return Err(HostError::InvalidSpec(format!(
                "device `{}` has no output channels",
                output.name
            )));
        }

        let objects: Vec<u32> = processes.iter().map(|p| p.object).collect();
        let tap = ProcessTap::create(&objects, Mute::for_tap(config.mute), "patchbay monitor")?;
        let format = hal::summarize(&tap.format()?);
        if !format.float32 {
            return Err(HostError::InvalidSpec(format!(
                "tap format is not Float32: {format:?}"
            )));
        }
        config
            .channel_map
            .validate(Some(format.channels), Some(output.output_channels))?;

        let aggregate_uid = format!("patchbay.monitor.{}", tap.uid());
        let aggregate = AggregateDevice::create_private(
            "patchbay monitor",
            &aggregate_uid,
            Some(&output.uid),
            tap.uid(),
        )?;
        let input_layout = hal::stream_layout(aggregate.id(), true);
        let output_layout = hal::stream_layout(aggregate.id(), false);

        let router = Arc::new(Router::new(
            config,
            format.channels,
            output.input_streams,
            gain,
        )?);
        let mut io = IoProc::create(aggregate.id(), Arc::clone(&router))?;
        io.start()?;

        let info = TapMonitorInfo {
            processes: processes
                .iter()
                .map(|p| AppInfo {
                    pid: p.pid,
                    bundle_id: p.bundle_id.clone(),
                    name: p.name.clone(),
                })
                .collect(),
            tap_uid: tap.uid().to_owned(),
            tap_object: tap.id(),
            aggregate_uid,
            aggregate_object: aggregate.id(),
            tap_sample_rate: format.sample_rate,
            tap_channels: format.channels,
            output_device: output.name,
            output_sample_rate: output.sample_rate,
            aggregate_input_layout: input_layout,
            aggregate_output_layout: output_layout,
            tap_buffer_offset: output.input_streams,
        };
        Ok(Self {
            _io: io,
            router,
            aggregate,
            tap,
            info,
        })
    }

    /// [`Self::start`] on a helper thread, giving up after `timeout` (a
    /// pending "System Audio Recording" prompt blocks tap creation). If
    /// the start completes after the timeout, the monitor is torn down
    /// immediately.
    ///
    /// # Errors
    /// As [`Self::start`], plus [`HostError::Timeout`].
    pub fn start_with_timeout(
        config: &TapMonitorConfig,
        timeout: Duration,
    ) -> Result<Self, HostError> {
        let (tx, rx) = mpsc::sync_channel(1);
        let config = config.clone();
        std::thread::Builder::new()
            .name("patchbay-ca-tap-start".to_owned())
            .spawn(move || {
                // If the receiver timed out, `send` hands the monitor back
                // and dropping it tears everything down.
                let _ = tx.send(Self::start(&config));
            })
            .map_err(|e| HostError::Os {
                op: "spawn tap thread".to_owned(),
                status: e.to_string(),
            })?;
        rx.recv_timeout(timeout).map_err(|_| {
            HostError::Timeout(format!(
                "creating the process tap took over {timeout:?} — a \"System Audio Recording\" permission prompt is \
                 probably waiting (System Settings → Privacy & Security → Screen & System Audio Recording)"
            ))
        })?
    }

    /// Composition details.
    #[must_use]
    pub const fn info(&self) -> &TapMonitorInfo {
        &self.info
    }

    /// Change the gain (lock-free, takes effect next cycle).
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] for an out-of-range gain.
    pub fn set_gain(&self, gain: f32) -> Result<(), HostError> {
        let gain = Gain::validate("monitor", gain)?;
        self.router.gain.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }

    /// Per-tap-channel peak (linear, pre-gain) since the last call.
    #[must_use]
    pub fn take_peaks(&self) -> Vec<f32> {
        self.router
            .peaks
            .iter()
            .map(|p| f32::from_bits(p.swap(0, Ordering::Relaxed)))
            .collect()
    }

    /// IO cycles rendered so far (0 means the device never started).
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.router.cycles.load(Ordering::Relaxed)
    }

    /// `(input buffers, output buffers)` the last IO cycle saw.
    #[must_use]
    pub fn observed_buffers(&self) -> (u32, u32) {
        (
            self.router.seen_in.load(Ordering::Relaxed),
            self.router.seen_out.load(Ordering::Relaxed),
        )
    }

    /// Tap and aggregate HAL object ids (for teardown checks).
    #[must_use]
    pub const fn objects(&self) -> (u32, u32) {
        (self.tap.id(), self.aggregate.id())
    }
}

/// One `tap channel → output channel` route, resolved to indices.
#[derive(Debug, Clone, Copy)]
struct Route {
    src: usize,
    dst: usize,
}

/// Real-time state shared with the IO thread. Immutable after creation
/// except for atomics.
struct Router {
    routes: Box<[Route]>,
    /// `f32` bits of the linear gain.
    gain: AtomicU32,
    /// `f32` bits of the running peak per tap channel. Non-negative floats
    /// order like their bit patterns, so `fetch_max` on bits is a float max.
    peaks: Box<[AtomicU32]>,
    /// Input buffers belonging to the main subdevice, before the tap's.
    skip_in: usize,
    cycles: AtomicU64,
    seen_in: AtomicU32,
    seen_out: AtomicU32,
}

impl Router {
    fn new(
        config: &TapMonitorConfig,
        tap_channels: u32,
        skip_in: usize,
        gain: f32,
    ) -> Result<Self, HostError> {
        let bad = || HostError::InvalidSpec("channel index does not fit usize".to_owned());
        let routes = config
            .channel_map
            .pairs()
            .iter()
            .map(|p| {
                Ok(Route {
                    src: usize::try_from(p.src).map_err(|_| bad())?,
                    dst: usize::try_from(p.dst).map_err(|_| bad())?,
                })
            })
            .collect::<Result<Vec<_>, HostError>>()?;
        Ok(Self {
            routes: routes.into_boxed_slice(),
            gain: AtomicU32::new(gain.to_bits()),
            peaks: (0..tap_channels).map(|_| AtomicU32::new(0)).collect(),
            skip_in,
            cycles: AtomicU64::new(0),
            seen_in: AtomicU32::new(0),
            seen_out: AtomicU32::new(0),
        })
    }
}

/// Find global channel `channel` in buffers `buffers` (channels per
/// buffer): `(buffer index, channel within buffer, buffer channel count)`.
fn locate(
    buffers: impl Iterator<Item = (usize, usize)>,
    channel: usize,
) -> Option<(usize, usize, usize)> {
    let mut remaining = channel;
    for (index, channels) in buffers {
        if remaining < channels {
            return Some((index, remaining, channels));
        }
        remaining = remaining.checked_sub(channels)?;
    }
    None
}

impl Render for Router {
    fn render(&self, input: &Buffers<'_>, output: &mut BuffersMut<'_>) {
        self.cycles.fetch_add(1, Ordering::Relaxed);
        self.seen_in.store(
            u32::try_from(input.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        self.seen_out.store(
            u32::try_from(output.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );

        // We own this private aggregate's output: start from silence.
        for index in 0..output.len() {
            if let Some(buffer) = output.get_mut(index) {
                buffer.samples.fill(0.0);
            }
        }

        let tap_buffers =
            || (self.skip_in..input.len()).filter_map(|i| input.get(i).map(|b| (i, b.channels)));

        // Meters (pre-gain).
        for (channel, peak) in self.peaks.iter().enumerate() {
            let Some((index, within, channels)) = locate(tap_buffers(), channel) else {
                continue;
            };
            let Some(buffer) = input.get(index) else {
                continue;
            };
            let max = buffer
                .samples
                .iter()
                .skip(within)
                .step_by(channels.max(1))
                .fold(0.0_f32, |m, s| m.max(s.abs()));
            peak.fetch_max(max.to_bits(), Ordering::Relaxed);
        }

        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        if gain <= 0.0 {
            return;
        }
        for route in &*self.routes {
            let Some((in_index, in_ch, in_channels)) = locate(tap_buffers(), route.src) else {
                continue;
            };
            let Some(src) = input.get(in_index) else {
                continue;
            };
            let out_buffers =
                (0..output.len()).filter_map(|i| output.get_mut(i).map(|b| (i, b.channels)));
            let Some((out_index, out_ch, out_channels)) = locate(out_buffers, route.dst) else {
                continue;
            };
            let Some(dst) = output.get_mut(out_index) else {
                continue;
            };
            for (frame_in, frame_out) in src
                .samples
                .chunks_exact(in_channels)
                .zip(dst.samples.chunks_exact_mut(out_channels))
            {
                if let (Some(s), Some(d)) = (frame_in.get(in_ch), frame_out.get_mut(out_ch)) {
                    *d += *s * gain;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::locate;

    #[test]
    fn locate_across_buffers() {
        let layout = [(0, 2), (1, 1), (2, 4)];
        assert_eq!(locate(layout.into_iter(), 0), Some((0, 0, 2)));
        assert_eq!(locate(layout.into_iter(), 2), Some((1, 0, 1)));
        assert_eq!(locate(layout.into_iter(), 6), Some((2, 3, 4)));
        assert_eq!(locate(layout.into_iter(), 7), None);
    }
}
