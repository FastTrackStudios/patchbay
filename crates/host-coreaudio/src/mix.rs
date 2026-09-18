//! Loopback-style mixes: a [`VirtualDeviceSpec`]'s sources (app taps,
//! the system tap, hardware inputs) summed through channel maps and
//! gains into its monitors (output devices — e.g. the "Patchbay
//! Broadcast" virtual device an app picks as its microphone, or spare
//! playback channels of an interface).
//!
//! ```text
//!  app ─tap─┐                                   ┌─▶ monitor 0 (clock)
//!  app ─tap─┼─▶ private aggregate ─ one IOProc ─┤
//!  input ───┘   (monitors + inputs + taps)      └─▶ monitor 1 (drift comp.)
//! ```
//!
//! The mix bus is flattened at build time: every `source channel → bus
//! channel → monitor channel` path becomes one route scaled by the
//! source's and the monitor's gain, so the IO thread only sums — no
//! scratch buffer, no locks, no allocation. Gains and enables are
//! atomics and change live; changing *which* sources exist rebuilds the
//! mix.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use patchbay_host::{AppInfo, Gain, HostError, SourceKind, VirtualDeviceSpec};
use serde::Serialize;

use crate::ffi::hal::{self, DeviceFacts, ProcessFacts};
use crate::ffi::ioproc::{Buffers, BuffersMut, IoProc, Render};
use crate::ffi::tap::{AggregateDevice, Mute, ProcessTap};

/// State of one source of a running mix.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MixSourceState {
    /// Index into the spec's `sources`.
    pub index: usize,
    /// Human label (`App: com.brave.Browser`, `Input: Galaxy32`, …).
    pub label: String,
    /// Channels the source delivers (0 when inactive).
    pub channels: u32,
    /// Whether it is feeding the mix (an app that isn't running, or a
    /// missing device, is inactive until the mix is rebuilt).
    pub active: bool,
    /// Why it is inactive.
    pub reason: Option<String>,
    /// Captured processes (app sources).
    pub processes: Vec<AppInfo>,
}

/// State of one monitor (output) of a running mix.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MixMonitorState {
    /// Index into the spec's `monitors`.
    pub index: usize,
    /// Output device name.
    pub device: String,
    /// Output device uid.
    pub device_uid: String,
    /// Whether this monitor clocks the mix (the first one).
    pub clock: bool,
}

/// What a running [`Mix`] is made of.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MixInfo {
    /// The spec's name.
    pub name: String,
    /// Sources, in spec order.
    pub sources: Vec<MixSourceState>,
    /// Monitors, in spec order.
    pub monitors: Vec<MixMonitorState>,
    /// Private aggregate uid.
    pub aggregate_uid: String,
    /// Clock rate (the first monitor's nominal rate).
    pub sample_rate: Option<f64>,
    /// The aggregate's input buffers as the HAL reports them (channels
    /// per buffer) — and as planned.
    pub input_layout: Vec<u32>,
    pub planned_input_layout: Vec<usize>,
    /// The aggregate's output buffers (channels per buffer), and as planned.
    pub output_layout: Vec<u32>,
    pub planned_output_layout: Vec<usize>,
}

/// A running mix; torn down on drop (IO stopped, aggregate destroyed,
/// taps destroyed — field order is drop order).
pub struct Mix {
    _io: IoProc<MixRender>,
    render: Arc<MixRender>,
    _aggregate: AggregateDevice,
    _taps: Vec<ProcessTap>,
    info: MixInfo,
}

impl std::fmt::Debug for Mix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mix")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Where one channel lives in an `IOProc` buffer list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    buffer: usize,
    channel: usize,
    /// Channels interleaved in that buffer.
    stride: usize,
}

/// Flat per-buffer channel layout → slots.
#[derive(Debug, Clone, Default)]
struct Layout {
    buffers: Vec<usize>,
}

impl Layout {
    /// Append `channels_per_buffer`, returning the first buffer index.
    fn push(&mut self, channels_per_buffer: &[u32]) -> usize {
        let first = self.buffers.len();
        self.buffers.extend(
            channels_per_buffer
                .iter()
                .map(|c| usize::try_from(*c).unwrap_or(0)),
        );
        first
    }

