//! Run a Loopback-style mix for N seconds, printing per-source and
//! per-output peaks every second.
//!
//! ```bash
//! cargo run -p patchbay-host-coreaudio --example ca_mix -- \
//!     --source app:com.cockos.reaper \
//!     --source input:Patchbay_UID:0:0,1:1 \
//!     --monitor Broadcast_UID \
//!     --seconds 10
//! ```
//!
//! `--source app:<bundle-id|pid>[:<map>]`, `--source input:<device-uid>[:<map>]`,
//! `--source system[:<map>]`; `--monitor <device-uid>[:<map>]`. Maps are
//! `src:dst,…` (source channel → mix channel; mix channel → output channel),
//! default `0:0,1:1`. The mix is 2 channels. Nothing is muted.
//!
//! App sources need "System Audio Recording" for the responsible app (the
//! terminal) — or run it inside Patchbay.app.

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use patchbay_host::{
        AppSelector, ChannelMap, MonitorSpec, SourceKind, SourceSpec, VirtualDeviceSpec,
    };
    use patchbay_host_coreaudio::Mix;

    fn map(s: Option<&str>) -> Result<ChannelMap, Box<dyn std::error::Error>> {
        Ok(match s {
            Some(m) if !m.is_empty() => ChannelMap::parse(m)?,
            _ => ChannelMap::identity(2),
        })
    }

    let mut sources = Vec::new();
    let mut monitors = Vec::new();
    let mut seconds = 10_u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next().ok_or_else(|| format!("{a} needs a value"))?;
        match a.as_str() {
            "--source" => {
                let (kind, rest) = v.split_once(':').unwrap_or((v.as_str(), ""));
                let (kind, m) = match kind {
                    "app" => {
                        let (app, m) = rest
                            .split_once(':')
                            .map_or((rest, None), |(a, m)| (a, Some(m)));
                        (
                            SourceKind::App {
                                app: AppSelector::parse(app),
                            },
                            m,
                        )
                    }
                    "input" => {
                        let (uid, m) = rest
                            .split_once(':')
                            .map_or((rest, None), |(u, m)| (u, Some(m)));
                        (
                            SourceKind::InputDevice {
                                uid: uid.to_owned(),
                            },
                            m,
                        )
                    }
                    "system" => (SourceKind::SystemAudio, Some(rest)),
                    other => return Err(format!("unknown source kind `{other}`").into()),
                };
                sources.push(SourceSpec {
                    kind,
                    mute: patchbay_host::TapMute::Unmuted,
                    channel_map: map(m)?,
                    volume: 1.0,
                    enabled: true,
                });
            }
            "--monitor" => {
                let (uid, m) = v
                    .split_once(':')
                    .map_or((v.as_str(), None), |(u, m)| (u, Some(m)));
                monitors.push(MonitorSpec {
                    device_uid: uid.to_owned(),
                    channel_map: map(m)?,
                    volume: 1.0,
                    enabled: true,
                });
            }
            "--seconds" => seconds = v.parse()?,
            other => return Err(format!("unknown flag `{other}`").into()),
        }
    }
    let spec = VirtualDeviceSpec {
        name: "ca_mix".to_owned(),
        channels: 2,
        sources,
        monitors,
    };
    let mix = Mix::start(&spec)?;
    println!("{}", serde_json::to_string_pretty(mix.info())?);
    for s in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let fmt = |v: Vec<Vec<f32>>| {
            v.iter()
                .map(|ch| {
                    ch.iter()
                        .map(|p| format!("{p:.3}"))
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        println!(
            "{:>3}s cycles={} sources[{}] outputs[{}]",
            s,
            mix.cycles(),
            fmt(mix.take_source_peaks()),
            fmt(mix.take_monitor_peaks())
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ca_mix is macOS-only");
}
