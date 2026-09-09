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
//! tunnel, no pulse — just the factories patchbay actually drives. A
//! session manager (`wireplumber`) runs alongside because without one
//! nothing applies a port config, and a null sink with no ports can't be
//! linked to anything.

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
        std::fs::create_dir_all(&dir).expect("create sandbox dir");
        let config = dir.join("sandbox.conf");
        std::fs::write(&config, CONFIG).expect("write sandbox config");

        let pipewire = Command::new("pipewire")
            .arg("-c")
            .arg(&config)
            .env("PIPEWIRE_RUNTIME_DIR", &dir)
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
