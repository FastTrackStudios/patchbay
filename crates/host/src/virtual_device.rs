//! Loopback-style virtual devices: a named device fed by sources and
//! optionally monitored to outputs (modelled on Rogue Amoeba Loopback).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{AppInfo, Gain, HostError};

/// Most channels a virtual device may have (Loopback's own ceiling).
const MAX_DEVICE_CHANNELS: u32 = 64;

/// One `source channel → destination channel` routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChannelPair {
    /// 0-based channel on the source side.
    pub src: u32,
    /// 0-based channel on the destination side.
    pub dst: u32,
}

/// A channel mapping. Several pairs may share a `dst` (they mix) or a
/// `src` (fan-out); duplicate pairs are rejected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelMap(pub Vec<ChannelPair>);

impl ChannelMap {
    /// `0→0, 1→1, … n-1→n-1`.
    #[must_use]
    pub fn identity(n: u32) -> Self {
        Self((0..n).map(|c| ChannelPair { src: c, dst: c }).collect())
    }

    /// `0→first, 1→first+1, …` for `n` channels; `None` on overflow.
    #[must_use]
    pub fn offset(n: u32, first: u32) -> Option<Self> {
        (0..n)
            .map(|c| c.checked_add(first).map(|dst| ChannelPair { src: c, dst }))
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }

    /// Parse `"0:0,1:1"` (src:dst pairs, comma separated). Whitespace is
    /// ignored; an empty string is the empty map.
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] on malformed input.
    pub fn parse(s: &str) -> Result<Self, HostError> {
        let bad =
            || HostError::InvalidSpec(format!("channel map `{s}`: expected `src:dst,src:dst…`"));
        s.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (a, b) = p.split_once(':').ok_or_else(bad)?;
                Ok(ChannelPair {
                    src: a.trim().parse().map_err(|_| bad())?,
                    dst: b.trim().parse().map_err(|_| bad())?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// The pairs.
    #[must_use]
    pub fn pairs(&self) -> &[ChannelPair] {
        &self.0
    }

    /// Check every pair against the channel counts; `None` means "unknown
    /// here, don't check that side".
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] for an out-of-range channel or a
    /// duplicate pair.
    pub fn validate(
        &self,
        src_channels: Option<u32>,
        dst_channels: Option<u32>,
    ) -> Result<(), HostError> {
        let mut seen = BTreeSet::new();
        for pair in &self.0 {
            if let Some(n) = src_channels.filter(|n| pair.src >= *n) {
                return Err(HostError::InvalidSpec(format!(
                    "source channel {} out of range (0..{n})",
                    pair.src
                )));
            }
            if let Some(n) = dst_channels.filter(|n| pair.dst >= *n) {
                return Err(HostError::InvalidSpec(format!(
                    "destination channel {} out of range (0..{n})",
                    pair.dst
                )));
            }
            if !seen.insert(*pair) {
                return Err(HostError::InvalidSpec(format!(
                    "duplicate mapping {}→{}",
                    pair.src, pair.dst
                )));
            }
        }
        Ok(())
    }
}

/// Which application(s) an app source captures.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppSelector {
    /// Every process with this bundle id (persistable).
    BundleId(String),
    /// One process (ephemeral — never persist).
    Pid(i32),
}

impl AppSelector {
    /// Parse a CLI argument: all digits → pid, anything else → bundle id.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        s.parse::<i32>()
            .map_or_else(|_| Self::BundleId(s.to_owned()), Self::Pid)
    }

    /// Whether `app` is selected.
    #[must_use]
    pub fn matches(&self, app: &AppInfo) -> bool {
        match self {
            Self::BundleId(id) => app.bundle_id.as_deref() == Some(id.as_str()),
            Self::Pid(pid) => app.pid == *pid,
        }
    }
}

