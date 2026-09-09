//! An isolated `PipeWire` graph for integration tests.
//!
//! This machine's real graph runs Sunday-worship production. Tests that
//! create and destroy links must never touch it, so they don't: this
//! spawns a private `PipeWire` daemon on its own runtime directory, with
//! its own socket, and points the test process at it via
//! `PIPEWIRE_RUNTIME_DIR`. Nothing in the sandbox is visible to the host
//! session and nothing in the host session is visible to the test.
//!
//! The daemon config is deliberately narrow — no ALSA, no v4l2, no jack
//! tunnel — just the factories patchbay actually drives. A session
//! manager (`wireplumber`) runs alongside because without one nothing
//! applies a port config, and a null sink with no ports can't be linked
//! to anything.
//!
//! # One daemon, two protocols
//!
//! There is no `PulseAudio` server here. The single `pipewire` process
//! loads `libpipewire-module-protocol-pulse`, which makes it *speak* the
//! `PulseAudio` wire protocol on a second socket:
//!
//! ```text
//! pipewire (one process)
//! ├── <runtime>/pipewire-0            native  → pw-*, patchbay's engine
//! └── <runtime>/pulse/native-pipewire-0  pulse → pactl, parec
//! ```
//!
//! Both doors open onto the SAME graph: a sink created through `pactl`
//! is a real `PipeWire` node built by the same `support.null-audio-sink`
//! factory patchbay drives natively. `pulseaudio` is only needed for its
//! client tools, never as a server.

#![allow(clippy::arithmetic_side_effects, dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Daemon config: the minimum that lets patchbay create buses and links.
const CONFIG: &str = r"
context.properties = {
    core.daemon = true
    core.name   = pipewire-0
    default.clock.rate        = 48000
    default.clock.quantum     = 1024
    default.clock.min-quantum = 32
    default.clock.max-quantum = 8192
}
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-rt
      args = { nice.level = 20 }
      flags = [ ifexists nofail ] }
    { name = libpipewire-module-protocol-native }
    # Without module-access clients are granted nothing and hang waiting
    # for globals that never arrive.
    { name = libpipewire-module-access }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-link-factory }
    # pipewire-pulse, so the pactl-backed features (capture sources,
    # app-stream routing) have an endpoint that is not the host's.
    { name = libpipewire-module-protocol-pulse }
]
";

/// How long to wait for the daemon to start serving.
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// A private `PipeWire` session. Everything it spawned dies with it.
pub struct Sandbox {
    dir: PathBuf,
    pipewire: Child,
    wireplumber: Option<Child>,
}

impl Sandbox {
    /// Start a sandbox, or `None` when this host has no `pipewire`.
    ///
    /// Set `PATCHBAY_REQUIRE_SANDBOX=1` to turn a missing daemon into a
    /// failure instead — that is what CI should do, so the integration
    /// tests can't silently stop running.
    pub fn start() -> Option<Self> {
        if which("pipewire").is_none() {
            assert!(
                std::env::var_os("PATCHBAY_REQUIRE_SANDBOX").is_none(),
                "PATCHBAY_REQUIRE_SANDBOX is set but `pipewire` is not installed"
            );
            return None;
        }

        // Short path on purpose: a unix socket path caps at 108 bytes,
        // and the usual per-test temp dirs blow straight through it.
        let dir = std::env::temp_dir().join(format!("pb-sbx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pulse")).expect("create sandbox dir");
        let config = dir.join("sandbox.conf");
        std::fs::write(&config, CONFIG).expect("write sandbox config");

        let pipewire = Command::new("pipewire")
            .arg("-c")
            .arg(&config)
            .env("PIPEWIRE_RUNTIME_DIR", &dir)
            .env("PULSE_RUNTIME_PATH", dir.join("pulse"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sandbox pipewire");

        let mut sandbox = Self {
            dir,
            pipewire,
            wireplumber: None,
        };
        sandbox.await_ready();

        // A session manager applies the port config; without it a null
        // sink has zero ports and nothing can be linked.
        sandbox.wireplumber = Command::new("wireplumber")
            .env("PIPEWIRE_RUNTIME_DIR", &sandbox.dir)
            .env("XDG_RUNTIME_DIR", &sandbox.dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();

        Some(sandbox)
    }

    /// This sandbox's runtime directory.
    pub fn runtime_dir(&self) -> &Path {
        &self.dir
    }

    /// Value for `PULSE_SERVER`: the socket where this sandbox's
    /// `pipewire` daemon speaks the `PulseAudio` protocol.
    ///
    /// Named for the environment variable, not for a separate server —
    /// there isn't one (see the module docs). The name is
    /// `native-pipewire-0` rather than the plain `native` that
    /// `PULSE_RUNTIME_PATH` alone would make `pactl` look for, because
    /// `pipewire-pulse` appends the core name.
    pub fn pulse_server(&self) -> String {
        format!(
            "unix:{}",
            self.dir.join("pulse/native-pipewire-0").display()
        )
    }

    /// Block until `pactl` can talk to the sandbox, or give up.
    ///
    /// Returns false when this host has no `pactl` — the pulse-backed
    /// features are best-effort by design, and their unit tests still
    /// cover the parsing.
    pub fn await_pulse(&self) -> bool {
        if which("pactl").is_none() {
            return false;
        }
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            let ok = Command::new("pactl")
                .arg("info")
                .env("PULSE_SERVER", self.pulse_server())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }

    /// Point THIS PROCESS at the sandbox.
    ///
    /// libpipewire reads `PIPEWIRE_RUNTIME_DIR` when a client connects,
    /// and patchbay has no per-connection override, so the redirection
    /// has to be process-wide.
    ///
    /// # Safety
    /// Must be called before any patchbay thread starts — i.e. before
    /// the first `PatchbayBackend::new()`. Mutating the environment
    /// races with any other thread reading it.
    pub unsafe fn redirect_this_process(&self) {
        unsafe {
            std::env::set_var("PIPEWIRE_RUNTIME_DIR", &self.dir);
            // pactl/parec look for `<PULSE_RUNTIME_PATH>/native`, but
            // pipewire-pulse names its socket after the core, so point
            // at the real path rather than the conventional one.
            std::env::set_var("PULSE_SERVER", self.pulse_server());
            // Keep the test's own config out of the user's real one.
            std::env::set_var("PATCHBAY_CONFIG", self.dir.join("patchbay.styx"));
        }
    }

    /// Block until the daemon answers, so tests don't race startup.
    fn await_ready(&mut self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(status) = self.pipewire.try_wait().ok().flatten() {
                panic!("sandbox pipewire exited early: {status}");
            }
            let answered = Command::new("pw-dump")
                .env("PIPEWIRE_RUNTIME_DIR", &self.dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if answered {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("sandbox pipewire did not become ready within {READY_TIMEOUT:?}");
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut wp) = self.wireplumber.take() {
            let _ = wp.kill();
            let _ = wp.wait();
        }
        let _ = self.pipewire.kill();
        let _ = self.pipewire.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Is `program` on PATH?
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Poll `check` until it returns true, or fail with `what`.
///
/// Graph changes are asynchronous: a command goes to the `PipeWire`
/// thread, the daemon acts, and the registry announces the result some
/// milliseconds later. Polling is the honest way to wait for that; a
/// fixed sleep is the thing this whole refactor removed.
pub fn eventually(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out after {timeout:?} waiting for: {what}");
}
