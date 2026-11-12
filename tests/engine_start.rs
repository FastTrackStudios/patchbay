//! A start that fails costs nothing: with the Patchbay port held by
//! something that is not Patchbay, starting over and over (an agent
//! polling `PATCHBAY_STATUS`) leaves the thread count flat. Every start
//! runs on the one engine runtime; nothing is made per attempt.
//!
//! A binary of its own, so no other test's threads move the count.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;

use patchbay::server::{Engine, EngineError, EngineOptions};

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
