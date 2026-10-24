//! Live VU metering.
//!
//! `PipeWire`'s registry has no per-port level, but a sink exposes a
//! `<node>.monitor` source and a hardware source can be recorded
//! directly — so a level meter is a matter of tapping that source and
//! measuring what comes out. `parec` gives us raw PCM on stdout for the
//! cost of one child process per tapped node.
//!
//! That cost is why metering is **opt-in per node**: a client declares
//! which nodes it is currently showing, and the engine keeps exactly
//! that set tapped. Nothing is metered until someone asks.
//!
//! The measurement and the tap bookkeeping are pure functions, tested
//! below; only [`Taps`] touches a process.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::num::NonZeroUsize;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::proto::MeterLevel;
use parking_lot::Mutex;

/// Sample rate we ask `parec` for. Metering doesn't need the graph rate;
/// it needs enough resolution to catch a transient.
const METER_RATE: u32 = 48_000;
/// Channels we tap. Stereo covers every meter the UI draws; a wider bus
/// still reports its first two channels rather than nothing.
pub(crate) const METER_CHANNELS: usize = 2;
/// A tap with no fresh data for this long reads as silence rather than
/// holding its last value — a stalled `parec` must not look like signal.
const STALE_AFTER: Duration = Duration::from_millis(500);

/// Peak level per channel from a raw little-endian `s16` buffer.
///
/// A partial trailing frame is ignored rather than misread: `parec`
/// hands us whatever was ready, and half a frame is not a sample.
#[must_use]
pub(crate) fn peak_s16le(pcm: &[u8], channels: usize) -> Vec<f32> {
    let mut peaks = vec![0.0_f32; channels];
    // A `NonZero` frame size makes the division below total by
    // construction rather than by a guard the compiler can't see.
    let Some(frame_bytes) = NonZeroUsize::new(channels.saturating_mul(2)) else {
        return peaks;
    };
    let usable = (pcm.len() / frame_bytes).saturating_mul(frame_bytes.get());
    let Some(frames) = pcm.get(..usable) else {
        return peaks;
    };
    for frame in frames.chunks_exact(frame_bytes.get()) {
        for (channel, peak) in peaks.iter_mut().enumerate() {
            let lo = channel.saturating_mul(2);
            let (Some(&a), Some(&b)) = (frame.get(lo), frame.get(lo.saturating_add(1))) else {
                continue;
            };
            // i16::MIN has no positive counterpart; saturate so a
            // full-scale negative sample reads as 1.0, not -1.0.
            let sample = f32::from(i16::from_le_bytes([a, b]).saturating_abs());
            let level = (sample / f32::from(i16::MAX)).clamp(0.0, 1.0);
            *peak = peak.max(level);
        }
    }
    peaks
}

/// The source name to record for a node.
///
/// A sink is tapped through its monitor; anything else is recorded
/// directly. This mirrors how `PipeWire` exposes the two cases and is
/// the one place that knows the `.monitor` convention.
#[must_use]
pub(crate) fn source_for(node_name: &str, media_class: &str) -> String {
    if media_class.contains("/Sink") {
        format!("{node_name}.monitor")
    } else {
        node_name.to_owned()
    }
}

/// Which taps to start and which to stop to reach `desired`.
///
/// Pure set arithmetic, separated out so the lifecycle is testable
/// without spawning anything.
#[must_use]
pub(crate) fn diff(
    active: &HashSet<String>,
    desired: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut start: Vec<String> = desired.difference(active).cloned().collect();
    let mut stop: Vec<String> = active.difference(desired).cloned().collect();
    // Deterministic order keeps logs and tests stable.
    start.sort_unstable();
    stop.sort_unstable();
    (start, stop)
}

/// Latest levels from one tap.
struct Snapshot {
    levels: Vec<f32>,
    updated: Instant,
}

/// A running `parec` child plus the thread draining it.
struct Tap {
    child: Child,
    snapshot: Arc<Mutex<Snapshot>>,
}

impl Tap {
    /// Levels, or silence when the tap has gone stale.
    fn levels(&self, now: Instant) -> Vec<f32> {
        let snap = self.snapshot.lock();
        if now.saturating_duration_since(snap.updated) > STALE_AFTER {
            vec![0.0; METER_CHANNELS]
        } else {
            snap.levels.clone()
        }
    }

    fn stop(mut self) {
        // The reader thread ends when the pipe closes, so killing the
        // child is enough; it is detached rather than joined so a wedged
        // `parec` can't block the caller.
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// The set of live meter taps.
#[derive(Default)]
pub(crate) struct Taps {
    taps: HashMap<String, Tap>,
}

impl Taps {
    /// Reconcile the running taps against `desired` (`node → source`).
    pub(crate) fn reconcile(&mut self, desired: &HashMap<String, String>) {
        let active: HashSet<String> = self.taps.keys().cloned().collect();
        let want: HashSet<String> = desired.keys().cloned().collect();
        let (start, stop) = diff(&active, &want);

        for node in stop {
            if let Some(tap) = self.taps.remove(&node) {
                tap.stop();
            }
        }
        for node in start {
            let Some(source) = desired.get(&node) else {
                continue;
            };
            match spawn_tap(source) {
                Ok(tap) => {
                    self.taps.insert(node, tap);
                }
                Err(e) => {
                    // Best-effort: a device that can't be recorded just
                    // reads as silence. Rides the settle/meter span.
                    tracing::debug!(node, source, "meter tap failed: {e}");
                }
            }
        }
    }

    /// Current levels for every live tap.
    pub(crate) fn levels(&self) -> Vec<MeterLevel> {
        let now = Instant::now();
        let mut out: Vec<MeterLevel> = self
            .taps
            .iter()
            .map(|(node, tap)| MeterLevel {
                node_name: node.clone(),
                peak: tap.levels(now),
            })
            .collect();
        out.sort_by(|a, b| a.node_name.cmp(&b.node_name));
        out
    }

    /// How many taps are running.
    pub(crate) fn len(&self) -> usize {
        self.taps.len()
    }
}

impl Drop for Taps {
    fn drop(&mut self) {
        for (_, tap) in self.taps.drain() {
            tap.stop();
        }
    }
}

/// Start `parec` on `source` and drain it on a thread.
fn spawn_tap(source: &str) -> Result<Tap, String> {
    let mut child = Command::new("parec")
        .args([
            "--record",
            "--raw",
            "--device",
            source,
            "--rate",
            &METER_RATE.to_string(),
            "--channels",
            &METER_CHANNELS.to_string(),
            "--format=s16le",
            "--latency-msec=20",
            "--process-time-msec=20",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn parec: {e}"))?;

    let Some(mut stdout) = child.stdout.take() else {
        drop(child.kill());
        return Err("parec exposed no stdout".to_owned());
    };

    let snapshot = Arc::new(Mutex::new(Snapshot {
        levels: vec![0.0; METER_CHANNELS],
        updated: Instant::now(),
    }));
    let writer = Arc::clone(&snapshot);

    let spawned = std::thread::Builder::new()
        .name("patchbay-meter".into())
        .spawn(move || {
            let mut buf = [0_u8; 4096];
            loop {
                let read = match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let Some(pcm) = buf.get(..read) else { break };
                let levels = peak_s16le(pcm, METER_CHANNELS);
                let mut snap = writer.lock();
                snap.levels = levels;
                snap.updated = Instant::now();
            }
        });
    if let Err(e) = spawned {
        drop(child.kill());
        return Err(format!("spawn meter reader: {e}"));
    }

    Ok(Tap { child, snapshot })
}
