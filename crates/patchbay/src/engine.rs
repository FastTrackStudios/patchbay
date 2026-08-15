//! The PipeWire main-loop thread.
//!
//! Helvum's engine design, headless: one OS thread owns the PipeWire
//! `MainLoop`; a registry listener mirrors nodes/ports/links into the
//! shared [`GraphStore`] and emits [`GraphEvent`]s; inbound commands
//! (create/destroy link) arrive over `pipewire::channel`, which is the
//! only channel type that can wake the main loop.
//!
//! Links are created via the `link-factory` with `object.linger` so the
//! wiring survives this process — the patchbay edits the system's graph,
//! it doesn't own it.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use parking_lot::RwLock;
use patchbay_proto::GraphEvent;

use crate::store::GraphStore;

/// Commands the service sends into the PipeWire thread.
pub(crate) enum Command {
    /// All four ids resolved by the service from the store (the link
    /// factory wants node AND port ids on both ends).
    CreateLink {
        output_node: u32,
        output_port: u32,
        input_node: u32,
        input_port: u32,
    },
    DestroyLink {
        id: u32,
    },
    /// Create a `support.null-audio-sink` adapter node (a named bus),
    /// tagged `patchbay.virtual` so it's identifiable + destroyable.
    CreateVirtualSink {
        node_name: String,
        description: String,
        channels: u32,
    },
    /// Destroy a node global — the service only sends this for nodes
    /// carrying the `patchbay.virtual` tag.
    DestroyNode {
        id: u32,
    },
    #[allow(dead_code)]
    Terminate,
}

/// Live command sender slot: each (re)connection installs a fresh
/// `pipewire::channel` sender here; `None` while disconnected. The
/// channel type can't outlive a connection (its receiver attaches to
/// that connection's main loop), so reconnects swap it out.
#[cfg(target_os = "linux")]
type CmdSlot = Arc<parking_lot::Mutex<Option<pipewire::channel::Sender<Command>>>>;

#[derive(Clone)]
pub(crate) struct EngineHandle {
    #[cfg(target_os = "linux")]
    cmd_slot: CmdSlot,
}

impl EngineHandle {
    pub fn send(&self, cmd: Command) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            let slot = self.cmd_slot.lock();
            match slot.as_ref() {
                Some(tx) => tx
                    .send(cmd)
                    .map_err(|_| "pipewire engine thread is gone".to_string()),
                None => Err("pipewire is down (engine reconnecting)".to_string()),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cmd;
            Err("pipewire is linux-only".to_string())
        }
    }
}

