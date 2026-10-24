//! Graph-settle detection.
//!
//! Virtual sinks and named routes can only be applied against a graph
//! that has finished announcing itself. The engine used to approximate
//! that with `thread::sleep(3s)` after a reconnect and hope — which is a
//! race, not a synchronization: on a loaded machine the mirror is still
//! filling at 3 s, the apply runs against a half-built graph, silently
//! creates nothing, and never retries.
//!
//! This is the honest version: watch the event stream and act once it
//! goes quiet. `now` is a parameter rather than read from the clock
//! inside, so the whole thing is testable without sleeping.

use std::time::{Duration, Instant};

/// How long the graph must be quiet before we call it settled.
pub(crate) const QUIET_FOR: Duration = Duration::from_millis(750);

/// Upper bound on a burst. A graph that never goes quiet (a node
/// flapping, a device re-announcing in a loop) would otherwise starve
/// the settle action forever, so force one through at this point.
pub(crate) const MAX_BURST: Duration = Duration::from_secs(15);

/// What a completed burst looked like — folded onto the settle span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Burst {
    /// Graph events seen since the burst began.
    pub events: u32,
    /// First event to quiet.
    pub duration: Duration,
    /// The burst was cut short by [`MAX_BURST`] rather than going quiet.
    pub forced: bool,
}

/// Tracks whether the graph has gone quiet long enough to act on.
///
/// Arms on the first event, fires once when quiet, then disarms until
/// the next event. A quiet graph costs nothing.
#[derive(Debug)]
pub(crate) struct Settle {
    quiet_for: Duration,
    max_burst: Duration,
    /// When the current burst started; `None` when disarmed.
    started: Option<Instant>,
    /// When the most recent event arrived.
    last_event: Option<Instant>,
    events: u32,
}

impl Settle {
    pub(crate) const fn new(quiet_for: Duration, max_burst: Duration) -> Self {
        Self {
            quiet_for,
            max_burst,
            started: None,
            last_event: None,
            events: 0,
        }
    }

    /// Record a graph-changing event.
    pub(crate) const fn observe(&mut self, now: Instant) {
        if self.started.is_none() {
            self.started = Some(now);
            self.events = 0;
        }
        self.last_event = Some(now);
        self.events = self.events.saturating_add(1);
    }

    /// Is a settle action due? Returns the burst exactly once per burst,
    /// then disarms.
    pub(crate) fn take_settled(&mut self, now: Instant) -> Option<Burst> {
        let (started, last) = (self.started?, self.last_event?);
        let quiet = now.saturating_duration_since(last) >= self.quiet_for;
        let overdue = now.saturating_duration_since(started) >= self.max_burst;
        if !quiet && !overdue {
            return None;
        }
        let burst = Burst {
            events: self.events,
            duration: now.saturating_duration_since(started),
            // Only call it forced when it genuinely hasn't gone quiet.
            forced: overdue && !quiet,
        };
        self.started = None;
        self.last_event = None;
        self.events = 0;
        Some(burst)
    }

    /// Is a burst currently in progress?
    #[cfg(test)]
    pub(crate) const fn armed(&self) -> bool {
        self.started.is_some()
    }
}

impl Default for Settle {
    fn default() -> Self {
        Self::new(QUIET_FOR, MAX_BURST)
    }
}