    /// Channel `channel` of the group whose buffers start at `first` and
    /// span `count` buffers.
    fn slot(&self, first: usize, count: usize, channel: usize) -> Option<Slot> {
        let mut remaining = channel;
        for (offset, stride) in self
            .buffers
            .get(first..first.checked_add(count)?)?
            .iter()
            .enumerate()
        {
            if remaining < *stride {
                return Some(Slot {
                    buffer: first.checked_add(offset)?,
                    channel: remaining,
                    stride: *stride,
                });
            }
            remaining = remaining.checked_sub(*stride)?;
        }
        None
    }
}

/// One `source channel → monitor channel` path.
#[derive(Debug, Clone, Copy)]
struct Route {
    src: Slot,
    dst: Slot,
    source: usize,
    monitor: usize,
}

/// A source's input buffers, for metering.
#[derive(Debug, Clone)]
struct SourceMeter {
    slots: Vec<Slot>,
    /// Offset of its first channel in `MixRender::source_peaks`.
    first_peak: usize,
}

/// Shared with the IO thread; immutable except for atomics.
struct MixRender {
    routes: Box<[Route]>,
    /// `f32` bits of each source's linear gain.
    source_gain: Box<[AtomicU32]>,
    source_on: Box<[AtomicBool]>,
    monitor_gain: Box<[AtomicU32]>,
    monitor_on: Box<[AtomicBool]>,
    meters: Box<[SourceMeter]>,
    /// Running peak per source channel (pre-gain), `f32` bits.
    source_peaks: Box<[AtomicU32]>,
    /// Output slots per monitor channel, for post-mix metering.
    monitor_slots: Box<[Vec<Slot>]>,
    monitor_peaks: Box<[Box<[AtomicU32]>]>,
    cycles: AtomicU64,
}

impl MixRender {
    fn new(
        spec: &VirtualDeviceSpec,
        routes: Vec<Route>,
        meters: Vec<SourceMeter>,
        peak_count: usize,
        monitor_slots: Vec<Vec<Slot>>,
    ) -> Self {
        Self {
            routes: routes.into_boxed_slice(),
            source_gain: spec
                .sources
                .iter()
                .map(|s| AtomicU32::new(s.volume.to_bits()))
                .collect(),
            source_on: spec
                .sources
                .iter()
                .map(|s| AtomicBool::new(s.enabled))
                .collect(),
            monitor_gain: spec
                .monitors
                .iter()
                .map(|m| AtomicU32::new(m.volume.to_bits()))
                .collect(),
            monitor_on: spec
                .monitors
                .iter()
                .map(|m| AtomicBool::new(m.enabled))
                .collect(),
            meters: meters.into_boxed_slice(),
            source_peaks: (0..peak_count).map(|_| AtomicU32::new(0)).collect(),
            monitor_peaks: monitor_slots
                .iter()
                .map(|s| s.iter().map(|_| AtomicU32::new(0)).collect())
                .collect(),
            monitor_slots: monitor_slots.into_boxed_slice(),
            cycles: AtomicU64::new(0),
        }
    }
}

fn peak_of(samples: &[f32], slot: Slot) -> f32 {
    samples
        .iter()
        .skip(slot.channel)
        .step_by(slot.stride.max(1))
        .fold(0.0_f32, |m, s| m.max(s.abs()))
}

