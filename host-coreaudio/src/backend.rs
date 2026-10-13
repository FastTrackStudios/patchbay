//! [`CoreAudioBackend`]: enumeration + change notifications
//! (milestone 1), behind [`patchbay_host::HostBackend`].
//!
//! Threads:
//! - HAL notification threads run the listener callbacks, which only
//!   push a [`Change`] into an mpsc channel (never block the HAL).
//! - One `patchbay-ca-watch` worker debounces changes, re-enumerates,
//!   diffs against the cached node list and broadcasts [`HostEvent`]s. It
//!   also owns the per-object listeners (sample rate / stream layout /
//!   alive per device, running state per process) and reconciles them
//!   after every topology change.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;
use patchbay_host::{
    HostBackend, HostCapabilities, HostError, HostEvent, HostLink, HostNode, HostSnapshot, PortRef,
    VirtualDeviceSpec, VolumeTarget, diff_nodes,
};
use tokio::sync::broadcast;

use crate::enumerate;
use crate::ffi::hal::{self, selector};
use crate::ffi::listener::PropertyListener;
use crate::ffi::property::Scope;
use crate::ffi::{ObjectId, SYSTEM_OBJECT};

const EVENT_CAPACITY: usize = 256;
/// Coalesce bursts (a hot-plug fires several properties at once).
const DEBOUNCE: Duration = Duration::from_millis(40);

/// What a listener saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// Device or process list changed, or an object's properties did.
    Topology,
    /// Default output (`true`) / input device changed.
    Default(bool),
    /// Backend dropped.
    Shutdown,
}

struct Shared {
    events: broadcast::Sender<HostEvent>,
    nodes: Mutex<Vec<HostNode>>,
}

/// The macOS Core Audio host backend.
///
/// Milestone 1 (done): devices + audio-client processes as
/// [`HostNode`]s, live [`HostEvent`]s from HAL property listeners.
/// Links, virtual devices and volumes return [`HostError::Unsupported`]
/// for now — app capture is available directly via
/// [`crate::TapMonitor`].
pub struct CoreAudioBackend {
    // Dropped explicitly first in `Drop`.
    system_listeners: Vec<PropertyListener>,
    tx: mpsc::Sender<Change>,
    worker: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for CoreAudioBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreAudioBackend")
            .field("nodes", &self.shared.nodes.lock().len())
            .finish_non_exhaustive()
    }
}

fn notify(tx: &mpsc::Sender<Change>, change: Change) -> Box<dyn Fn(ObjectId, u32) + Send + Sync> {
    let tx = tx.clone();
    // `Sender::send` on an unbounded channel never blocks the HAL thread.
    Box::new(move |_, _| {
        let _ = tx.send(change);
    })
}

impl CoreAudioBackend {
    /// Enumerate once and start listening.
    ///
    /// # Errors
    /// [`HostError::Os`] if the HAL can't be queried or listened to.
    pub fn new() -> Result<Self, HostError> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let shared = Arc::new(Shared {
            events,
            nodes: Mutex::new(enumerate::nodes()?),
        });
        let (tx, rx) = mpsc::channel();

        let mut system_listeners = vec![
            PropertyListener::add(
                SYSTEM_OBJECT,
                selector::DEVICES,
                Scope::Global,
                notify(&tx, Change::Topology),
            )?,
            PropertyListener::add(
                SYSTEM_OBJECT,
                selector::DEFAULT_OUTPUT,
                Scope::Global,
                notify(&tx, Change::Default(true)),
            )?,
            PropertyListener::add(
                SYSTEM_OBJECT,
                selector::DEFAULT_INPUT,
                Scope::Global,
                notify(&tx, Change::Default(false)),
            )?,
        ];
        // The process list only exists on macOS 14.2+.
        if hal::has_property(SYSTEM_OBJECT, selector::PROCESSES) {
            system_listeners.push(PropertyListener::add(
                SYSTEM_OBJECT,
                selector::PROCESSES,
                Scope::Global,
                notify(&tx, Change::Topology),
            )?);
        }

        let worker_shared = Arc::clone(&shared);
        let worker_tx = tx.clone();
        let worker = std::thread::Builder::new()
            .name("patchbay-ca-watch".to_owned())
            .spawn(move || watch(&worker_shared, &worker_tx, &rx))
            .map_err(|e| HostError::Os {
                op: "spawn watcher".to_owned(),
                status: e.to_string(),
            })?;

        Ok(Self {
            system_listeners,
            tx,
            worker: Some(worker),
            shared,
        })
    }

    /// Node id of the current default output (`true`) or input device.
    #[must_use]
    pub fn default_device(&self, output: bool) -> Option<String> {
        enumerate::default_device_id(output)
    }
}