/// Where a virtual-device source takes audio from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceKind {
    /// One application's output (a process tap on macOS).
    App {
        /// Which app.
        app: AppSelector,
    },
    /// An input device's capture channels.
    InputDevice {
        /// Backend device uid (Core Audio `DeviceUID`, `PipeWire`
        /// `node.name`).
        uid: String,
    },
    /// Everything the system plays (a global tap).
    SystemAudio,
    /// Audio other apps send *to* this virtual device when they select it
    /// as their output. Needs [`crate::HostCapabilities::pass_thru_device`].
    PassThru,
}

/// One source of a virtual device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Where the audio comes from.
    pub kind: SourceKind,
    /// Source channel → virtual-device channel.
    pub channel_map: ChannelMap,
    /// Linear gain.
    pub volume: f32,
    /// Disabled sources stay configured but pass nothing.
    pub enabled: bool,
}

/// One monitor (listen-through) of a virtual device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSpec {
    /// Output device uid.
    pub device_uid: String,
    /// Virtual-device channel → output-device channel.
    pub channel_map: ChannelMap,
    /// Linear gain.
    pub volume: f32,
    /// Disabled monitors stay configured but pass nothing.
    pub enabled: bool,
}

/// A Loopback-style virtual device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualDeviceSpec {
    /// Display name (what other apps see when the backend can publish it).
    pub name: String,
    /// Channel count of the device.
    pub channels: u32,
    /// What feeds it.
    pub sources: Vec<SourceSpec>,
    /// Where it is monitored.
    pub monitors: Vec<MonitorSpec>,
}

impl VirtualDeviceSpec {
    /// Structural validation — everything checkable without the OS:
    /// name, channel count, each map's device side, gains, duplicate
    /// sources, at most one pass-thru.
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] describing the first problem.
    pub fn validate(&self) -> Result<(), HostError> {
        if self.name.trim().is_empty() {
            return Err(HostError::InvalidSpec(
                "virtual device name is empty".to_owned(),
            ));
        }
        if self.channels == 0 || self.channels > MAX_DEVICE_CHANNELS {
            return Err(HostError::InvalidSpec(format!(
                "channel count {} outside 1..={MAX_DEVICE_CHANNELS}",
                self.channels
            )));
        }
        let mut kinds = BTreeSet::new();
        for (i, source) in self.sources.iter().enumerate() {
            let what = format!("source {i}");
            source
                .channel_map
                .validate(None, Some(self.channels))
                .map_err(|e| prefix(&what, e))?;
            Gain::validate(&what, source.volume)?;
            if !kinds.insert(format!("{:?}", source.kind)) {
                return Err(HostError::InvalidSpec(format!(
                    "{what}: duplicate source {:?}",
                    source.kind
                )));
            }
        }
        for (i, monitor) in self.monitors.iter().enumerate() {
            let what = format!("monitor {i}");
            if monitor.device_uid.is_empty() {
                return Err(HostError::InvalidSpec(format!("{what}: empty device uid")));
            }
            monitor
                .channel_map
                .validate(Some(self.channels), None)
                .map_err(|e| prefix(&what, e))?;
            Gain::validate(&what, monitor.volume)?;
        }
        Ok(())
    }

    /// Whether any enabled source needs a pass-thru (HAL) device.
    #[must_use]
    pub fn needs_pass_thru(&self) -> bool {
        self.sources
            .iter()
            .any(|s| s.enabled && s.kind == SourceKind::PassThru)
    }
}