impl Render for MixRender {
    fn render(&self, input: &Buffers<'_>, output: &mut BuffersMut<'_>) {
        self.cycles.fetch_add(1, Ordering::Relaxed);
        // We own this private aggregate's output side: start from silence
        // (other clients of the same hardware are mixed by the HAL).
        for index in 0..output.len() {
            if let Some(buffer) = output.get_mut(index) {
                buffer.samples.fill(0.0);
            }
        }
        for meter in &*self.meters {
            for (i, slot) in meter.slots.iter().enumerate() {
                let (Some(buffer), Some(peak)) = (
                    input.get(slot.buffer),
                    meter
                        .first_peak
                        .checked_add(i)
                        .and_then(|p| self.source_peaks.get(p)),
                ) else {
                    continue;
                };
                peak.fetch_max(peak_of(buffer.samples, *slot).to_bits(), Ordering::Relaxed);
            }
        }
        for route in &*self.routes {
            let on = self
                .source_on
                .get(route.source)
                .is_some_and(|b| b.load(Ordering::Relaxed))
                && self
                    .monitor_on
                    .get(route.monitor)
                    .is_some_and(|b| b.load(Ordering::Relaxed));
            if !on {
                continue;
            }
            let gain = self
                .source_gain
                .get(route.source)
                .map_or(0.0, |g| f32::from_bits(g.load(Ordering::Relaxed)))
                * self
                    .monitor_gain
                    .get(route.monitor)
                    .map_or(0.0, |g| f32::from_bits(g.load(Ordering::Relaxed)));
            if gain <= 0.0 {
                continue;
            }
            let (Some(src), Some(dst)) = (
                input.get(route.src.buffer),
                output.get_mut(route.dst.buffer),
            ) else {
                continue;
            };
            for (frame_in, frame_out) in src
                .samples
                .chunks_exact(route.src.stride.max(1))
                .zip(dst.samples.chunks_exact_mut(route.dst.stride.max(1)))
            {
                if let (Some(s), Some(d)) = (
                    frame_in.get(route.src.channel),
                    frame_out.get_mut(route.dst.channel),
                ) {
                    *d += *s * gain;
                }
            }
        }
        for (slots, peaks) in self.monitor_slots.iter().zip(&*self.monitor_peaks) {
            for (slot, peak) in slots.iter().zip(&**peaks) {
                if let Some(buffer) = output.get_mut(slot.buffer) {
                    peak.fetch_max(peak_of(buffer.samples, *slot).to_bits(), Ordering::Relaxed);
                }
            }
        }
    }
}

/// A resolved source before the aggregate exists.
enum Planned {
    /// One or more taps whose buffers form the source, in order.
    Tap {
        taps: Vec<ProcessTap>,
        /// Channels per tap buffer.
        layout: Vec<u32>,
        processes: Vec<ProcessFacts>,
    },
    Input {
        device: DeviceFacts,
    },
    Inactive {
        reason: String,
    },
}

fn source_label(kind: &SourceKind) -> String {
    match kind {
        SourceKind::App { app } => format!("App: {app:?}"),
        SourceKind::AppOnDevice { app, device_uid } => format!("App: {app:?} → {device_uid}"),
        SourceKind::InputDevice { uid } => format!("Input: {uid}"),
        SourceKind::SystemAudio => "System audio".to_owned(),
        SourceKind::PassThru => "Pass-thru".to_owned(),
    }
}

fn own_process_object() -> Option<u32> {
    i32::try_from(std::process::id())
        .ok()
        .and_then(|pid| hal::process_for_pid(pid).ok().flatten())
}

fn not_running() -> Planned {
    Planned::Inactive {
        reason: "app is not running (or hasn't opened audio yet)".to_owned(),
    }
}

/// Audio client processes of `app` (helpers included), never ourselves.
fn app_processes(app: &patchbay_host::AppSelector) -> Result<Vec<ProcessFacts>, HostError> {
    let own = i32::try_from(std::process::id()).unwrap_or(-1);
    let processes: Vec<ProcessFacts> = match app {
        patchbay_host::AppSelector::Pid(pid) => hal::process_for_pid(*pid)?
            .and_then(|o| hal::process(o).ok())
            .into_iter()
            .collect(),
        patchbay_host::AppSelector::BundleId(b) => hal::process_ids()?
            .into_iter()
            .filter_map(|o| hal::process(o).ok())
            .filter(|p| {
                p.bundle_id
                    .as_deref()
                    .is_some_and(|pb| patchbay_host::bundle_matches(b, pb))
            })
            .collect(),
    };
    Ok(processes.into_iter().filter(|p| p.pid != own).collect())
}