/// Spawn the engine thread. It (re)connects forever: when PipeWire
/// dies the graph mirror is cleared (`GraphEvent::Reset`) and the
/// thread retries every few seconds — restart PipeWire from the
/// services panel and the graph re-mirrors by itself.
pub(crate) fn spawn(store: Arc<RwLock<GraphStore>>, events: Sender<GraphEvent>) -> EngineHandle {
    #[cfg(target_os = "linux")]
    {
        let cmd_slot: CmdSlot = Arc::new(parking_lot::Mutex::new(None));
        let slot = cmd_slot.clone();
        std::thread::Builder::new()
            .name("patchbay-pw".into())
            .spawn(move || {
                loop {
                    // Fresh mirror for every attempt — a restarted
                    // daemon re-announces everything with new ids.
                    {
                        let mut s = store.write();
                        let empty = s.nodes.is_empty() && s.ports.is_empty();
                        s.nodes.clear();
                        s.ports.clear();
                        s.links.clear();
                        if !empty && events.send(GraphEvent::Reset).is_err() {
                            return;
                        }
                    }
                    match linux::run_connection(&store, &events, &slot) {
                        Ok(()) => tracing::warn!("pipewire connection closed; reconnecting"),
                        Err(e) => tracing::warn!("pipewire connect failed: {e}; retrying"),
                    }
                    *slot.lock() = None;
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
            })
            .expect("spawn patchbay-pw thread");
        EngineHandle { cmd_slot }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (store, events);
        EngineHandle {}
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::mpsc::Sender;

    use parking_lot::RwLock;
    use pipewire::link::{Link, LinkChangeMask, LinkListener, LinkState};
    use pipewire::properties::properties;
    use pipewire::registry::GlobalObject;
    use pipewire::types::ObjectType;
    use pipewire::{context::ContextRc, main_loop::MainLoopRc};

    use patchbay_proto::{GraphEvent, MediaKind, PortDirection, PwLink, PwNode, PwPort};

    use super::Command;
    use crate::store::GraphStore;

    /// Link proxies + their info listeners must stay alive to keep
    /// receiving state changes (helvum's `proxies` map).
    struct LinkProxy {
        _proxy: Link,
        _listener: LinkListener,
    }

    /// One connection lifetime: connect, mirror, serve commands until
    /// the daemon goes away (core error quits the loop). Ok(()) =
    /// connection dropped after being up; Err = couldn't connect.
    pub(super) fn run_connection(
        store: &Arc<RwLock<GraphStore>>,
        events: &Sender<GraphEvent>,
        slot: &super::CmdSlot,
    ) -> Result<(), pipewire::Error> {
        let store = store.clone();
        let events = events.clone();
        let (cmd_tx, cmd_rx) = pipewire::channel::channel::<Command>();

        let mainloop = MainLoopRc::new(None)?;
        let context = ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;

        // Commands become sendable only once the connection is up.
        *slot.lock() = Some(cmd_tx);

        // Core error on id 0 = the connection itself died (daemon
        // stopped/restarted) → leave the loop so the outer reconnect
        // loop takes over.
        let _core_listener = {
            let mainloop = mainloop.clone();
            core.add_listener_local()
                .error(move |id, _seq, res, message| {
                    if id != 0 {
                        return;
                    }
                    // "unknown resource" (-ENOENT) = we dropped a bound
                    // proxy AFTER the server already reclaimed the
                    // object (node/link vanished) — routine, NOT a dead
                    // daemon. Quitting here caused reconnect churn that
                    // corrupted the mirror.
                    if res == -2 || message.contains("unknown resource") {
                        tracing::debug!("pipewire benign core error ({res}): {message}");
                        return;
                    }
                    tracing::warn!("pipewire core error ({res}): {message}");
                    mainloop.quit();
                })
                .register()
        };

        let link_proxies: Rc<RefCell<HashMap<u32, LinkProxy>>> =
            Rc::new(RefCell::new(HashMap::new()));

        // Virtual sinks created on THIS connection whose registry
        // announce may still be in flight — closes the window where a
        // second CreateVirtualSink for the same name would duplicate
        // (ensure_virtual_sinks races the announce otherwise). Pruned
        // on global_remove so a removed name can be re-created.
        let pending_sinks: Rc<RefCell<std::collections::HashSet<String>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));

        // ── Inbound commands ────────────────────────────────────────
        let _cmd_receiver = {
            let core = core.clone();
            let registry = registry.clone();
            let mainloop_quit = mainloop.clone();
            let store_cmd = store.clone();
            let pending_cmd = pending_sinks.clone();
            cmd_rx.attach(mainloop.loop_(), move |cmd| match cmd {
                Command::CreateLink {
                    output_node,
                    output_port,
                    input_node,
                    input_port,
                } => {
                    let res = core.create_object::<Link>(
                        "link-factory",
                        &properties! {
                            "link.output.node" => output_node.to_string(),
                            "link.output.port" => output_port.to_string(),
                            "link.input.node" => input_node.to_string(),
                            "link.input.port" => input_port.to_string(),
                            // Survive this app exiting — the patchbay
                            // edits the system graph, it doesn't own it.
                            "object.linger" => "1",
                        },
                    );
                    if let Err(e) = res {
                        tracing::warn!(output_port, input_port, "link-factory create failed: {e}");
                    }
                }
                Command::DestroyLink { id } | Command::DestroyNode { id } => {
                    registry
                        .destroy_global(id)
                        .into_result()
                        .map(|_| ())
                        .unwrap_or_else(|e| {
                            tracing::warn!(id, "destroy_global failed: {e}");
                        });
                }
                Command::CreateVirtualSink {
                    node_name,
                    description,
                    channels,
                } => {
                    let already_live = store_cmd.read().nodes.values().any(|n| n.name == node_name);
                    if already_live || !pending_cmd.borrow_mut().insert(node_name.clone()) {
                        tracing::debug!(node_name, "virtual sink already exists; skipping");
                        return;
                    }
                    let position = match channels {
                        1 => "[ MONO ]".to_string(),
                        2 => "[ FL FR ]".to_string(),
                        n => {
                            let auxes: Vec<String> = (0..n).map(|i| format!("AUX{i}")).collect();
                            format!("[ {} ]", auxes.join(" "))
                        }
                    };
                    let res = core.create_object::<pipewire::node::Node>(
                        "adapter",
                        &properties! {
                            "factory.name" => "support.null-audio-sink",
                            "node.name" => node_name.as_str(),
                            "node.description" => description.as_str(),
                            "media.class" => "Audio/Sink",
                            "audio.position" => position.as_str(),
                            "monitor.channel-volumes" => "true",
                            "object.linger" => "1",
                            "patchbay.virtual" => "1",
                        },
                    );
                    if let Err(e) = res {
                        tracing::warn!(node_name, "virtual sink create failed: {e}");
                    }
                }
                Command::Terminate => mainloop_quit.quit(),
            })
        };

        // ── Registry listener: mirror the graph ─────────────────────
        let _registry_listener = {
            let store_add = store.clone();
            let events_add = events.clone();
            let registry_bind = registry.clone();
            let proxies_add = link_proxies.clone();
            let store_rm = store.clone();
            let events_rm = events.clone();
            let proxies_rm = link_proxies.clone();
            let pending_rm = pending_sinks.clone();
            registry
                .add_listener_local()
                .global(move |global| match global.type_ {
                    ObjectType::Node => handle_node(global, &store_add, &events_add),
                    ObjectType::Port => handle_port(global, &store_add, &events_add),
                    ObjectType::Link => handle_link(
                        global,
                        &registry_bind,
                        &proxies_add,
                        &store_add,
                        &events_add,
                    ),
                    _ => {}
                })
                .global_remove(move |id| {
                    let removed = {
                        let mut s = store_rm.write();
                        if let Some(n) = s.nodes.remove(&id) {
                            pending_rm.borrow_mut().remove(&n.name);
                            Some(GraphEvent::NodeRemoved { id })
                        } else if let Some(p) = s.ports.remove(&id) {
                            Some(GraphEvent::PortRemoved {
                                id,
                                node_id: p.node_id,
                            })
                        } else if s.links.remove(&id).is_some() {
                            Some(GraphEvent::LinkRemoved { id })
                        } else {
                            None
                        }
                    };
                    proxies_rm.borrow_mut().remove(&id);
                    if let Some(ev) = removed {
                        let _ = events_rm.send(ev);
                    }
                })
                .register()
        };

        mainloop.run();
        Ok(())
    }

    fn node_name_is_virtual(name: &str) -> bool {
        name.starts_with("patchbay.")
    }

    fn media_kind(media_class: &str) -> MediaKind {
        if media_class.contains("Audio") {
            MediaKind::Audio
        } else if media_class.contains("Video") {
            MediaKind::Video
        } else if media_class.contains("Midi") {
            MediaKind::Midi
        } else {
            MediaKind::Other
        }
    }

    /// PwNode from a props dict — used for both the registry global's
    /// prop subset (immediate) and the bound node's full info (refines
    /// with node.group / application.* moments later).
    fn build_node(id: u32, props: &pipewire::spa::utils::dict::DictRef) -> PwNode {
        let name = props.get("node.name").unwrap_or_default().to_string();
        let label = props
            .get("node.nick")
            .or_else(|| props.get("node.description"))
            .or_else(|| props.get("node.name"))
            .unwrap_or_default()
            .to_string();
        let media_class = props.get("media.class").unwrap_or_default().to_string();
        let virtual_sink =
            props.get("patchbay.virtual") == Some("1") || node_name_is_virtual(&name);
        PwNode {
            id,
            name,
            label,
            media_kind: media_kind(&media_class),
            media_class,
            app_name: props
                .get("application.name")
                .unwrap_or_default()
                .to_string(),
            latency: props.get("node.latency").unwrap_or_default().to_string(),
            icon_name: props
                .get("application.icon-name")
                .or_else(|| props.get("application.icon_name"))
                .unwrap_or_default()
                .to_string(),
            group: props.get("node.group").unwrap_or_default().to_string(),
            virtual_sink,
            // Registry globals don't carry live state — filled by the
            // pw-dump state poller (see crate::enrich).
            state: patchbay_proto::NodeState::Unknown,
        }
    }

    fn handle_node(
        global: &GlobalObject<&pipewire::spa::utils::dict::DictRef>,
        store: &Arc<RwLock<GraphStore>>,
        events: &Sender<GraphEvent>,
    ) {
        let Some(props) = global.props else { return };
        // Registry globals carry a SUBSET of node props (no node.group,
        // often no application.*). Do NOT bind node proxies for the
        // rest — held node proxies wedge the registry event stream
        // (verified live: with ~48 bound, new globals stop being
        // announced). Full props arrive out-of-band via the pw-dump
        // enrichment pass (see crate::enrich).
        let node = build_node(global.id, props);
        store.write().nodes.insert(global.id, node.clone());
        let _ = events.send(GraphEvent::NodeAdded(node));
    }

    fn handle_port(
        global: &GlobalObject<&pipewire::spa::utils::dict::DictRef>,
        store: &Arc<RwLock<GraphStore>>,
        events: &Sender<GraphEvent>,
    ) {
        let Some(props) = global.props else { return };
        let Some(node_id) = props.get("node.id").and_then(|s| s.parse::<u32>().ok()) else {
            return;
        };
        let direction = match props.get("port.direction") {
            Some("in") => PortDirection::Input,
            _ => PortDirection::Output,
        };
        // Ports inherit their node's media kind (helvum's approach);
        // `format.dsp` refines MIDI ports on audio nodes.
        let node_kind = store
            .read()
            .nodes
            .get(&node_id)
            .map(|n| n.media_kind)
            .unwrap_or(MediaKind::Other);
        let kind = match props.get("format.dsp") {
            Some(f) if f.contains("midi") => MediaKind::Midi,
            Some(f) if f.contains("audio") => MediaKind::Audio,
            _ => node_kind,
        };
        let port = PwPort {
            id: global.id,
            node_id,
            name: props.get("port.name").unwrap_or_default().to_string(),
            direction,
            media_kind: kind,
        };
        store.write().ports.insert(global.id, port.clone());
        let _ = events.send(GraphEvent::PortAdded(port));
    }

    fn handle_link(
        global: &GlobalObject<&pipewire::spa::utils::dict::DictRef>,
        registry: &pipewire::registry::RegistryRc,
        proxies: &Rc<RefCell<HashMap<u32, LinkProxy>>>,
        store: &Arc<RwLock<GraphStore>>,
        events: &Sender<GraphEvent>,
    ) {
        // Endpoints aren't on the raw global — bind a proxy and wait for
        // the first info event (helvum's pattern). The listener also
        // tracks later active/inactive flips.
        let proxy: Link = match registry.bind(global) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(id = global.id, "bind link failed: {e}");
                return;
            }
        };
        let store = store.clone();
        let events = events.clone();
        let listener = proxy
            .add_listener_local()
            .info(move |info| {
                let id = info.id();
                let active = matches!(info.state(), LinkState::Active);
                let known = store.read().links.contains_key(&id);
                if known {
                    if info.change_mask().contains(LinkChangeMask::STATE) {
                        if let Some(l) = store.write().links.get_mut(&id) {
                            l.active = active;
                        }
                        let _ = events.send(GraphEvent::LinkStateChanged { id, active });
                    }
                } else {
                    let link = PwLink {
                        id,
                        output_node: info.output_node_id(),
                        output_port: info.output_port_id(),
                        input_node: info.input_node_id(),
                        input_port: info.input_port_id(),
                        active,
                    };
                    store.write().links.insert(id, link.clone());
                    let _ = events.send(GraphEvent::LinkAdded(link));
                }
            })
            .register();
        proxies.borrow_mut().insert(
            global.id,
            LinkProxy {
                _proxy: proxy,
                _listener: listener,
            },
        );
    }
}