fn prefix(what: &str, e: HostError) -> HostError {
    match e {
        HostError::InvalidSpec(s) => HostError::InvalidSpec(format!("{what}: {s}")),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_app(bundle: &str) -> SourceSpec {
        SourceSpec {
            kind: SourceKind::App {
                app: AppSelector::BundleId(bundle.to_owned()),
            },
            channel_map: ChannelMap::identity(2),
            volume: 1.0,
            enabled: true,
        }
    }

    fn spec() -> VirtualDeviceSpec {
        VirtualDeviceSpec {
            name: "Stream Mix".to_owned(),
            channels: 4,
            sources: vec![
                stereo_app("com.apple.Music"),
                SourceSpec {
                    kind: SourceKind::InputDevice {
                        uid: "BuiltInMic".to_owned(),
                    },
                    channel_map: ChannelMap::offset(1, 2).unwrap(),
                    volume: 0.5,
                    enabled: true,
                },
            ],
            monitors: vec![MonitorSpec {
                device_uid: "BuiltInSpeakers".to_owned(),
                channel_map: ChannelMap::identity(2),
                volume: 1.0,
                enabled: true,
            }],
        }
    }

    #[test]
    fn channel_map_helpers() {
        assert_eq!(
            ChannelMap::identity(2).pairs(),
            &[
                ChannelPair { src: 0, dst: 0 },
                ChannelPair { src: 1, dst: 1 }
            ]
        );
        assert_eq!(
            ChannelMap::offset(2, 4).unwrap().pairs()[1],
            ChannelPair { src: 1, dst: 5 }
        );
        assert!(ChannelMap::offset(2, u32::MAX).is_none());
        assert_eq!(
            ChannelMap::parse(" 0:2, 1:3 ").unwrap(),
            ChannelMap::offset(2, 2).unwrap()
        );
        assert_eq!(ChannelMap::parse("").unwrap(), ChannelMap::default());
        assert!(ChannelMap::parse("0-1").is_err());
        assert!(ChannelMap::parse("a:1").is_err());
    }

    #[test]
    fn channel_map_validation() {
        let map = ChannelMap::identity(2);
        assert!(map.validate(Some(2), Some(2)).is_ok());
        assert!(map.validate(None, None).is_ok());
        assert!(map.validate(Some(1), None).is_err());
        assert!(map.validate(None, Some(1)).is_err());
        // Mixing two sources into one destination is fine…
        assert!(
            ChannelMap::parse("0:0,1:0")
                .unwrap()
                .validate(Some(2), Some(1))
                .is_ok()
        );
        // …a duplicate pair is not.
        assert!(
            ChannelMap::parse("0:0,0:0")
                .unwrap()
                .validate(None, None)
                .is_err()
        );
    }

    #[test]
    fn spec_validation() {
        assert!(spec().validate().is_ok());

        let mut s = spec();
        s.name = "  ".to_owned();
        assert!(s.validate().is_err());

        let mut s = spec();
        s.channels = 0;
        assert!(s.validate().is_err());

        let mut s = spec();
        s.sources[0].channel_map = ChannelMap::offset(2, 3).unwrap();
        let err = s.validate().unwrap_err();
        assert!(err.to_string().contains("source 0"), "{err}");

        let mut s = spec();
        s.sources.push(stereo_app("com.apple.Music"));
        assert!(s.validate().is_err());

        let mut s = spec();
        s.monitors[0].volume = 10.0;
        assert!(s.validate().is_err());

        let mut s = spec();
        s.monitors[0].channel_map = ChannelMap::parse("4:0").unwrap();
        assert!(s.validate().is_err());
    }

    #[test]
    fn pass_thru_detection() {
        let mut s = spec();
        assert!(!s.needs_pass_thru());
        s.sources.push(SourceSpec {
            kind: SourceKind::PassThru,
            channel_map: ChannelMap::identity(2),
            volume: 1.0,
            enabled: true,
        });
        assert!(s.needs_pass_thru());
    }

    #[test]
    fn app_selector() {
        let app = AppInfo {
            pid: 42,
            bundle_id: Some("com.apple.Music".to_owned()),
            name: "Music".to_owned(),
        };
        assert_eq!(AppSelector::parse("42"), AppSelector::Pid(42));
        assert!(AppSelector::parse("42").matches(&app));
        assert!(AppSelector::parse("com.apple.Music").matches(&app));
        assert!(!AppSelector::parse("com.spotify.client").matches(&app));
    }

    #[test]
    fn serde_shape() {
        let json = serde_json::to_value(&spec()).unwrap();
        assert_eq!(json["sources"][0]["kind"]["kind"], "app");
        assert_eq!(
            json["sources"][0]["kind"]["app"]["bundle_id"],
            "com.apple.Music"
        );
        assert_eq!(json["sources"][0]["channel_map"][1]["dst"], 1);
        let back: VirtualDeviceSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec());
    }
}