fn plan_source(kind: &SourceKind, name: &str) -> Result<Planned, HostError> {
    Ok(match kind {
        SourceKind::App { app } => {
            let processes = app_processes(app)?;
            if processes.is_empty() {
                return Ok(not_running());
            }
            let objects: Vec<u32> = processes.iter().map(|p| p.object).collect();
            let tap = ProcessTap::create(&objects, Mute::Unmuted, name)?;
            let format = hal::summarize(&tap.format()?);
            Planned::Tap {
                taps: vec![tap],
                layout: vec![format.channels],
                processes,
            }
        }
        SourceKind::AppOnDevice { app, device_uid } => {
            let processes = app_processes(app)?;
            if processes.is_empty() {
                return Ok(not_running());
            }
            let Some(device) = hal::device_by_uid(device_uid)? else {
                return Ok(Planned::Inactive {
                    reason: format!("output device `{device_uid}` not present"),
                });
            };
            let objects: Vec<u32> = processes.iter().map(|p| p.object).collect();
            // One tap per output stream, so channel N of the source is
            // channel N of the device.
            let streams = hal::stream_layout(device.object, false);
            let mut taps = Vec::with_capacity(streams.len());
            let mut layout = Vec::with_capacity(streams.len());
            for index in 0..streams.len() {
                let tap = ProcessTap::create_device_stream(
                    &objects,
                    device_uid,
                    index,
                    Mute::Unmuted,
                    name,
                )?;
                layout.push(hal::summarize(&tap.format()?).channels);
                taps.push(tap);
            }
            Planned::Tap {
                taps,
                layout,
                processes,
            }
        }
        SourceKind::SystemAudio => {
            // Everything but ourselves — never feed our own output back in.
            let excluded: Vec<u32> = own_process_object().into_iter().collect();
            let tap = ProcessTap::create_global(&excluded, Mute::Unmuted, name)?;
            let format = hal::summarize(&tap.format()?);
            Planned::Tap {
                taps: vec![tap],
                layout: vec![format.channels],
                processes: Vec::new(),
            }
        }
        SourceKind::InputDevice { uid } => match hal::device_by_uid(uid)? {
            Some(device) if device.input_channels > 0 => Planned::Input { device },
            Some(device) => Planned::Inactive {
                reason: format!("`{}` has no input channels", device.name),
            },
            None => Planned::Inactive {
                reason: format!("input device `{uid}` not present"),
            },
        },
        SourceKind::PassThru => {
            return Err(HostError::Unsupported(
                "pass-thru sources need the Patchbay virtual device as a source: add it as an \
                 input device instead"
                    .to_owned(),
            ));
        }
    })
}

/// Output devices of `spec`'s monitors, deduplicated in order (the first
/// clocks the mix), and each monitor's index into them.
fn resolve_monitors(spec: &VirtualDeviceSpec) -> Result<(Vec<DeviceFacts>, Vec<usize>), HostError> {
    let mut devices: Vec<DeviceFacts> = Vec::new();
    let mut monitor_device = Vec::with_capacity(spec.monitors.len());
    for m in &spec.monitors {
        let d = hal::device_by_uid(&m.device_uid)?
            .ok_or_else(|| HostError::NotFound(format!("output device `{}`", m.device_uid)))?;
        if d.output_channels == 0 {
            return Err(HostError::InvalidSpec(format!(
                "`{}` has no output channels",
                d.name
            )));
        }
        m.channel_map
            .validate(Some(spec.channels), Some(d.output_channels))?;
        let existing = devices.iter().position(|x| x.uid == d.uid);
        let idx = existing.unwrap_or_else(|| {
            devices.push(d);
            devices.len().saturating_sub(1)
        });
        monitor_device.push(idx);
    }
    Ok((devices, monitor_device))
}

/// A source's input buffers: `(first buffer, buffer count, channels)`.
type Group = Option<(usize, usize, u32)>;

/// The aggregate's buffer layout, resolved per source and monitor.
struct Plan {
    ins: Layout,
    outs: Layout,
    /// Per source (spec order).
    sources: Vec<Group>,
    /// Per monitor (spec order): `(first buffer, buffer count)`.
    monitors: Vec<Option<(usize, usize)>>,
}

