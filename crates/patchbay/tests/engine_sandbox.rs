//! End-to-end tests against a private `PipeWire` graph.
//!
//! The unit tests cover planning — what patchbay decides to do. These
//! cover the half a pure function can't: that the engine thread
//! connects, that the registry mirror actually fills, that a command
//! reaches the daemon, and that the settle path wires a rig without
//! anyone poking it.
//!
//! All of it runs in a sandbox (see `common::Sandbox`), never the host's
//! session. On a machine with no `pipewire` these skip; set
//! `PATCHBAY_REQUIRE_SANDBOX=1` to make that a failure instead.
//!
//! One test function, not several: the sandbox redirects the whole
//! process via an environment variable, so tests sharing a binary must
//! not run concurrently against different graphs.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
// One test function on purpose (see the module docs): the sandbox
// redirects the whole process, so this cannot be split into parallel
// tests without them racing each other's graph.
#![allow(clippy::too_many_lines)]

mod common;

use std::time::Duration;

use common::{Sandbox, eventually};
use patchbay::PatchbayBackend;
use patchbay_proto::{NamedRoute, PatchbayService, RouteEndpoint, VirtualSink, sink_node_name};

/// Generous, because it covers daemon round-trips plus patchbay's own
/// settle window — not a guess at how long a burst takes.
const SETTLE: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread")]
async fn engine_drives_a_real_graph() {
    let Some(sandbox) = Sandbox::start() else {
        // No pipewire on this host; unit tests still cover the logic.
        return;
    };
    // SAFETY: nothing patchbay-owned has started yet — the backend is
    // constructed below, after this returns.
    unsafe { sandbox.redirect_this_process() };

    let backend = PatchbayBackend::new();

    // ── The mirror fills ────────────────────────────────────────────
    eventually("the engine to connect and mirror the graph", SETTLE, || {
        futures_lite_block(backend.graph()).is_ok()
    });

    // ── A virtual sink becomes a real node with real ports ──────────
    let bus = "Sandbox Bus";
    backend
        .add_virtual_sink(VirtualSink {
            name: bus.to_owned(),
            channels: 2,
            capturable: false,
        })
        .await
        .expect("add_virtual_sink");

    let node_name = sink_node_name(bus);
    await_patchable(&backend, &node_name);

    // ── A second bus, so there is something to route INTO ───────────
    let sink = "Sandbox Sink";
    backend
        .add_virtual_sink(VirtualSink {
            name: sink.to_owned(),
            channels: 2,
            capturable: false,
        })
        .await
        .expect("add second sink");
    let sink_node = sink_node_name(sink);
    await_patchable(&backend, &sink_node);

    // ── Links create and destroy through the engine ─────────────────
    let (out_port, in_port) = {
        let g = backend.graph().await.expect("graph");
        let src = g
            .nodes
            .iter()
            .find(|n| n.name == node_name)
            .expect("bus node");
        let dst = g
            .nodes
            .iter()
            .find(|n| n.name == sink_node)
            .expect("sink node");
        let out = g
            .ports
            .iter()
            .find(|p| p.node_id == src.id && p.name.starts_with("monitor_"))
            .expect("bus monitor port");
        let inp = g
            .ports
            .iter()
            .find(|p| p.node_id == dst.id && p.name.starts_with("playback_"))
            .expect("sink playback port");
        (out.id, inp.id)
    };

    backend
        .create_link(out_port, in_port)
        .await
        .expect("create_link");
    eventually("the link to show up in the mirror", SETTLE, || {
        futures_lite_block(backend.graph()).is_ok_and(|g| {
            g.links
                .iter()
                .any(|l| l.output_port == out_port && l.input_port == in_port)
        })
    });

    let link_id = backend
        .graph()
        .await
        .expect("graph")
        .links
        .iter()
        .find(|l| l.output_port == out_port && l.input_port == in_port)
        .expect("link present")
        .id;
    backend.destroy_link(link_id).await.expect("destroy_link");
    eventually("the link to disappear from the mirror", SETTLE, || {
        futures_lite_block(backend.graph()).is_ok_and(|g| {
            !g.links
                .iter()
                .any(|l| l.output_port == out_port && l.input_port == in_port)
        })
    });

    // ── A named route wires itself, with nobody watching ────────────
    //
    // The payoff: this is what a fixed `thread::sleep` got wrong. The
    // route is declared, and the settle path is expected to notice the
    // graph is quiet and apply it.
    backend
        .set_route(NamedRoute {
            name: "sandbox route".to_owned(),
            from: RouteEndpoint {
                node: node_name.clone(),
                port: "monitor_FL".to_owned(),
            },
            to: RouteEndpoint {
                node: sink_node.clone(),
                port: "playback_FL".to_owned(),
            },
            enabled: true,
        })
        .await
        .expect("set_route");

    eventually("the named route to wire itself", SETTLE, || {
        futures_lite_block(backend.graph()).is_ok_and(|g| {
            g.links
                .iter()
                .any(|l| l.output_port == out_port && l.input_port == in_port)
        })
    });

    // Re-applying must not duplicate — routes run on every settle.
    let before = backend.graph().await.expect("graph").links.len();
    backend.apply_routes().await.expect("apply_routes");
    let after = backend.graph().await.expect("graph").links.len();
    assert_eq!(
        before, after,
        "applying an already-satisfied route must create nothing"
    );
}

/// Wait until `node_name` exists AND carries the ports that make it
/// patchable.
///
/// A node appears in the registry before the session manager has
/// applied its port config, so "the node exists" is not the same as
/// "the node can be linked" — waiting on the former is a race.
fn await_patchable(backend: &PatchbayBackend, node_name: &str) {
    eventually(
        &format!("{node_name} to appear with playback and monitor ports"),
        SETTLE,
        || {
            let Ok(g) = futures_lite_block(backend.graph()) else {
                return false;
            };
            let Some(node) = g.nodes.iter().find(|n| n.name == node_name) else {
                return false;
            };
            let has = |prefix: &str| {
                g.ports
                    .iter()
                    .any(|p| p.node_id == node.id && p.name.starts_with(prefix))
            };
            has("playback_") && has("monitor_")
        },
    );
}

/// Drive a future to completion on the current thread.
///
/// `eventually` takes a synchronous closure (it is a polling helper, not
/// an async one), so the checks need a way to await inside it.
fn futures_lite_block<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}
