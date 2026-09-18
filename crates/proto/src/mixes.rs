//! Mixes: Loopback / OBS-style audio mixes on the host.
//!
//! A mix sums **sources** (an app's audio, an input device's channels,
//! everything the system plays) through channel maps and gains into its
//! **outputs** (output devices — e.g. the "Broadcast" virtual device an
//! app like Discord or `FaceTime` picks as its microphone, or spare
//! playback channels of an interface that loop back into a DAW).
//!
//! The mix itself has `channels` channels (2 = stereo): a source map is
//! `source channel → mix channel`, an output map `mix channel → output
//! channel`, both written `src:dst,src:dst` (0-based).

use facet::Facet;
use serde::{Deserialize, Serialize};

/// Default map: stereo straight through.
#[must_use]
pub fn default_map() -> String {
    "0:0,1:1".to_owned()
}

const fn default_channels() -> u32 {
    2
}

/// Where a mix source takes audio from.
pub mod source_kind {
    /// An application's output (process tap). `target` = bundle id.
    pub const APP: &str = "app";
    /// An input device's capture channels. `target` = device uid.
    pub const INPUT: &str = "input";
    /// Everything the system plays except Patchbay itself.
    pub const SYSTEM: &str = "system";
}

/// One source of a mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixSourceConfig {
    /// `app`, `input` or `system` (see [`source_kind`]).
    pub kind: String,
    /// Bundle id (`app`), device uid (`input`), empty (`system`).
    #[serde(default)]
    #[facet(default)]
    pub target: String,
    /// `app` only: take what the app plays to THIS output device (uid),
    /// every channel unmixed — e.g. REAPER's outputs 33–34 on a 64-channel
    /// interface, picked by `map`. Empty = the app's stereo mixdown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[facet(default, skip_serializing_if = String::is_empty)]
    pub device: String,
    /// `source channel → mix channel`, `src:dst,…` (0-based).
    #[serde(default = "default_map")]
    #[facet(default = default_map())]
    pub map: String,
    /// Gain in dB (0 = unity).
    #[serde(default)]
    #[facet(default)]
    pub gain_db: f64,
    /// Muted sources stay configured but pass nothing.
    #[serde(default)]
    #[facet(default)]
    pub muted: bool,
}

/// One output of a mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixOutputConfig {
    /// Output device uid (`Broadcast_UID`, `com.antelope.…`).
    pub device: String,
    /// `mix channel → output channel`, `src:dst,…` (0-based).
    #[serde(default = "default_map")]
    #[facet(default = default_map())]
    pub map: String,
    /// Gain in dB (0 = unity).
    #[serde(default)]
    #[facet(default)]
    pub gain_db: f64,
    /// Muted outputs stay configured but pass nothing.
    #[serde(default)]
    #[facet(default)]
    pub muted: bool,
}

/// A saved mix (config section `mixes`, upsert by `name`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixConfig {
    /// Unique name.
    pub name: String,
    /// Mix channel count (2 = stereo).
    #[serde(default = "default_channels")]
    #[facet(default = default_channels())]
    pub channels: u32,
    #[serde(default)]
    #[facet(default)]
    pub sources: Vec<MixSourceConfig>,
    #[serde(default)]
    #[facet(default)]
    pub outputs: Vec<MixOutputConfig>,
    /// `false` keeps the mix saved but stopped. Unset = running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub enabled: Option<bool>,
}

impl MixConfig {
    /// Whether the mix should run.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// Live state of one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixSourceStatus {
    /// Human label (`REAPER`, `Input: Galaxy32`, …).
    pub label: String,
    /// Feeding the mix right now.
    pub active: bool,
    /// Why not (app not running, device missing, …).
    #[serde(default)]
    #[facet(default)]
    pub reason: String,
    /// Channels the source delivers.
    pub channels: u32,
}

/// Live state of one output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixOutputStatus {
    /// Output device display name.
    pub device_name: String,
    /// This output clocks the mix (the first one).
    pub clock: bool,
}

/// A mix: its config and, when running, its live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixView {
    pub config: MixConfig,
    /// Rendering now.
    pub running: bool,
    /// Why it isn't (empty when running or disabled).
    #[serde(default)]
    #[facet(default)]
    pub error: String,
    /// Per source, same order as `config.sources` (empty when stopped).
    #[serde(default)]
    #[facet(default)]
    pub sources: Vec<MixSourceStatus>,
    /// Per output, same order as `config.outputs` (empty when stopped).
    #[serde(default)]
    #[facet(default)]
    pub outputs: Vec<MixOutputStatus>,
}

/// Peak levels since the previous call (linear 0..1+).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct MixMeters {
    pub name: String,
    /// Per source, per source channel (pre-gain).
    pub sources: Vec<Vec<f32>>,
    /// Per output, per mapped output channel (post-mix).
    pub outputs: Vec<Vec<f32>>,
}

/// An application that can be a mix source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct HostApp {
    pub bundle_id: String,
    pub name: String,
    /// Producing audio right now.
    pub playing: bool,
}

/// An audio device that can be a mix source (inputs) or output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct HostDevice {
    pub uid: String,
    pub name: String,
    pub input_channels: u32,
    pub output_channels: u32,
}

/// What mixes can be built from on this host.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, Facet)]
pub struct HostTargets {
    /// Mixes can run here (macOS with Core Audio).
    pub supported: bool,
    pub apps: Vec<HostApp>,
    pub devices: Vec<HostDevice>,
}

/// A Patchbay virtual audio device (published by `Patchbay.driver`).
///
/// A loopback apps can pick as an output (Patchbay takes the audio) or as
/// an input / microphone (they hear a mix). Created, renamed and removed
/// at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct VirtualDeviceView {
    /// Core Audio UID (what mixes reference; survives renames).
    pub uid: String,
    pub name: String,
    pub channels: u32,
    #[serde(default)]
    #[facet(default)]
    pub hidden: bool,
}

/// The virtual-device driver and what it publishes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Facet)]
pub struct VirtualDevicesStatus {
    /// `Patchbay.driver` is installed and loaded.
    pub driver_loaded: bool,
    pub devices: Vec<VirtualDeviceView>,
}

/// A public aggregate device Patchbay made (e.g. "REAPER I/O" =
/// Galaxy32 + Patchbay, for a DAW that can only open one device).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct AggregateView {
    pub uid: String,
    pub name: String,
    pub input_channels: u32,
    pub output_channels: u32,
}
