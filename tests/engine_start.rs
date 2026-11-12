//! Starting when the Patchbay port is already held.
//!
//! - A start that fails costs nothing: with the port held by something
//!   that is not Patchbay, starting over and over (an agent polling
//!   `PATCHBAY_STATUS`) leaves the thread count flat.
//! - A holder that never answers (a Patchbay starting up, or wedged) is
//!   waited for and then refused: never a second engine.
//! - Only a holder that answers and is not Patchbay lets `RunUnserved`
//!   run the engine unserved.
//! - Port 0 (any free port: a phone's engine) is taken at once: there is
//!   no engine there to join, so no join deadline to sit out.
//!
//! A binary of its own, so no other test's threads move the count
//! (nextest runs each test in a process of its own).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;

use std::time::Duration;

use patchbay::server::{Engine, EngineError, EngineMode, EngineOptions, IfTaken};

/// This process's threads.
fn threads() -> usize {
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        return dir.count();
    }
    // macOS: one line per thread, after the header.
    let out = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .count()
        .saturating_sub(1)
}

#[test]
fn failed_starts_leave_the_thread_count_flat() {
    // SAFETY: before any thread of this test process reads it.
    unsafe {
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
    }
    // The port, held by something that answers TCP and hangs up: not a
    // Patchbay engine.
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for conn in holder.incoming().flatten() {
            let mut conn = conn;
            let _ = conn.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }
    });
    let start = || {
        let mut opts = EngineOptions::new(addr.clone());
        opts.discover_peers = false;
        Engine::start_or_attach_blocking(opts)
    };

    // The first failure brings up the runtime; count from there.
    assert!(matches!(start(), Err(EngineError::AddressTaken { .. })));
    let before = threads();
    for _ in 0..5 {
        assert!(matches!(start(), Err(EngineError::AddressTaken { .. })));
    }
    let after = threads();
    // A little slack for tokio's blocking pool (address lookups), which
    // reuses its threads; a runtime per attempt would add dozens each.
    assert!(
        after <= before + 4,
        "five failed starts grew the process from {before} to {after} threads"
    );
}

/// A port held by something that accepts and never says a word.
fn silent_holder() -> (std::net::TcpListener, String) {
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap().to_string();
    (holder, addr)
}

#[test]
fn a_holder_that_never_answers_is_waited_for_and_never_doubled() {
    // SAFETY: before any thread of this test process reads it.
    unsafe {
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
        std::env::set_var("PATCHBAY_DEVICES", "off");
    }
    // Kept bound for the whole test: connections queue, nobody answers.
    let (_holder, addr) = silent_holder();
    let mut opts = EngineOptions::new(addr);
    opts.discover_peers = false;
    // Even the app's "run unserved anyway" must not start a second engine
    // when the holder may be a Patchbay.
    opts.if_taken = IfTaken::RunUnserved;
    opts.join_deadline = Duration::from_secs(3);
    match Engine::start_or_attach_blocking(opts) {
        Err(EngineError::NoAnswer { .. }) => {}
        Err(other) => panic!("expected NoAnswer, got {other}"),
        Ok(engine) => panic!(
            "started an engine next to a silent holder: {:?}",
            engine.mode()
        ),
    }
}

#[test]
fn a_holder_that_is_not_patchbay_runs_the_engine_unserved_only_when_asked() {
    // SAFETY: before any thread of this test process reads it.
    unsafe {
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
        std::env::set_var("PATCHBAY_DEVICES", "off");
        std::env::set_var(
            "PATCHBAY_CONFIG",
            std::env::temp_dir().join(format!("pb-unserved-{}.styx", std::process::id())),
        );
    }
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for conn in holder.incoming().flatten() {
            let mut conn = conn;
            let _ = conn.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }
    });
    let mut opts = EngineOptions::new(addr.clone());
    opts.discover_peers = false;
    assert!(matches!(
        Engine::start_or_attach_blocking(opts),
        Err(EngineError::AddressTaken { .. })
    ));
    let mut opts = EngineOptions::new(addr.clone());
    opts.discover_peers = false;
    opts.if_taken = IfTaken::RunUnserved;
    let engine = Engine::start_or_attach_blocking(opts).unwrap();
    assert_eq!(
        engine.mode(),
        &EngineMode::Owned {
            addr,
            serving: false
        }
    );
}

#[test]
fn port_zero_starts_at_once_without_a_join() {
    // SAFETY: before any thread of this test process reads it.
    unsafe {
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
        std::env::set_var("PATCHBAY_DEVICES", "off");
        std::env::set_var(
            "PATCHBAY_CONFIG",
            std::env::temp_dir().join(format!("pb-port-zero-{}.styx", std::process::id())),
        );
    }
    let mut opts = EngineOptions::new("127.0.0.1:0");
    opts.discover_peers = false;
    // A join attempt at port 0 can only fail; waiting it out would cost
    // the whole deadline.
    opts.join_deadline = Duration::from_secs(30);
    let started = std::time::Instant::now();
    let engine = Engine::start_or_attach_blocking(opts).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "port 0 took {:?} to start",
        started.elapsed()
    );
    // It reports the port it got, not 0, and serves there.
    let EngineMode::Owned {
        addr,
        serving: true,
    } = engine.mode()
    else {
        panic!("not an owned, serving engine: {:?}", engine.mode());
    };
    let bound: std::net::SocketAddr = addr.parse().unwrap();
    assert_ne!(bound.port(), 0, "the engine reports port 0");
    std::net::TcpStream::connect(bound).expect("nothing serves the reported address");
}
