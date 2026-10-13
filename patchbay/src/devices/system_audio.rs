//! "System Audio": the machine's own audio system as a read-only device.
//!
//! Why a [`DeviceAdapter`] and not a separate `host` RPC section: what
//! the device layer needs today is *presence and inspection* — the entry
//! in `patchbay device list`, `device show/params --json`, the Devices
//! tab, events and failure isolation — and the adapter path gives all of
//! that with zero new wire surface. The host **graph** (many-to-many
//! mixing links) does not fit router semantics, so no routing is mapped:
//! the adapter has no port groups and `set_route` is `Unsupported`, the
//! same shape as the Yamaha TF over RCP. When host links land
//! (`docs/host-backends.md` milestone 3) they get their own RPC section
//! over `patchbay-host` types; this entry stays the summary view.
//!
//! Sources:
//! - **macOS**: `patchbay-host-coreaudio`'s `CoreAudioBackend` (device +
//!   process enumeration, HAL listeners → `SnapshotReplaced`).
//! - **Linux**: the existing `PipeWire` engine's graph mirror, read only
//!   (polled; engine behaviour is untouched).
//!
//! Params (all read-only): `backend`, `summary/{devices,apps,playing,
//! recording}`, `default/{output,input}`, `device/<key>/{name,kind,
//! inputs,outputs,sample_rate,transport,manufacturer,running}`,
//! `app/<key>/{name,bundle_id,pid,playing,recording}`. Keys are the
//! Core Audio device UID / process pid, or the `PipeWire` `node.name`
//! (`/` replaced by `_`).

// The `PipeWire` source is the non-macOS path (and is unit-tested
// everywhere); on a macOS build it is otherwise unused.
#![cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use patchbay_device::{
    ChannelRef, DeviceAdapter, DeviceError, DeviceEvent, DeviceId, DeviceInfo, DeviceSnapshot,
    Param, ParamKind, ParamValue, Transport, WriteGuard,
};
use patchbay_host::{
    AppInfo, DynHostBackend, HostCapabilities, HostNode, HostSnapshot, NodeDirection, NodeKind,
    PortCounts,
};
use patchbay_proto::{MediaKind, NodeState, PortDirection};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::store::GraphStore;

/// How often the `PipeWire` source re-checks the graph for changes.
const GRAPH_POLL: Duration = Duration::from_secs(5);
/// Coalescing window for bursts of host events.
const HOST_DEBOUNCE: Duration = Duration::from_millis(250);

type Defaults = Box<dyn Fn(bool) -> Option<String> + Send + Sync>;

enum Source {
    /// A `HostBackend` (Core Audio) plus its default-device lookup.
    Host {
        backend: Arc<dyn DynHostBackend>,
        defaults: Defaults,
    },
    /// The `PipeWire` engine's mirror.
    Graph(Arc<RwLock<GraphStore>>),
}

/// The machine's audio system, read-only.
pub(crate) struct SystemAudioAdapter {
    info: DeviceInfo,
    source: Source,
    events: broadcast::Sender<DeviceEvent>,
    watcher: Option<JoinHandle<()>>,
}

impl Drop for SystemAudioAdapter {
    fn drop(&mut self) {
        if let Some(t) = &self.watcher {
            t.abort();
        }
    }
}

fn info(name: &str, backend: &str, vendor: &str, model: &str, via: &str) -> DeviceInfo {
    DeviceInfo {
        id: DeviceId::from_parts("host", backend, name),
        vendor: vendor.to_owned(),
        model: model.to_owned(),
        serial: None,
        firmware: None,
        transport: Transport::Other {
            description: via.to_owned(),
        },
        online: true,
    }
}

impl SystemAudioAdapter {
    /// Core Audio (macOS). Enumeration is a few fast HAL calls.
    #[cfg(target_os = "macos")]
    pub(crate) fn coreaudio(name: &str) -> Result<Self, String> {
        let backend = Arc::new(
            patchbay_host_coreaudio::CoreAudioBackend::new()
                .map_err(|e| format!("core audio: {e}"))?,
        );
        let lookup = Arc::clone(&backend);
        let defaults: Defaults = Box::new(move |output| lookup.default_device(output));
        let backend: Arc<dyn DynHostBackend> = backend;
        let (events, _) = broadcast::channel(256);
        let watcher = tokio::runtime::Handle::try_current()
            .ok()
            .map(|rt| rt.spawn(forward_host_events(backend.subscribe(), events.clone())));
        Ok(Self {
            info: info(
                name,
                "coreaudio",
                "Apple",
                "Core Audio",
                "Core Audio HAL (read-only: devices, app streams)",
            ),
            source: Source::Host { backend, defaults },
            events,
            watcher,
        })
    }