fn plan_layout(devices: &[DeviceFacts], monitor_device: &[usize], planned: &[Planned]) -> Plan {
    let mut ins = Layout::default();
    let mut outs = Layout::default();
    let mut dev_in = Vec::with_capacity(devices.len());
    let mut dev_out = Vec::with_capacity(devices.len());
    for d in devices {
        let l_in = hal::stream_layout(d.object, true);
        let l_out = hal::stream_layout(d.object, false);
        dev_in.push((ins.push(&l_in), l_in.len()));
        dev_out.push((outs.push(&l_out), l_out.len()));
    }
    let sources = planned
        .iter()
        .map(|p| match p {
            Planned::Tap { layout, .. } => Some((
                ins.push(layout),
                layout.len(),
                layout.iter().fold(0_u32, |a, c| a.saturating_add(*c)),
            )),
            Planned::Input { device } => devices
                .iter()
                .position(|d| d.uid == device.uid)
                .and_then(|i| dev_in.get(i))
                .map(|(first, count)| (*first, *count, device.input_channels)),
            Planned::Inactive { .. } => None,
        })
        .collect();
    let monitors = monitor_device
        .iter()
        .map(|d| dev_out.get(*d).copied())
        .collect();
    Plan {
        ins,
        outs,
        sources,
        monitors,
    }
}

fn to_index(c: u32) -> usize {
    usize::try_from(c).unwrap_or(usize::MAX)
}

/// Flatten `source → bus → monitor` into direct routes.
fn flatten_routes(spec: &VirtualDeviceSpec, plan: &Plan) -> Vec<Route> {
    let mut routes = Vec::new();
    for (si, (source, group)) in spec.sources.iter().zip(&plan.sources).enumerate() {
        let Some((first, count, _)) = group else {
            continue;
        };
        for sp in source.channel_map.pairs() {
            let Some(src) = plan.ins.slot(*first, *count, to_index(sp.src)) else {
                continue;
            };
            for (mi, (monitor, out)) in spec.monitors.iter().zip(&plan.monitors).enumerate() {
                let Some((dfirst, dcount)) = out else {
                    continue;
                };
                for mp in monitor
                    .channel_map
                    .pairs()
                    .iter()
                    .filter(|mp| mp.src == sp.dst)
                {
                    if let Some(dst) = plan.outs.slot(*dfirst, *dcount, to_index(mp.dst)) {
                        routes.push(Route {
                            src,
                            dst,
                            source: si,
                            monitor: mi,
                        });
                    }
                }
            }
        }
    }
    routes
}

/// Source meters (every delivered channel) and the peak slot count.
fn source_meters(plan: &Plan) -> (Vec<SourceMeter>, usize) {
    let mut meters = Vec::with_capacity(plan.sources.len());
    let mut peaks = 0_usize;
    for group in &plan.sources {
        let slots: Vec<Slot> = group
            .map(|(first, count, channels)| {
                (0..usize::try_from(channels).unwrap_or(0))
                    .filter_map(|c| plan.ins.slot(first, count, c))
                    .collect()
            })
            .unwrap_or_default();
        let n = slots.len();
        meters.push(SourceMeter {
            first_peak: peaks,
            slots,
        });
        peaks = peaks.saturating_add(n);
    }
    (meters, peaks)
}

/// Output slots per monitor channel (spec order).
fn monitor_slots(spec: &VirtualDeviceSpec, plan: &Plan) -> Vec<Vec<Slot>> {
    spec.monitors
        .iter()
        .zip(&plan.monitors)
        .map(|(m, out)| {
            let Some((first, count)) = out else {
                return Vec::new();
            };
            m.channel_map
                .pairs()
                .iter()
                .filter_map(|p| plan.outs.slot(*first, *count, to_index(p.dst)))
                .collect()
        })
        .collect()
}

fn source_states(spec: &VirtualDeviceSpec, planned: &[Planned]) -> Vec<MixSourceState> {
    spec.sources
        .iter()
        .zip(planned)
        .enumerate()
        .map(|(index, (s, p))| {
            let label = source_label(&s.kind);
            match p {
                Planned::Tap {
                    layout, processes, ..
                } => MixSourceState {
                    index,
                    label,
                    channels: layout.iter().fold(0_u32, |a, c| a.saturating_add(*c)),
                    active: true,
                    reason: None,
                    processes: processes
                        .iter()
                        .map(|p| AppInfo {
                            pid: p.pid,
                            bundle_id: p.bundle_id.clone(),
                            name: p.name.clone(),
                        })
                        .collect(),
                },
                Planned::Input { device } => MixSourceState {
                    index,
                    label: format!("Input: {}", device.name),
                    channels: device.input_channels,
                    active: true,
                    reason: None,
                    processes: Vec::new(),
                },
                Planned::Inactive { reason } => MixSourceState {
                    index,
                    label,
                    channels: 0,
                    active: false,
                    reason: Some(reason.clone()),
                    processes: Vec::new(),
                },
            }
        })
        .collect()
}