impl Drop for CoreAudioBackend {
    fn drop(&mut self) {
        // Stop HAL callbacks first, then the worker (which drops its own
        // per-object listeners on exit).
        self.system_listeners.clear();
        let _ = self.tx.send(Change::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Per-object listeners, keyed by HAL object id.
type Watches = HashMap<ObjectId, Vec<PropertyListener>>;

fn reconcile(watches: &mut Watches, tx: &mpsc::Sender<Change>) {
    let devices = hal::device_ids().unwrap_or_default();
    let processes = hal::process_ids().unwrap_or_default();
    let wanted: Vec<(ObjectId, &[u32])> = devices
        .iter()
        .map(|d| {
            (
                *d,
                &[
                    selector::NOMINAL_SAMPLE_RATE,
                    selector::STREAM_CONFIGURATION,
                    selector::DEVICE_IS_ALIVE,
                ][..],
            )
        })
        .chain(processes.iter().map(|p| {
            (
                *p,
                &[
                    selector::PROCESS_IS_RUNNING,
                    selector::PROCESS_IS_RUNNING_OUTPUT,
                ][..],
            )
        }))
        .collect();
    watches.retain(|object, _| wanted.iter().any(|(o, _)| o == object));
    for (object, selectors) in wanted {
        watches.entry(object).or_insert_with(|| {
            selectors
                .iter()
                .filter_map(|sel| {
                    PropertyListener::add(
                        object,
                        *sel,
                        Scope::Wildcard,
                        notify(tx, Change::Topology),
                    )
                    .ok()
                })
                .collect()
        });
    }
}

fn watch(shared: &Shared, tx: &mpsc::Sender<Change>, rx: &mpsc::Receiver<Change>) {
    let mut watches = Watches::new();
    reconcile(&mut watches, tx);
    while let Ok(first) = rx.recv() {
        let mut changes = vec![first];
        while let Ok(more) = rx.recv_timeout(DEBOUNCE) {
            changes.push(more);
        }
        if changes.contains(&Change::Shutdown) {
            break;
        }
        if changes.contains(&Change::Topology) {
            match enumerate::nodes() {
                Ok(fresh) => {
                    let events = {
                        let mut cached = shared.nodes.lock();
                        let events = diff_nodes(&cached, &fresh);
                        *cached = fresh;
                        events
                    };
                    for event in events {
                        let _ = shared.events.send(event);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "re-enumeration failed");
                    let _ = shared.events.send(HostEvent::SnapshotReplaced);
                }
            }
            reconcile(&mut watches, tx);
        }
        for output in [true, false] {
            if changes.contains(&Change::Default(output)) {
                let id = enumerate::default_device_id(output);
                let _ = shared
                    .events
                    .send(HostEvent::DefaultDeviceChanged { output, id });
            }
        }
    }
    drop(watches);
}

fn not_yet(what: &str) -> HostError {
    HostError::Unsupported(format!(
        "{what} on Core Audio (milestone 3, see docs/host-backends.md)"
    ))
}

impl HostBackend for CoreAudioBackend {
    fn name(&self) -> &'static str {
        "coreaudio"
    }

    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            graph_links: false,
            virtual_devices: false,
            // Via `TapMonitor` (process taps, macOS 14.2+).
            app_capture: true,
            // Needs the HAL AudioServerPlugIn (not built yet).
            pass_thru_device: false,
            meters: false,
        }
    }

    async fn snapshot(&self) -> Result<HostSnapshot, HostError> {
        Ok(HostSnapshot {
            backend: "coreaudio".to_owned(),
            capabilities: HostBackend::capabilities(self),
            nodes: enumerate::nodes()?,
            links: Vec::new(),
        })
    }

    async fn create_link(&self, _link: HostLink) -> Result<(), HostError> {
        Err(not_yet("graph links"))
    }

    async fn remove_link(&self, _from: PortRef, _to: PortRef) -> Result<(), HostError> {
        Err(not_yet("graph links"))
    }

    async fn create_virtual_device(&self, spec: VirtualDeviceSpec) -> Result<String, HostError> {
        spec.validate()?;
        if spec.needs_pass_thru() {
            return Err(HostError::Unsupported(
                "pass-thru sources need the patchbay HAL plug-in".to_owned(),
            ));
        }
        Err(not_yet("virtual devices"))
    }

    async fn remove_virtual_device(&self, id: &str) -> Result<(), HostError> {
        Err(HostError::NotFound(format!("virtual device `{id}`")))
    }

    async fn set_volume(&self, _target: VolumeTarget, _gain: f32) -> Result<(), HostError> {
        Err(not_yet("volume control"))
    }

    fn subscribe(&self) -> broadcast::Receiver<HostEvent> {
        self.shared.events.subscribe()
    }
}