    /// The `PipeWire` engine's graph (Linux), read-only.
    pub(crate) fn pipewire(name: &str, graph: Arc<RwLock<GraphStore>>) -> Self {
        let (events, _) = broadcast::channel(256);
        let watcher = tokio::runtime::Handle::try_current()
            .ok()
            .map(|rt| rt.spawn(poll_graph(Arc::clone(&graph), events.clone())));
        Self {
            info: info(
                name,
                "pipewire",
                "PipeWire",
                "PipeWire",
                "PipeWire engine graph (read-only: devices, app streams)",
            ),
            source: Source::Graph(graph),
            events,
            watcher,
        }
    }

    async fn host_snapshot(
        &self,
    ) -> Result<(HostSnapshot, Option<String>, Option<String>), DeviceError> {
        match &self.source {
            Source::Host { backend, defaults } => {
                let snap = backend
                    .snapshot()
                    .await
                    .map_err(|e| DeviceError::Transport(e.to_string()))?;
                Ok((snap, defaults(true), defaults(false)))
            }
            Source::Graph(graph) => Ok((graph_snapshot(&graph.read()), None, None)),
        }
    }
}

async fn forward_host_events(
    mut rx: broadcast::Receiver<patchbay_host::HostEvent>,
    tx: broadcast::Sender<DeviceEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                // Coalesce the burst (a hot-plug fires several events).
                tokio::time::sleep(HOST_DEBOUNCE).await;
                while rx.try_recv().is_ok() {}
                let _ = tx.send(DeviceEvent::SnapshotReplaced);
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// What changes in the graph that the summary shows.
fn graph_fingerprint(g: &GraphStore) -> Vec<(u32, String, NodeState)> {
    let mut v: Vec<(u32, String, NodeState)> = g
        .nodes
        .values()
        .filter(|n| n.media_kind == MediaKind::Audio)
        .map(|n| (n.id, n.name.clone(), n.state))
        .collect();
    v.sort_by_key(|(id, _, _)| *id);
    v
}

async fn poll_graph(graph: Arc<RwLock<GraphStore>>, tx: broadcast::Sender<DeviceEvent>) {
    let mut last = graph_fingerprint(&graph.read());
    loop {
        tokio::time::sleep(GRAPH_POLL).await;
        let now = graph_fingerprint(&graph.read());
        if now != last {
            last = now;
            let _ = tx.send(DeviceEvent::SnapshotReplaced);
        }
    }
}

/// The `PipeWire` mirror as a host snapshot (audio nodes only).
pub(crate) fn graph_snapshot(g: &GraphStore) -> HostSnapshot {
    let mut counts: BTreeMap<u32, PortCounts> = BTreeMap::new();
    for p in g
        .ports
        .values()
        .filter(|p| p.media_kind == MediaKind::Audio)
    {
        let c = counts.entry(p.node_id).or_default();
        match p.direction {
            PortDirection::Input => c.inputs = c.inputs.saturating_add(1),
            PortDirection::Output => c.outputs = c.outputs.saturating_add(1),
        }
    }
    let mut nodes: Vec<HostNode> = g
        .nodes
        .values()
        .filter(|n| n.media_kind == MediaKind::Audio || n.media_class.contains("Audio"))
        .map(|n| {
            let ports = counts.get(&n.id).copied().unwrap_or_default();
            let stream = n.media_class.starts_with("Stream/");
            let kind = if stream {
                NodeKind::AppStream
            } else if n.virtual_sink || n.media_class.contains("Virtual") {
                NodeKind::VirtualDevice
            } else {
                NodeKind::HardwareDevice
            };
            let running = n.state == NodeState::Running;
            let mut props = BTreeMap::new();
            props.insert("pipewire.media_class".to_owned(), n.media_class.clone());
            props.insert("running".to_owned(), running.to_string());
            if stream {
                let playing = running && n.media_class.starts_with("Stream/Output");
                let recording = running && n.media_class.starts_with("Stream/Input");
                props.insert("running_output".to_owned(), playing.to_string());
                props.insert("running_input".to_owned(), recording.to_string());
            } else {
                props.insert("running_somewhere".to_owned(), running.to_string());
            }
            HostNode {
                id: format!("pipewire:{}", n.name),
                name: if n.label.is_empty() {
                    n.name.clone()
                } else {
                    n.label.clone()
                },
                kind,
                direction: NodeDirection::from_counts(ports),
                ports,
                sample_rate: None,
                app: stream.then(|| AppInfo {
                    pid: 0,
                    bundle_id: None,
                    name: if n.app_name.is_empty() {
                        n.label.clone()
                    } else {
                        n.app_name.clone()
                    },
                }),
                props,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    HostSnapshot {
        backend: "pipewire".to_owned(),
        capabilities: HostCapabilities::default(),
        nodes,
        links: Vec::new(),
    }
}

const fn ro(path: String, label: String, kind: ParamKind, value: ParamValue) -> Param {
    Param {
        path,
        label,
        kind,
        value,
        writable: false,
        disruptive: false,
    }
}

const fn text(path: String, label: String, value: String) -> Param {
    ro(path, label, ParamKind::Text, ParamValue::Text(value))
}

const fn int(path: String, label: String, value: i64) -> Param {
    ro(
        path,
        label,
        ParamKind::Int {
            min: 0,
            max: i64::MAX,
        },
        ParamValue::Int(value),
    )
}

const fn toggle(path: String, label: String, value: bool) -> Param {
    ro(path, label, ParamKind::Toggle, ParamValue::Toggle(value))
}

/// A node prop that is `"true"` under either the backend's or the
/// generic key.
fn flag(n: &HostNode, key: &str) -> bool {
    [format!("coreaudio.{key}"), key.to_owned()]
        .iter()
        .any(|k| n.props.get(k).is_some_and(|v| v == "true"))
}

fn prop<'a>(n: &'a HostNode, key: &str) -> Option<&'a str> {
    n.props
        .get(&format!("coreaudio.{key}"))
        .or_else(|| n.props.get(key))
        .map(String::as_str)
}

/// Param-path key of a node: the id without its backend prefix.
fn node_key(n: &HostNode) -> String {
    let raw =
        n.id.strip_prefix("coreaudio:device:")
            .or_else(|| n.id.strip_prefix("coreaudio:process:"))
            .or_else(|| n.id.strip_prefix("pipewire:"))
            .unwrap_or(&n.id);
    raw.replace('/', "_")
}

const fn kind_str(k: NodeKind) -> &'static str {
    match k {
        NodeKind::HardwareDevice => "hardware",
        NodeKind::VirtualDevice => "virtual",
        NodeKind::AppStream => "app",
        NodeKind::Aggregate => "aggregate",
    }
}

/// Rate in whole Hz (Core Audio reports `f64`).
fn rate_hz(r: f64) -> Option<i64> {
    format!("{r:.0}").parse().ok()
}

fn device_params(n: &HostNode, out: &mut Vec<Param>) {
    let key = node_key(n);
    let p = |leaf: &str| format!("device/{key}/{leaf}");
    let l = |what: &str| format!("{} · {what}", n.name);
    out.push(text(p("name"), l("name"), n.name.clone()));
    out.push(text(p("kind"), l("kind"), kind_str(n.kind).to_owned()));
    out.push(int(
        p("inputs"),
        l("playback channels"),
        i64::from(n.ports.inputs),
    ));
    out.push(int(
        p("outputs"),
        l("capture channels"),
        i64::from(n.ports.outputs),
    ));
    if let Some(hz) = n.sample_rate.and_then(rate_hz) {
        out.push(int(p("sample_rate"), l("sample rate (Hz)"), hz));
    }
    if let Some(t) = prop(n, "transport") {
        out.push(text(p("transport"), l("transport"), t.trim().to_owned()));
    }
    if let Some(m) = prop(n, "manufacturer") {
        out.push(text(p("manufacturer"), l("manufacturer"), m.to_owned()));
    }
    out.push(toggle(
        p("running"),
        l("running"),
        flag(n, "running_somewhere"),
    ));
}

fn app_params(n: &HostNode, out: &mut Vec<Param>) {
    let key = node_key(n);
    let p = |leaf: &str| format!("app/{key}/{leaf}");
    let name = n.app.as_ref().map_or(n.name.as_str(), |a| a.name.as_str());
    let l = |what: &str| format!("{name} · {what}");
    out.push(text(p("name"), l("name"), name.to_owned()));
    if let Some(a) = &n.app {
        if let Some(b) = &a.bundle_id {
            out.push(text(p("bundle_id"), l("bundle id"), b.clone()));
        }
        if a.pid > 0 {
            out.push(int(p("pid"), l("pid"), i64::from(a.pid)));
        }
    }
    out.push(toggle(
        p("playing"),
        l("playing"),
        flag(n, "running_output"),
    ));
    out.push(toggle(
        p("recording"),
        l("recording"),
        flag(n, "running_input"),
    ));
}

/// The read-only params of a host snapshot.
pub(crate) fn params(
    snap: &HostSnapshot,
    default_output: Option<&str>,
    default_input: Option<&str>,
) -> Vec<Param> {
    let devices: Vec<&HostNode> = snap
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::AppStream)
        .collect();
    let apps: Vec<&HostNode> = snap
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::AppStream)
        .collect();
    let names = |pred: &dyn Fn(&HostNode) -> bool| {
        let mut v: Vec<&str> = apps
            .iter()
            .filter(|n| pred(n))
            .map(|n| n.app.as_ref().map_or(n.name.as_str(), |a| a.name.as_str()))
            .collect();
        v.sort_unstable();
        v.dedup();
        v.join(", ")
    };
    let default_name = |id: Option<&str>| {
        id.and_then(|id| snap.node(id))
            .map(|n| n.name.clone())
            .unwrap_or_default()
    };
    let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    let mut out = vec![
        text("backend".into(), "Backend".into(), snap.backend.clone()),
        int(
            "summary/devices".into(),
            "Devices".into(),
            count(devices.len()),
        ),
        int(
            "summary/apps".into(),
            "Audio apps".into(),
            count(apps.len()),
        ),
        text(
            "summary/playing".into(),
            "Apps playing".into(),
            names(&|n| flag(n, "running_output")),
        ),
        text(
            "summary/recording".into(),
            "Apps recording".into(),
            names(&|n| flag(n, "running_input")),
        ),
        text(
            "default/output".into(),
            "Default output".into(),
            default_name(default_output),
        ),
        text(
            "default/input".into(),
            "Default input".into(),
            default_name(default_input),
        ),
    ];
    for n in devices {
        device_params(n, &mut out);
    }
    for n in apps {
        app_params(n, &mut out);
    }
    out
}