fn monitor_states(
    spec: &VirtualDeviceSpec,
    devices: &[DeviceFacts],
    monitor_device: &[usize],
) -> Vec<MixMonitorState> {
    spec.monitors
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let d = monitor_device.get(index).and_then(|i| devices.get(*i));
            MixMonitorState {
                index,
                device: d.map_or_else(|| m.device_uid.clone(), |d| d.name.clone()),
                device_uid: m.device_uid.clone(),
                clock: monitor_device.get(index) == Some(&0),
            }
        })
        .collect()
}

impl Mix {
    /// Build and start a mix. Blocking (tap creation may wait on a TCC
    /// prompt); run it off any UI thread.
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] (no monitors, bad maps or gains),
    /// [`HostError::NotFound`] (a monitor device), [`HostError::Os`].
    pub fn start(spec: &VirtualDeviceSpec) -> Result<Self, HostError> {
        spec.validate()?;
        if spec.monitors.is_empty() {
            return Err(HostError::InvalidSpec(format!(
                "mix `{}` has no outputs (monitors)",
                spec.name
            )));
        }
        let (mut devices, monitor_device) = resolve_monitors(spec)?;

        // Sources: taps and input devices (inputs join the subdevices).
        let tap_name = format!("patchbay mix {}", spec.name);
        let planned = spec
            .sources
            .iter()
            .map(|s| plan_source(&s.kind, &tap_name))
            .collect::<Result<Vec<_>, _>>()?;
        for p in &planned {
            if let Planned::Input { device } = p
                && !devices.iter().any(|d| d.uid == device.uid)
            {
                devices.push(device.clone());
            }
        }

        let sub_uids: Vec<&str> = devices.iter().map(|d| d.uid.as_str()).collect();
        let tap_uids: Vec<&str> = planned
            .iter()
            .flat_map(|p| match p {
                Planned::Tap { taps, .. } => taps.iter().map(ProcessTap::uid).collect(),
                _ => Vec::new(),
            })
            .collect();
        let aggregate_uid = format!(
            "patchbay.mix.{}.{}",
            tap_uids.first().copied().unwrap_or("none"),
            std::process::id()
        );
        let aggregate =
            AggregateDevice::create_private_multi(&tap_name, &aggregate_uid, &sub_uids, &tap_uids)?;

        let plan = plan_layout(&devices, &monitor_device, &planned);
        let input_layout = hal::stream_layout(aggregate.id(), true);
        let output_layout = hal::stream_layout(aggregate.id(), false);
        let (seen_in, seen_out) = (input_layout.len(), output_layout.len());
        if seen_in != plan.ins.buffers.len() || seen_out != plan.outs.buffers.len() {
            tracing::warn!(
                expected_in = plan.ins.buffers.len(),
                seen_in,
                expected_out = plan.outs.buffers.len(),
                seen_out,
                "mix: aggregate buffer layout differs from the plan; routes may be off"
            );
        }
        let routes = flatten_routes(spec, &plan);
        let (meters, peak_count) = source_meters(&plan);
        let monitor_slots = monitor_slots(spec, &plan);

        let render = Arc::new(MixRender::new(
            spec,
            routes,
            meters,
            peak_count,
            monitor_slots,
        ));
        let mut io = IoProc::create(aggregate.id(), Arc::clone(&render))?;
        io.start()?;

        let info = MixInfo {
            name: spec.name.clone(),
            sources: source_states(spec, &planned),
            monitors: monitor_states(spec, &devices, &monitor_device),
            aggregate_uid,
            sample_rate: devices.first().and_then(|d| d.sample_rate),
            input_layout,
            planned_input_layout: plan.ins.buffers.clone(),
            output_layout,
            planned_output_layout: plan.outs.buffers,
        };
        let taps = planned
            .into_iter()
            .flat_map(|p| match p {
                Planned::Tap { taps, .. } => taps,
                _ => Vec::new(),
            })
            .collect();
        Ok(Self {
            _io: io,
            render,
            _aggregate: aggregate,
            _taps: taps,
            info,
        })
    }

    /// What the mix is made of.
    #[must_use]
    pub const fn info(&self) -> &MixInfo {
        &self.info
    }

    /// Set a source's gain and enable (live).
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] for a bad index or gain.
    pub fn set_source(&self, index: usize, volume: f32, enabled: bool) -> Result<(), HostError> {
        let g = Gain::validate("source", volume)?;
        let (Some(gain), Some(on)) = (
            self.render.source_gain.get(index),
            self.render.source_on.get(index),
        ) else {
            return Err(HostError::InvalidSpec(format!("no source {index}")));
        };
        gain.store(g.to_bits(), Ordering::Relaxed);
        on.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    /// Set a monitor's gain and enable (live).
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] for a bad index or gain.
    pub fn set_monitor(&self, index: usize, volume: f32, enabled: bool) -> Result<(), HostError> {
        let g = Gain::validate("monitor", volume)?;
        let (Some(gain), Some(on)) = (
            self.render.monitor_gain.get(index),
            self.render.monitor_on.get(index),
        ) else {
            return Err(HostError::InvalidSpec(format!("no monitor {index}")));
        };
        gain.store(g.to_bits(), Ordering::Relaxed);
        on.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    /// Per-source, per-channel peaks (linear, pre-gain) since last call.
    #[must_use]
    pub fn take_source_peaks(&self) -> Vec<Vec<f32>> {
        self.render
            .meters
            .iter()
            .map(|m| {
                (0..m.slots.len())
                    .filter_map(|i| {
                        let p = self.render.source_peaks.get(m.first_peak.checked_add(i)?)?;
                        Some(f32::from_bits(p.swap(0, Ordering::Relaxed)))
                    })
                    .collect()
            })
            .collect()
    }

    /// Per-monitor, per-channel peaks (linear, post-mix) since last call.
    #[must_use]
    pub fn take_monitor_peaks(&self) -> Vec<Vec<f32>> {
        self.render
            .monitor_peaks
            .iter()
            .map(|ps| {
                ps.iter()
                    .map(|p| f32::from_bits(p.swap(0, Ordering::Relaxed)))
                    .collect()
            })
            .collect()
    }

    /// IO cycles rendered so far.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.render.cycles.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::{Layout, Slot};

    #[test]
    fn layout_slots_span_buffers() {
        let mut l = Layout::default();
        let a = l.push(&[2, 1]); // device: 2ch + 1ch buffers
        let b = l.push(&[2]); // tap
        assert_eq!((a, b), (0, 2));
        assert_eq!(
            l.slot(a, 2, 0),
            Some(Slot {
                buffer: 0,
                channel: 0,
                stride: 2
            })
        );
        assert_eq!(
            l.slot(a, 2, 2),
            Some(Slot {
                buffer: 1,
                channel: 0,
                stride: 1
            })
        );
        assert_eq!(l.slot(a, 2, 3), None);
        assert_eq!(
            l.slot(b, 1, 1),
            Some(Slot {
                buffer: 2,
                channel: 1,
                stride: 2
            })
        );
    }
}

/// Audio client processes right now (for pickers and for noticing apps
/// that start or quit). Never includes this process.
#[must_use]
pub fn audio_processes() -> Vec<(AppInfo, bool)> {
    let own = i32::try_from(std::process::id()).unwrap_or(-1);
    hal::process_ids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| hal::process(o).ok())
        .filter(|p| p.pid != own)
        .map(|p| {
            (
                AppInfo {
                    pid: p.pid,
                    bundle_id: p.bundle_id,
                    name: p.name,
                },
                p.running_output,
            )
        })
        .collect()
}

/// A device as mixes see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MixDevice {
    pub uid: String,
    pub name: String,
    pub input_channels: u32,
    pub output_channels: u32,
}

/// Audio devices right now, private aggregates excluded.
#[must_use]
pub fn audio_devices() -> Vec<MixDevice> {
    hal::device_ids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| hal::device(o).ok())
        .filter(|d| !d.uid.starts_with("patchbay."))
        .map(|d| MixDevice {
            uid: d.uid,
            name: d.name,
            input_channels: d.input_channels,
            output_channels: d.output_channels,
        })
        .collect()
}
