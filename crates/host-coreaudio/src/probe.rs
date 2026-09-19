//! Metering for apps that are not in any mix.
//!
//! A mix meters its sources because it already has them in an aggregate.
//! The dashboard needs the same numbers for every app that is playing,
//! including apps nobody has routed anywhere — so this builds the
//! smallest thing that can produce them: one stereo-mixdown tap per app,
//! all of them in a single **tap-only** private aggregate (no subdevices,
//! no output side), and one `IOProc` that computes peaks and writes
//! nothing.
//!
//! Taps here are always unmuted: metering must never change what the
//! user hears. Stealing an app's audio is a mix source's job, where the
//! user asked for it.
//!
//! Costs one tap per app plus one aggregate and one IO thread, so the
//! caller picks which apps are worth it ([`MAX_APPS`]) and rebuilds when
//! that set changes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use patchbay_host::HostError;

use crate::ffi::hal;
use crate::ffi::ioproc::{Buffers, BuffersMut, IoProc, Render};
use crate::ffi::tap::{AggregateDevice, Mute, ProcessTap};

/// How many apps one probe will meter.
///
/// Each costs a tap; past this the dashboard shows levels for the
/// loudest handful rather than paying for every background daemon that
/// happens to hold an audio client open.
pub const MAX_APPS: usize = 16;

/// An app to meter: a stable key the caller recognises (a bundle id) and
/// the pids currently behind it (a browser is many processes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeApp {
    pub key: String,
    pub pids: Vec<i32>,
}

/// One app's peak since the previous read (linear, 0..1+).
#[derive(Debug, Clone, PartialEq)]
pub struct AppPeak {
    pub key: String,
    pub peak: f32,
}

/// Shared with the IO thread; immutable except for the atomics.
struct ProbeRender {
    /// Running peak per tap, `f32` bits (see [`ProbeRender::note`]).
    peaks: Box<[AtomicU32]>,
    cycles: AtomicU64,
}

impl ProbeRender {
    /// Raise tap `index`'s peak to `v`.
    ///
    /// The peaks are non-negative, and for non-negative floats the IEEE
    /// bit pattern orders the same way the value does — so a `fetch_max`
    /// on the bits is a `max` on the values, without a CAS loop.
    fn note(&self, index: usize, v: f32) {
        if let Some(cell) = self.peaks.get(index) {
            cell.fetch_max(v.to_bits(), Ordering::Relaxed);
        }
    }
}

impl Render for ProbeRender {
    fn render(&self, input: &Buffers<'_>, _output: &mut BuffersMut<'_>) {
        // One buffer per tap, in tap order (see `Probe::start`).
        for index in 0..self.peaks.len() {
            let Some(buffer) = input.get(index) else {
                continue;
            };
            let peak = buffer
                .samples
                .iter()
                .fold(0.0_f32, |acc, s| acc.max(s.abs()));
            self.note(index, peak);
        }
        self.cycles.fetch_add(1, Ordering::Relaxed);
    }
}

/// A running meter-only engine over a fixed set of apps.
///
/// Field order is drop order: the IO proc stops before the aggregate
/// goes, which goes before the taps it refers to.
pub struct Probe {
    _io: IoProc<ProbeRender>,
    render: Arc<ProbeRender>,
    _aggregate: AggregateDevice,
    _taps: Vec<ProcessTap>,
    /// App keys in tap order.
    keys: Vec<String>,
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe")
            .field("apps", &self.keys)
            .field("cycles", &self.render.cycles.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Probe {
    /// Start metering `apps`. Apps whose pids have no audio client are
    /// skipped; an empty result is [`HostError::InvalidSpec`] so the
    /// caller doesn't hold a probe that can never report anything.
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] when nothing could be tapped, and
    /// [`HostError::Os`] from the tap, aggregate or `IOProc` calls (a
    /// missing "System Audio Recording" grant is not an error here — the
    /// taps deliver digital silence, which reads as "not playing").
    pub fn start(apps: &[ProbeApp]) -> Result<Self, HostError> {
        let mut taps = Vec::new();
        let mut keys = Vec::new();
        for app in apps.iter().take(MAX_APPS) {
            let objects: Vec<u32> = app
                .pids
                .iter()
                .filter_map(|pid| hal::process_for_pid(*pid).ok().flatten())
                .collect();
            if objects.is_empty() {
                continue;
            }
            // Unmuted, always: see the module docs.
            let name = format!("patchbay meter {}", app.key);
            match ProcessTap::create(&objects, Mute::Unmuted, &name) {
                Ok(tap) => {
                    taps.push(tap);
                    keys.push(app.key.clone());
                }
                Err(e) => tracing::debug!(app = %app.key, "probe: no tap: {e}"),
            }
        }
        if taps.is_empty() {
            return Err(HostError::InvalidSpec(
                "nothing to meter: none of these apps has an audio client".to_owned(),
            ));
        }

        let uid = format!("patchbay.monitor.probe.{}", std::process::id());
        let tap_uids: Vec<&str> = taps.iter().map(ProcessTap::uid).collect();
        // No subdevices: the taps clock it, and there is no output side
        // to render into.
        let aggregate =
            AggregateDevice::create_private_multi("patchbay meters", &uid, &[], &tap_uids)?;

        // One stereo-mixdown tap is one stream, so buffer i is tap i.
        // If the HAL disagrees, say so rather than mislabel meters.
        let buffers = hal::stream_layout(aggregate.id(), true).len();
        if buffers != taps.len() {
            tracing::warn!(
                taps = taps.len(),
                buffers,
                "probe: aggregate buffer count differs from the tap count; meters may be mislabelled"
            );
        }

        let render = Arc::new(ProbeRender {
            peaks: (0..taps.len()).map(|_| AtomicU32::new(0)).collect(),
            cycles: AtomicU64::new(0),
        });
        let mut io = IoProc::create(aggregate.id(), Arc::clone(&render))?;
        io.start()?;
        Ok(Self {
            _io: io,
            render,
            _aggregate: aggregate,
            _taps: taps,
            keys,
        })
    }

    /// Peaks since the previous call, and reset.
    ///
    /// Destructive on purpose: an engine that has stopped clocking (every
    /// tapped app went quiet) then reads as silence rather than holding
    /// its last value on screen forever.
    #[must_use]
    pub fn take_peaks(&self) -> Vec<AppPeak> {
        self.keys
            .iter()
            .enumerate()
            .map(|(i, key)| AppPeak {
                key: key.clone(),
                peak: self
                    .render
                    .peaks
                    .get(i)
                    .map_or(0.0, |c| f32::from_bits(c.swap(0, Ordering::Relaxed))),
            })
            .collect()
    }

    /// The apps this probe covers, in tap order.
    #[must_use]
    pub fn apps(&self) -> &[String] {
        &self.keys
    }

    /// IO cycles since it started — 0 means the HAL never called us
    /// (nothing the probe taps is playing).
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.render.cycles.load(Ordering::Relaxed)
    }
}
