//! Several engines, one UI.
//!
//! Each Patchbay engine serves one machine. This module lets one window
//! work on several: it keeps a connection to every known engine, shows
//! all their devices in the rail's switcher, and makes whichever one you
//! pick the *live* engine — the one every view reads from and writes to.
//!
//! The pieces:
//!
//! * The shell (desktop app, browser remote) provides a [`Shell`]: its
//!   own connection — the **home** engine — and a way to dial others.
//!   That is all a shell does now; the event bridging both shells used
//!   to duplicate lives here, in [`EngineScope`].
//! * The home engine's address book (`hosts()`: saved + discovered) says
//!   which other engines exist. [`use_peers`] dials them and keeps each
//!   one's device list fresh for the switcher.
//! * [`LIVE`] names the engine in use. [`EngineScope`] is keyed on it, so
//!   switching remounts every view against the new connection, and the
//!   global mirrors are [`reset`] first so nothing from the previous
//!   machine is shown as if it were this one's.
//!
//! Engines never talk to each other, and nothing is proxied: a phone
//! talks to THEBATTLESHIP directly, so it has to be able to reach it.

// Connections live on the UI thread and are `Rc` on purpose — a wasm vox
// client isn't `Send` at all — so no future that holds one can be.
#![allow(clippy::future_not_send)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use dioxus::prelude::*;
use futures_util::future::select_ok;
use patchbay_proto::services::patchbay_service::PatchbayServiceStreamClient;
use patchbay_proto::{DeviceEventWire, DeviceSummary, GraphEvent, HostsStatus, PatchbayHost};

use crate::state::{self, PatchbayHandle};

/// Peer poll period, and how long an engine that didn't answer is left
/// alone before the next dial.
const PEER_POLL_SECS: u64 = 5;
const PEER_RETRY_POLLS: u32 = 6;
/// Backstop for anything the event stream dropped.
const RECONCILE_SECS: u64 = 10;

/// A connection to one engine: calls, and the lane its event streams
/// are pumped on.
#[derive(Clone)]
pub struct EngineLink {
    pub handle: PatchbayHandle,
    pub stream: Rc<PatchbayServiceStreamClient>,
}

type DialFuture = Pin<Box<dyn Future<Output = Result<EngineLink, String>>>>;

/// Connects to an engine by URL (`ws://host:4046/vox`). The shell owns
/// the transport, so this crate stays free of it.
#[derive(Clone)]
pub struct Dialer(pub Rc<dyn Fn(String) -> DialFuture>);

/// What a shell provides, via context.
#[derive(Clone)]
pub struct Shell {
    /// The engine this UI belongs to: in-process for the desktop app,
    /// the one that served the page for a browser.
    pub home: EngineLink,
    /// `None` = a shell that can't reach other engines (the switcher then
    /// only ever lists home).
    pub dial: Option<Dialer>,
    /// Home's event stream ended. A browser remote reconnects on this;
    /// the desktop app's in-process link never calls it in practice.
    pub on_home_lost: Callback<()>,
}

/// What is known about one other engine.
#[derive(Clone, Default)]
pub struct Peer {
    pub host: PatchbayHost,
    pub link: Option<EngineLink>,
    pub devices: Vec<DeviceSummary>,
    /// Why it isn't connected (empty while connected or not tried yet).
    pub error: String,
    /// Polls left before it is dialled again.
    cooldown: u32,
}

/// The home engine's address book.
pub static BOOK: GlobalSignal<HostsStatus> = Signal::global(HostsStatus::default);
/// Other engines by `addr`.
pub static PEERS: GlobalSignal<HashMap<String, Peer>> = Signal::global(HashMap::new);
/// Home's own devices — listed in the switcher even while another engine
/// is live (when home IS live, `devices::contexts()` is fresher).
pub static HOME_DEVICES: GlobalSignal<Vec<DeviceSummary>> = Signal::global(Vec::new);
/// `addr` of the engine in use; `None` = home.
pub static LIVE: GlobalSignal<Option<String>> = Signal::global(|| None);
/// A device to select as soon as the engine just switched to has listed
/// its devices (its key, as `devices::device_key`).
pub static PENDING_DEVICE: GlobalSignal<Option<String>> = Signal::global(|| None);

/// The name of the machine in use.
pub fn live_name() -> String {
    let live = LIVE.read().clone();
    live.map_or_else(
        || BOOK.read().this.clone(),
        |addr| {
            let name = PEERS.read().get(&addr).map(|p| p.host.name.clone());
            name.unwrap_or(addr)
        },
    )
}

/// More than one engine is in play — worth saying which one a device is on.
pub fn multi_host() -> bool {
    !BOOK.read().hosts.is_empty()
}

/// Peers in address-book order.
pub fn peers_in_order() -> Vec<Peer> {
    let peers = PEERS.read();
    BOOK.read()
        .hosts
        .iter()
        .filter_map(|h| peers.get(&h.addr).cloned())
        .collect()
}

