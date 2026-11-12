//! The Patchbay core as an Architect app hosts it: linked in, its actions
//! on the app's list, over one engine per machine.
//!
//! The engines here never touch the machine: a free loopback port, a
//! scratch config, the device layer off (`PATCHBAY_DEVICES=off`) and no
//! `PipeWire` connection (`PATCHBAY_PIPEWIRE=off`).
//!
//! The "other app" that owns the engine first is a child process: this
//! binary run as [`owner_process`], killed when the test wants it gone.
//! One test function besides: the engine is one per process, and the
//! environment it reads is the process's.
#![cfg(feature = "extension")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use architect::action::ActionList;
use architect_extensions::extensions::Extensions;
use architect_extensions::host::{Host, SharedSections};
use architect_extensions::install::Installs;
use architect_settings::Registry;
use futures::executor::block_on;
use patchbay::server::{Engine, EngineMode, EngineOptions};
use serde_json::{Value, json};

/// Set on the child process: the address its engine serves.
const OWNER_ADDR: &str = "PATCHBAY_TEST_OWNER_ADDR";

fn run(list: &ActionList, id: &str, args: &Value) -> Result<Value, String> {
    block_on(list.run_json_async(id, args.to_string().as_bytes()))
        .map(|out| serde_json::from_slice(&out).unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn until(what: &str, limit: Duration, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < limit, "waited {limit:?} for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Keeps the machine out of every engine this process or its child runs.
fn isolate(cmd: Option<&mut Command>, config: &std::path::Path, addr: &str) {
    let vars = [
        ("PATCHBAY_CONFIG", config.to_str().unwrap()),
        ("PATCHBAY_ADDR", addr),
        ("PATCHBAY_DEVICES", "off"),
        ("PATCHBAY_PIPEWIRE", "off"),
    ];
    match cmd {
        Some(cmd) => {
            cmd.envs(vars);
        }
        // SAFETY: set before anything in this process reads the
        // environment or starts a thread (nextest runs each test in a
        // process of its own).
        None => unsafe {
            for (k, v) in vars {
                std::env::set_var(k, v);
            }
        },
    }
}

/// The other app, as a child process: runs and serves the engine at
/// `PATCHBAY_TEST_OWNER_ADDR` until it is killed. Without that variable
/// (any ordinary run) it does nothing.
#[test]
fn owner_process() {
    let Ok(addr) = std::env::var(OWNER_ADDR) else {
        return;
    };
    let mut opts = EngineOptions::new(addr);
    opts.discover_peers = false;
    let engine = Engine::start_or_attach_blocking(opts).unwrap();
    assert!(matches!(
        engine.mode(),
        EngineMode::Owned { serving: true, .. }
    ));
    std::thread::sleep(Duration::from_secs(300));
}

fn spawn_owner(config: &std::path::Path, addr: &str) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "owner_process", "--nocapture"])
        .env(OWNER_ADDR, addr);
    isolate(Some(&mut cmd), config, addr);
    cmd.spawn().unwrap()
}

#[test]
fn the_core_runs_over_one_engine_and_takes_it_over_when_its_owner_quits() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("patchbay.styx");
    let addr = format!("127.0.0.1:{}", free_port());
    isolate(None, &config, &addr);

    let list = ActionList::new();
    let host = Host::new(list.clone(), SharedSections::new(Registry::new()));
    let ext = Extensions::new(host, Installs::new(tmp.path().join("installs")))
        .with_linked(&patchbay::ext::Patchbay)
        .unwrap();
    ext.install_contribution_actions().unwrap();
    let _ = ext.sync();

    // Loading it starts nothing: the port is still free.
    drop(std::net::TcpListener::bind(&addr).expect("loading the extension took the port"));

    // Another app on this machine runs the engine.
    let mut owner = spawn_owner(&config, &addr);
    until("the other app's engine", Duration::from_secs(60), || {
        std::net::TcpStream::connect(&addr).is_ok()
    });

    // The first action joins it instead of running a second engine.
    let status = run(&list, "PATCHBAY_STATUS", &json!({})).unwrap();
    let joined = block_on(patchbay::home::engine()).unwrap();
    let url = format!("ws://{addr}/vox");
    assert_eq!(joined.mode(), &EngineMode::Attached { url: url.clone() });

    // One engine: what one client writes, another reads.
    let mut opts = EngineOptions::new(addr.clone());
    opts.discover_peers = false;
    let other = block_on(Engine::start_or_attach(opts)).unwrap();
    block_on(other.run(|c| async move { c.set_alias("probe.node".into(), "Probe".into()).await }))
        .unwrap()
        .unwrap();
    let aliases = block_on(joined.run(|c| async move { c.aliases().await }))
        .unwrap()
        .unwrap();
    assert!(
        aliases
            .iter()
            .any(|a| a.target == "probe.node" && a.alias == "Probe"),
        "the alias set through one client is not in the engine: {aliases:?}"
    );

    // The actions answer the JSON `patchbay … --json` prints, which a
    // client over ws (the CLI's link) builds the same way.
    let cli_status = block_on(other.run(|c| async move { patchbay::report::status(&c).await }))
        .unwrap()
        .unwrap();
    assert_eq!(status, cli_status);
    // The engines here run without PipeWire, and say so.
    let expected = if cfg!(target_os = "linux") {
        "off"
    } else {
        "unsupported"
    };
    assert_eq!(status["pipewire"], expected);
    let devices = run(&list, "PATCHBAY_DEVICE_LIST", &json!({})).unwrap();
    let cli_devices = block_on(other.run(|c| async move { c.list_devices().await }))
        .unwrap()
        .unwrap();
    assert_eq!(devices, serde_json::to_value(cli_devices).unwrap());
    let health = run(&list, "PATCHBAY_HEALTH", &json!({})).unwrap();
    assert_eq!(health["pipewire"], expected);
    assert_eq!(
        health["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "pipewire_disabled"),
        cfg!(target_os = "linux"),
        "health: {health}"
    );
    for key in ["ok", "graph", "clock", "dante", "services", "issues"] {
        assert!(health.get(key).is_some(), "health has no `{key}`: {health}");
    }
    let now = run(&list, "PATCHBAY_NOW", &json!({})).unwrap();
    assert!(now.get("supported").is_some(), "now: {now}");
    let err = run(&list, "PATCHBAY_DEVICE_SHOW", &json!({})).unwrap_err();
    assert!(err.contains("give `id`"), "{err}");
    assert!(run(&list, "PATCHBAY_DEVICE_SHOW", &json!({"id": "nope"})).is_err());

    // The other app quits: the joined engine notices, and the next action
    // takes the engine over, here, serving the port.
    owner.kill().unwrap();
    owner.wait().unwrap();
    until(
        "the joined engine to notice",
        Duration::from_secs(10),
        || joined.is_lost(),
    );
    run(&list, "PATCHBAY_STATUS", &json!({})).unwrap();
    let now_here = block_on(patchbay::home::engine()).unwrap();
    assert_eq!(
        now_here.mode(),
        &EngineMode::Owned {
            addr: addr.clone(),
            serving: true
        }
    );
    // The config is on disk, so the takeover kept what was set.
    let aliases = block_on(now_here.run(|c| async move { c.aliases().await }))
        .unwrap()
        .unwrap();
    assert!(aliases.iter().any(|a| a.alias == "Probe"));

    // And the next app to come along joins this one.
    let mut opts = EngineOptions::new(addr);
    opts.discover_peers = false;
    let next = block_on(Engine::start_or_attach(opts)).unwrap();
    assert_eq!(next.mode(), &EngineMode::Attached { url });
}