impl DeviceAdapter for SystemAudioAdapter {
    fn info(&self) -> DeviceInfo {
        self.info.clone()
    }

    async fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        let (snap, out, inp) = self.host_snapshot().await?;
        Ok(DeviceSnapshot {
            info: self.info.clone(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            params: params(&snap, out.as_deref(), inp.as_deref()),
        })
    }

    async fn apply_param(
        &self,
        path: &str,
        _value: ParamValue,
        _guard: WriteGuard,
    ) -> Result<(), DeviceError> {
        // Read-only by design: patchbay never changes system defaults.
        Err(DeviceError::ReadOnly(path.to_owned()))
    }

    async fn set_route(
        &self,
        _output: ChannelRef,
        _source: Option<ChannelRef>,
    ) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported(
            "system audio routing is a mixing graph, not a router; host links are not exposed yet"
                .to_owned(),
        ))
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use patchbay_proto::{PwNode, PwPort};

    use super::*;

    fn pw_node(id: u32, name: &str, class: &str, app: &str, state: NodeState) -> PwNode {
        PwNode {
            id,
            name: name.into(),
            label: name.into(),
            media_class: class.into(),
            media_kind: MediaKind::Audio,
            app_name: app.into(),
            latency: String::new(),
            icon_name: String::new(),
            group: String::new(),
            virtual_sink: false,
            state,
        }
    }

    fn pw_port(id: u32, node_id: u32, direction: PortDirection) -> PwPort {
        PwPort {
            id,
            node_id,
            name: format!("p{id}"),
            direction,
            media_kind: MediaKind::Audio,
        }
    }

    #[test]
    fn pipewire_graph_maps_devices_and_streams() {
        let mut g = GraphStore::default();
        g.nodes.insert(
            1,
            pw_node(1, "alsa_output.usb", "Audio/Sink", "", NodeState::Running),
        );
        g.nodes.insert(
            2,
            pw_node(
                2,
                "firefox",
                "Stream/Output/Audio",
                "Firefox",
                NodeState::Running,
            ),
        );
        g.nodes.insert(
            3,
            pw_node(3, "obs", "Stream/Input/Audio", "OBS", NodeState::Idle),
        );
        for (id, node, dir) in [
            (10, 1, PortDirection::Input),
            (11, 1, PortDirection::Input),
            (12, 2, PortDirection::Output),
        ] {
            g.ports.insert(id, pw_port(id, node, dir));
        }
        let snap = graph_snapshot(&g);
        assert_eq!(snap.nodes.len(), 3);
        let ps = params(&snap, None, None);
        let get = |path: &str| {
            ps.iter()
                .find(|p| p.path == path)
                .map_or_else(|| panic!("missing {path}"), |p| p.value.clone())
        };
        assert_eq!(get("backend"), ParamValue::Text("pipewire".into()));
        assert_eq!(get("summary/devices"), ParamValue::Int(1));
        assert_eq!(get("summary/apps"), ParamValue::Int(2));
        assert_eq!(get("summary/playing"), ParamValue::Text("Firefox".into()));
        assert_eq!(get("summary/recording"), ParamValue::Text(String::new()));
        assert_eq!(get("device/alsa_output.usb/inputs"), ParamValue::Int(2));
        assert_eq!(
            get("device/alsa_output.usb/running"),
            ParamValue::Toggle(true)
        );
        assert_eq!(get("app/firefox/playing"), ParamValue::Toggle(true));
        assert!(ps.iter().all(|p| !p.writable));
    }

    #[test]
    fn coreaudio_nodes_map() {
        let mut dev_props = BTreeMap::new();
        dev_props.insert("coreaudio.transport".to_owned(), "usb ".to_owned());
        dev_props.insert("coreaudio.running_somewhere".to_owned(), "true".to_owned());
        let mut app_props = BTreeMap::new();
        app_props.insert("coreaudio.running_output".to_owned(), "true".to_owned());
        let snap = HostSnapshot {
            backend: "coreaudio".into(),
            capabilities: HostCapabilities::default(),
            nodes: vec![
                HostNode {
                    id: "coreaudio:device:AppleUSB/1".into(),
                    name: "Galaxy32".into(),
                    kind: NodeKind::HardwareDevice,
                    direction: NodeDirection::Duplex,
                    ports: PortCounts {
                        inputs: 64,
                        outputs: 64,
                    },
                    sample_rate: Some(48_000.0),
                    app: None,
                    props: dev_props,
                },
                HostNode {
                    id: "coreaudio:process:42".into(),
                    name: "Music".into(),
                    kind: NodeKind::AppStream,
                    direction: NodeDirection::Source,
                    ports: PortCounts {
                        inputs: 0,
                        outputs: 2,
                    },
                    sample_rate: None,
                    app: Some(AppInfo {
                        pid: 42,
                        bundle_id: Some("com.apple.Music".into()),
                        name: "Music".into(),
                    }),
                    props: app_props,
                },
            ],
            links: Vec::new(),
        };
        let ps = params(&snap, Some("coreaudio:device:AppleUSB/1"), None);
        let get = |path: &str| ps.iter().find(|p| p.path == path).map(|p| p.value.clone());
        assert_eq!(
            get("default/output"),
            Some(ParamValue::Text("Galaxy32".into()))
        );
        assert_eq!(
            get("device/AppleUSB_1/sample_rate"),
            Some(ParamValue::Int(48_000))
        );
        assert_eq!(
            get("device/AppleUSB_1/transport"),
            Some(ParamValue::Text("usb".into()))
        );
        assert_eq!(
            get("app/42/bundle_id"),
            Some(ParamValue::Text("com.apple.Music".into()))
        );
        assert_eq!(get("app/42/playing"), Some(ParamValue::Toggle(true)));
        assert_eq!(
            get("summary/playing"),
            Some(ParamValue::Text("Music".into()))
        );
    }

    #[tokio::test]
    async fn writes_are_refused() {
        let a = SystemAudioAdapter::pipewire("system-audio", Arc::default());
        assert_eq!(a.info().id.as_str(), "host:pipewire:system-audio");
        assert!(matches!(
            a.set_param("backend", ParamValue::Text("x".into())).await,
            Err(DeviceError::ReadOnly(_))
        ));
        assert!(matches!(
            a.set_route(ChannelRef::new("x", 0), None).await,
            Err(DeviceError::Unsupported(_))
        ));
        let s = a.snapshot().await.expect("snapshot");
        assert!(s.outputs.is_empty() && s.routes.is_empty());
    }
}