/// Make `addr`'s engine (`None` = home) the live one, and `device` its
/// current device.
pub fn go_live(addr: Option<String>, device: Option<String>) {
    // A deliberate pick outranks wherever the last session was headed.
    *crate::session::WANT.write() = None;
    if *LIVE.peek() == addr {
        return;
    }
    *PENDING_DEVICE.write() = device;
    *LIVE.write() = addr;
}

fn ws_url(addr: &str) -> String {
    format!("ws://{addr}/vox")
}

/// Dial every address an engine is known by at once; the first to answer
/// wins. A machine on several networks answers on the one this client
/// shares with it, and a client that can't resolve `.local` still gets
/// through on an IP — without waiting out the others' timeouts.
async fn dial(dialer: &Dialer, host: &PatchbayHost) -> Result<EngineLink, String> {
    let mut addrs = vec![host.addr.clone()];
    for a in &host.addrs {
        if !addrs.contains(a) {
            addrs.push(a.clone());
        }
    }
    let attempts = addrs.into_iter().map(|a| (dialer.0)(ws_url(&a)));
    select_ok(attempts)
        .await
        .map(|(link, _)| link)
        .map_err(|e| format!("no answer ({e})"))
}

/// Keeps [`BOOK`] and [`PEERS`] fresh. Call once, above [`EngineScope`],
/// so connections outlive a switch.
pub fn use_peers() {
    let shell = use_context::<Shell>();
    use_future(move || {
        let shell = shell.clone();
        async move {
            loop {
                // The address book is home's, whichever engine is live:
                // it is the list this UI was given, and it must not
                // change under you because you looked at another machine.
                if let Ok(book) = shell.home.handle.0.hosts().await
                    && *BOOK.peek() != book
                {
                    *BOOK.write() = book;
                }
                if let Some(dialer) = shell.dial.as_ref() {
                    poll_peers(dialer).await;
                }
                // Only needed while away from home, and only then worth
                // a call: at home the device poll already has this.
                if LIVE.peek().is_some()
                    && let Ok(devices) = shell.home.handle.0.list_devices().await
                    && *HOME_DEVICES.peek() != devices
                {
                    *HOME_DEVICES.write() = devices;
                }
                state::sleep_secs(PEER_POLL_SECS).await;
            }
        }
    });
}

async fn poll_peers(dialer: &Dialer) {
    let hosts = BOOK.peek().hosts.clone();
    // Forget engines that left the book — unless one is live: it stays
    // until you leave it.
    {
        let live = LIVE.peek().clone();
        let gone: Vec<String> = PEERS
            .peek()
            .keys()
            .filter(|a| !hosts.iter().any(|h| &h.addr == *a) && live.as_ref() != Some(*a))
            .cloned()
            .collect();
        if !gone.is_empty() {
            let mut peers = PEERS.write();
            for a in gone {
                peers.remove(&a);
            }
        }
    }
    for host in hosts {
        let mut peer = PEERS.peek().get(&host.addr).cloned().unwrap_or_default();
        peer.host = host.clone();
        if peer.link.is_none() {
            if peer.cooldown > 0 {
                peer.cooldown = peer.cooldown.saturating_sub(1);
                PEERS.write().insert(host.addr.clone(), peer);
                continue;
            }
            match dial(dialer, &host).await {
                Ok(link) => {
                    peer.link = Some(link);
                    peer.error.clear();
                }
                Err(e) => {
                    peer.error = e;
                    peer.cooldown = PEER_RETRY_POLLS;
                }
            }
        }
        if let Some(link) = peer.link.clone() {
            match link.handle.0.list_devices().await {
                Ok(devices) => peer.devices = devices,
                Err(e) => {
                    peer.link = None;
                    peer.devices.clear();
                    peer.error = format!("connection dropped ({e})");
                    peer.cooldown = 1;
                }
            }
        }
        PEERS.write().insert(host.addr.clone(), peer);
    }
    // The machine the last session was on has connected: go back to it.
    let want = crate::session::WANT.peek().clone();
    if let Some((addr, device)) = want
        && PEERS.peek().get(&addr).is_some_and(|p| p.link.is_some())
    {
        go_live(Some(addr), device);
    }
}

/// Everything mirrored from an engine, emptied — called before another
/// engine goes live, so its first render isn't the last machine's data.
fn reset() {
    state::reset();
    crate::devices::reset();
    crate::now::reset();
    crate::mixes::reset();
    crate::scenes::reset();
    crate::settings::reset();
    crate::dante_grid::reset();
}

