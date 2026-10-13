//! Tap one app and monitor it to an output device for N seconds, then tear
//! everything down (`IOProc`, private aggregate, process tap).
//!
//! ```bash
//! cargo run -p patchbay-host-coreaudio --example ca_tap_monitor -- \
//!     <bundle-id-or-pid> <output-device-uid> [--seconds 5] [--gain 1.0] \
//!     [--map 0:0,1:1] [--mute] [--timeout 15]
//! ```
//!
//! Default is **non-muting**: the app keeps playing on its own device and
//! is *additionally* heard on the monitor output. `--mute` silences the
//! tapped app on its own device while the tap exists.
//!
//! Needs "System Audio Recording" (Privacy & Security → Screen & System
//! Audio Recording) for the *responsible* app — the terminal you run
//! cargo from. Without it the tap is created but delivers silence; this
//! example detects and reports that.

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};

    use patchbay_host::HostBackend;
    use patchbay_host_coreaudio::{
        CapturePermission, CoreAudioBackend, TapMonitor, capture_permission,
    };

    let (config, seconds, timeout) = parse_args()?;

    let permission = capture_permission();
    eprintln!("System Audio Recording preflight: {permission:?}");
    if permission == CapturePermission::Denied {
        eprintln!(
            "  → denied for the responsible app (your terminal). The tap will be silent. Grant it in System \
             Settings → Privacy & Security → Screen & System Audio Recording, then re-run."
        );
    }

    // Snapshot before: is the app actually playing?
    let backend = CoreAudioBackend::new()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let before = rt.block_on(backend.snapshot())?;

    eprintln!("config: {}", serde_json::to_string(&config)?);
    let started = Instant::now();
    let monitor = TapMonitor::start_with_timeout(&config, Duration::from_secs(timeout))?;
    eprintln!("started in {:?}", started.elapsed());
    println!("{}", serde_json::to_string_pretty(monitor.info())?);

    let playing = monitor.info().processes.iter().any(|p| {
        before
            .node(&format!("coreaudio:process:{}", p.pid))
            .is_some_and(|n| {
                n.props
                    .get("coreaudio.running_output")
                    .is_some_and(|v| v == "true")
            })
    });

    let mut loudest = 0.0_f32;
    for second in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let peaks = monitor.take_peaks();
        loudest = peaks.iter().copied().fold(loudest, f32::max);
        let db: Vec<String> = peaks
            .iter()
            .map(|p| {
                if *p > 0.0 {
                    format!("{:6.1} dBFS", 20.0 * p.log10())
                } else {
                    "  -inf dBFS".to_owned()
                }
            })
            .collect();
        let (bufs_in, bufs_out) = monitor.observed_buffers();
        eprintln!(
            "t={second:>3}s cycles={:>6} buffers in/out={bufs_in}/{bufs_out} tap peaks [{}]",
            monitor.cycles(),
            db.join(", ")
        );
    }

    let cycles = monitor.cycles();
    let aggregate_uid = monitor.info().aggregate_uid.clone();
    let (tap_obj, agg_obj) = monitor.objects();
    drop(monitor);
    eprintln!("torn down tap {tap_obj} + aggregate {agg_obj}");

    let after = rt.block_on(backend.snapshot())?;
    let leaked = after
        .nodes
        .iter()
        .any(|n| n.props.get("coreaudio.uid") == Some(&aggregate_uid));
    eprintln!("aggregate still present after teardown: {leaked}");

    diagnose(cycles, loudest, playing, permission);
    if leaked {
        return Err("teardown left the aggregate device behind".into());
    }
    Ok(())
}

/// `(config, seconds, timeout seconds)` from the command line.
#[cfg(target_os = "macos")]
fn parse_args()
-> Result<(patchbay_host_coreaudio::TapMonitorConfig, u64, u64), Box<dyn std::error::Error>> {
    use patchbay_host::{AppSelector, ChannelMap};
    use patchbay_host_coreaudio::{TapMonitorConfig, TapMute};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag_value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i.saturating_add(1)))
    };
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            !a.starts_with("--")
                && !i
                    .checked_sub(1)
                    .and_then(|p| args.get(p))
                    .is_some_and(|prev| {
                        matches!(
                            prev.as_str(),
                            "--seconds" | "--gain" | "--map" | "--timeout"
                        )
                    })
        })
        .map(|(_, a)| a)
        .collect();
    let (Some(app), Some(output_uid)) = (positional.first(), positional.get(1)) else {
        return Err("usage: ca_tap_monitor <bundle-id-or-pid> <output-device-uid> [--seconds N] [--gain G] \
                    [--map 0:0,1:1] [--mute] [--timeout S]"
            .into());
    };
    let seconds: u64 = flag_value("--seconds").map_or(Ok(5), |s| s.parse())?;
    let timeout: u64 = flag_value("--timeout").map_or(Ok(15), |s| s.parse())?;

    let mut config = TapMonitorConfig::new(AppSelector::parse(app), output_uid.as_str());
    config.gain = flag_value("--gain").map_or(Ok(1.0), |s| s.parse())?;
    if let Some(map) = flag_value("--map") {
        config.channel_map = ChannelMap::parse(map)?;
    }
    if args.iter().any(|a| a == "--mute") {
        config.mute = TapMute::Muted;
    }

    Ok((config, seconds, timeout))
}

/// Explain the run's outcome (IO ran? tap silent? why?).
#[cfg(target_os = "macos")]
fn diagnose(
    cycles: u64,
    loudest: f32,
    playing: bool,
    permission: patchbay_host_coreaudio::CapturePermission,
) {
    if cycles == 0 {
        if playing {
            eprintln!("DIAGNOSIS: the aggregate's IO never ran although the app was playing.");
        } else {
            eprintln!(
                "DIAGNOSIS: the aggregate's IO never ran — the tapped app was not playing (a tap-bearing aggregate \
                 only clocks while its tapped process runs output). Start playback and re-run."
            );
        }
    } else if loudest <= 0.0 {
        if playing {
            eprintln!(
                "DIAGNOSIS: IO ran but the tap was silent although the app was playing — System Audio Recording \
                 is most likely denied for the responsible app (preflight: {permission:?})."
            );
        } else {
            eprintln!(
                "DIAGNOSIS: IO ran, tap silent — the app was not playing (running_output=false)."
            );
        }
    } else {
        eprintln!(
            "OK: tap delivered audio (peak {:.1} dBFS).",
            20.0 * loudest.log10()
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ca_tap_monitor: Core Audio is macOS-only");
}