/// The link for whichever engine is live, falling back to home when the
/// live peer has dropped.
fn live_link(shell: &Shell) -> (Option<String>, EngineLink) {
    let live = LIVE.read().clone();
    let link = live
        .as_ref()
        .and_then(|addr| PEERS.read().get(addr).and_then(|p| p.link.clone()));
    match (live, link) {
        (Some(addr), Some(link)) => (Some(addr), link),
        _ => (None, shell.home.clone()),
    }
}

/// Mounts `children` against the live engine. Keyed on it: a switch
/// tears the whole tree down and builds it again on the new connection,
/// which is what makes every view's polling and state follow without any
/// of them knowing there is more than one engine.
#[component]
pub fn LiveEngine(children: Element) -> Element {
    let shell = use_context::<Shell>();
    let (addr, link) = live_link(&shell);
    // The live peer dropped: come home rather than sit on a dead link.
    if addr.is_none() && LIVE.peek().is_some() {
        *LIVE.write() = None;
    }
    let key = addr.clone().unwrap_or_else(|| "home".to_owned());
    // A list of one, on purpose: a `key` only means something among list
    // siblings. On a lone child a changed key just updates its props —
    // the scope would keep its hooks, so no reset and, worse, the old
    // engine's handle still in context under the new engine's name.
    rsx! {
        for scope_key in [key] {
            EngineScope {
                key: "{scope_key}",
                addr: addr.clone(),
                link: EngineLinkProp(link.clone()),
                {children.clone()}
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
struct EngineScopeProps {
    addr: Option<String>,
    link: EngineLinkProp,
    children: Element,
}

/// [`EngineLink`] as a prop: equal when it is the same connection.
#[derive(Clone)]
struct EngineLinkProp(EngineLink);

impl PartialEq for EngineLinkProp {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0.handle.0, &other.0.handle.0)
            && Rc::ptr_eq(&self.0.stream, &other.0.stream)
    }
}

#[allow(non_snake_case)]
fn EngineScope(props: EngineScopeProps) -> Element {
    let shell = use_context::<Shell>();
    let link = props.link.0.clone();
    let addr = props.addr.clone();

    // Before anything under this reads a mirror.
    use_hook(reset);
    use_context_provider(|| link.handle.clone());

    // Initial snapshot + graph events → the mirrors. When the stream
    // ends the connection is gone: home tells its shell, a peer hands
    // the UI back to home.
    let graph_link = link.clone();
    let lost_addr = addr;
    use_future(move || {
        let link = graph_link.clone();
        let addr = lost_addr.clone();
        let on_home_lost = shell.on_home_lost;
        async move {
            // Consume the stream through the stream client so the vox
            // lane pumps it (a raw Tx attached to the hub is never
            // drained).
            let (tx, mut rx) = vox::channel::<GraphEvent>();
            let stream = link.stream.clone();
            spawn(async move {
                if let Err(e) = stream.graph_events(tx).await {
                    tracing::warn!("graph_events subscription ended: {e:?}");
                }
            });

            state::refresh_all(&link.handle).await;

            while let Ok(Some(ev)) = rx.recv().await {
                let ev = ev.get();
                state::apply_graph_event(ev);
                // A Reset means the engine rebuilt its mirror (PipeWire
                // restart) — the follow-up flood can overrun any buffer,
                // so reconcile from the snapshot instead of trusting it.
                if matches!(ev, GraphEvent::Reset) {
                    state::refresh_all(&link.handle).await;
                }
            }
            tracing::warn!("graph event stream ended");
            match addr {
                None => on_home_lost.call(()),
                Some(addr) => {
                    if let Some(p) = PEERS.write().get_mut(&addr) {
                        p.link = None;
                        p.devices.clear();
                        "connection dropped".clone_into(&mut p.error);
                    }
                    if LIVE.peek().as_ref() == Some(&addr) {
                        *LIVE.write() = None;
                    }
                }
            }
        }
    });

    // External-device events → the device pages.
    let device_link = link.clone();
    use_future(move || {
        let link = device_link.clone();
        async move {
            let (tx, mut rx) = vox::channel::<DeviceEventWire>();
            let stream = link.stream.clone();
            spawn(async move {
                if let Err(e) = stream.device_events(tx).await {
                    tracing::warn!("device_events subscription ended: {e:?}");
                }
            });
            while let Ok(Some(ev)) = rx.recv().await {
                crate::devices::apply_device_event(ev.get());
            }
        }
    });

    // Belt-and-suspenders reconcile: streams can drop under burst (an
    // app connecting = hundreds of events at once); a periodic snapshot
    // swap guarantees the UI converges within seconds even if the event
    // path lost something.
    use_future(move || {
        let link = link.clone();
        async move {
            loop {
                state::sleep_secs(RECONCILE_SECS).await;
                match link.handle.0.graph().await {
                    Ok(snap) => state::replace_graph(snap),
                    Err(e) => tracing::warn!("graph reconcile failed: {e:?}"),
                }
            }
        }
    });

    rsx! { {props.children} }
}
