//! External-device wire types: hardware adapters (Antelope Galaxy32,
//! later Yamaha TF, Dante, …) seen through one generic model.
//!
//! These mirror `patchbay-device`'s model as Facet types. The proto
//! crate deliberately does NOT depend on `patchbay-device` or any
//! adapter: the engine converts at the boundary, so the wire contract
//! (and every UI, including the wasm one) stays adapter-free.
//!
//! Conventions shared by every consumer:
//!
//! - **Param paths** are slash paths, 1-based like the hardware panel:
//!   `mixer/1/strip/16/level`.
//! - **Channels on the wire** ([`DeviceChannel::channel`]) are 0-based;
//!   **displayed / typed by humans** they are 1-based
//!   (`DIGI_OUT0:17` = channel index 16).
//! - **Snapshot route paths** are `route/<OUTPUT_GROUP>/<1-based ch>`,
//!   so params and crosspoints share one prefix-filterable namespace.

use facet::Facet;
use serde::{Deserialize, Serialize};

// ─── Identity / summary ─────────────────────────────────────────────────

/// Connection state of one configured device.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, Facet)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLinkState {
    /// First connect attempt to a pinned address still running.
    #[default]
    Connecting,
    /// Control connection up.
    Online,
    /// Was found (or is pinned) but isn't reachable now; the hub retries
    /// with backoff.
    Offline,
    /// Configured with `enabled false` / `disabled true`.
    Disabled,
    /// Auto-discovery running, nothing found yet.
    Searching,
    /// Auto-discovery finished without finding the device; the hub keeps
    /// looking in the background.
    NotFound,
}

/// One device as listed by `list_devices` — cheap, no device I/O.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceSummary {
    /// Stable identity `vendor:model:serial` once the device has been
    /// reached at least once; empty before that.
    pub id: String,
    /// Config entry name (the `devices` section upsert key).
    pub name: String,
    /// Adapter kind from config, e.g. `antelope-galaxy32`.
    pub kind: String,
    pub vendor: String,
    pub model: String,
    /// Hardware serial (empty when unknown).
    pub serial: String,
    /// Firmware version (empty when unknown).
    pub firmware: String,
    /// Human description of how the device is reached.
    pub transport: String,
    pub state: DeviceLinkState,
    /// Last connect / link error (empty when fine).
    pub error: String,
}

// ─── Router ─────────────────────────────────────────────────────────────

/// A named bank of router channels (`HDX OUT 1-32`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
pub struct DevicePortGroup {
    /// Stable group id (`DIGI_OUT0`).
    pub id: String,
    /// Display name.
    pub name: String,
    pub channels: u16,
}

/// One channel of a [`DevicePortGroup`]. `channel` is **0-based**.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Facet)]
pub struct DeviceChannel {
    pub group: String,
    pub channel: u16,
}

impl DeviceChannel {
    #[must_use]
    pub fn new(group: impl Into<String>, channel: u16) -> Self {
        Self {
            group: group.into(),
            channel,
        }
    }

    /// Human form, 1-based: `DIGI_OUT0:17`.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{}:{}",
            self.group,
            u32::from(self.channel).saturating_add(1)
        )
    }

    /// Parse the human form `GROUP:N` (1-based `N`).
    #[must_use]
    pub fn parse_label(s: &str) -> Option<Self> {
        let (group, n) = s.trim().rsplit_once(':')?;
        let n: u16 = n.trim().parse().ok()?;
        let channel = n.checked_sub(1)?;
        let group = group.trim();
        (!group.is_empty()).then(|| Self::new(group, channel))
    }
}

/// One router cell: the output channel and its single source (`None` =
/// unpatched / silence).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
pub struct DeviceCrosspoint {
    pub output: DeviceChannel,
    pub source: Option<DeviceChannel>,
}

/// Snapshot/plan path of a router output: `route/DIGI_OUT0/17`.
#[must_use]
pub fn route_path(output: &DeviceChannel) -> String {
    format!(
        "route/{}/{}",
        output.group,
        u32::from(output.channel).saturating_add(1)
    )
}

/// Inverse of [`route_path`].
#[must_use]
pub fn parse_route_path(path: &str) -> Option<DeviceChannel> {
    let rest = path.strip_prefix("route/")?;
    let (group, n) = rest.rsplit_once('/')?;
    let channel = n.parse::<u16>().ok()?.checked_sub(1)?;
    (!group.is_empty()).then(|| DeviceChannel::new(group, channel))
}

/// Display form of a crosspoint source (`"-"` for none).
#[must_use]
pub fn source_label(source: Option<&DeviceChannel>) -> String {
    source.map_or_else(|| "-".to_owned(), DeviceChannel::label)
}

// ─── Parameters ─────────────────────────────────────────────────────────

/// JSON encoding for dB / pan floats: finite values are numbers,
/// infinities are the strings `"-inf"` / `"inf"` (plain `serde_json`
/// would write `null`, losing "fader fully down"). Reading also accepts
/// `null` as `-inf` for leniency. Only affects serde (CLI `--json`);
/// the vox wire and styx carry the float natively.
mod db_json {
    use std::fmt;

    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde `with` signature
    pub fn serialize<S: Serializer>(x: &f64, s: S) -> Result<S::Ok, S::Error> {
        if x.is_finite() {
            s.serialize_f64(*x)
        } else if x.is_nan() {
            s.serialize_str("nan")
        } else if x.is_sign_negative() {
            s.serialize_str("-inf")
        } else {
            s.serialize_str("inf")
        }
    }

    struct Db;

    impl Visitor<'_> for Db {
        type Value = f64;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a number, \"-inf\", \"inf\" or null")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> {
            // dB values are small; the string round-trip avoids an `as`.
            v.to_string().parse().map_err(E::custom)
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> {
            v.to_string().parse().map_err(E::custom)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<f64, E> {
            match v.trim().to_ascii_lowercase().as_str() {
                "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                "inf" | "+inf" | "infinity" => Ok(f64::INFINITY),
                other => other.parse().map_err(E::custom),
            }
        }

        fn visit_unit<E: de::Error>(self) -> Result<f64, E> {
            Ok(f64::NEG_INFINITY)
        }

        fn visit_none<E: de::Error>(self) -> Result<f64, E> {
            Ok(f64::NEG_INFINITY)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        d.deserialize_any(Db)
    }
}

/// What a parameter is and which values it accepts.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceParamKind {
    /// Gain in dB.
    Level {
        #[serde(with = "db_json")]
        min_db: f64,
        #[serde(with = "db_json")]
        max_db: f64,
    },
    /// Stereo position `-1.0 (L) ..= 1.0 (R)`.
    Pan,
    /// On/off.
    Toggle,
    /// One of a fixed set; the value is the index.
    Enum { options: Vec<String> },
    /// Integer in `min..=max`.
    Int { min: i64, max: i64 },
    /// Free text.
    Text,
}

/// A parameter value.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum DeviceParamValue {
    /// dB; `-inf` = fully down (JSON: the string `"-inf"`).
    Level(#[serde(with = "db_json")] f64),
    Pan(#[serde(with = "db_json")] f64),
    Toggle(bool),
    Enum(u32),
    Int(i64),
    Text(String),
}

/// Strip a trailing unit, case-insensitively.
fn strip_unit<'a>(s: &'a str, unit: &str) -> &'a str {
    let t = s.trim();
    let cut = t.len().saturating_sub(unit.len());
    match (t.get(..cut), t.get(cut..)) {
        (Some(head), Some(tail)) if tail.eq_ignore_ascii_case(unit) => head.trim(),
        _ => t,
    }
}

impl DeviceParamKind {
    /// Parse a human/agent-typed value for this kind.
    ///
    /// - Level: `-12`, `-12dB`, `-inf` / `off`
    /// - Pan: `-1..1`, or `L`/`C`/`R`
    /// - Toggle: `on|off|true|false|1|0|yes|no`
    /// - Enum: option label (case-insensitive) or index
    /// - Int: integer; Text: verbatim
    ///
    /// # Errors
    /// A message naming what was expected.
    pub fn parse_value(&self, s: &str) -> Result<DeviceParamValue, String> {
        let t = s.trim();
        match self {
            Self::Level { .. } => {
                let v = strip_unit(t, "db");
                if v.eq_ignore_ascii_case("-inf") || v.eq_ignore_ascii_case("off") {
                    return Ok(DeviceParamValue::Level(f64::NEG_INFINITY));
                }
                v.parse::<f64>()
                    .ok()
                    .filter(|x| x.is_finite())
                    .map(DeviceParamValue::Level)
                    .ok_or_else(|| format!("expected a level in dB (e.g. -12, -inf), got '{s}'"))
            }
            Self::Pan => match t.to_ascii_lowercase().as_str() {
                "l" => Ok(DeviceParamValue::Pan(-1.0)),
                "c" => Ok(DeviceParamValue::Pan(0.0)),
                "r" => Ok(DeviceParamValue::Pan(1.0)),
                other => other
                    .parse::<f64>()
                    .ok()
                    .filter(|x| (-1.0..=1.0).contains(x))
                    .map(DeviceParamValue::Pan)
                    .ok_or_else(|| format!("expected a pan in -1..1 (or L/C/R), got '{s}'")),
            },
            Self::Toggle => match t.to_ascii_lowercase().as_str() {
                "on" | "true" | "1" | "yes" => Ok(DeviceParamValue::Toggle(true)),
                "off" | "false" | "0" | "no" => Ok(DeviceParamValue::Toggle(false)),
                _ => Err(format!("expected on/off, got '{s}'")),
            },
            Self::Enum { options } => {
                if let Some(i) = options.iter().position(|o| o.eq_ignore_ascii_case(t)) {
                    return u32::try_from(i)
                        .map(DeviceParamValue::Enum)
                        .map_err(|e| e.to_string());
                }
                t.parse::<u32>()
                    .ok()
                    .filter(|i| usize::try_from(*i).is_ok_and(|i| i < options.len()))
                    .map(DeviceParamValue::Enum)
                    .ok_or_else(|| {
                        format!(
                            "expected one of [{}] or an index, got '{s}'",
                            options.join(", ")
                        )
                    })
            }
            Self::Int { min, max } => t
                .parse::<i64>()
                .ok()
                .filter(|n| (*min..=*max).contains(n))
                .map(DeviceParamValue::Int)
                .ok_or_else(|| format!("expected an integer in {min}..={max}, got '{s}'")),
            Self::Text => Ok(DeviceParamValue::Text(s.to_owned())),
        }
    }

    /// Short kind name for listings (`level`, `enum`, …).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Level { .. } => "level",
            Self::Pan => "pan",
            Self::Toggle => "toggle",
            Self::Enum { .. } => "enum",
            Self::Int { .. } => "int",
            Self::Text => "text",
        }
    }
}

impl DeviceParamValue {
    /// Human rendering; enum indices resolve against `kind`'s options
    /// when given.
    #[must_use]
    pub fn display(&self, kind: Option<&DeviceParamKind>) -> String {
        match self {
            Self::Level(db) if db.is_infinite() && *db < 0.0 => "-inf dB".to_owned(),
            Self::Level(db) => format!("{db:.1} dB"),
            Self::Pan(p) => format!("{p:+.2}"),
            Self::Toggle(b) => if *b { "on" } else { "off" }.to_owned(),
            Self::Enum(i) => {
                let label = match kind {
                    Some(DeviceParamKind::Enum { options }) => usize::try_from(*i)
                        .ok()
                        .and_then(|i| options.get(i))
                        .cloned(),
                    _ => None,
                };
                label.map_or_else(|| format!("#{i}"), |l| format!("{l} (#{i})"))
            }
            Self::Int(n) => n.to_string(),
            Self::Text(s) => format!("{s:?}"),
        }
    }

    /// Equality with a float tolerance (device round-trips quantize
    /// levels/pans; a restore must not re-write a value that is
    /// already there within the device's own resolution).
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        const EPS: f64 = 0.02;
        match (self, other) {
            (Self::Level(a), Self::Level(b)) | (Self::Pan(a), Self::Pan(b)) => {
                (a.is_infinite() && b.is_infinite() && a.signum() == b.signum())
                    || (a - b).abs() < EPS
            }
            _ => self == other,
        }
    }
}

/// One device parameter as seen on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct ParamView {
    pub path: String,
    pub label: String,
    pub kind: DeviceParamKind,
    pub value: DeviceParamValue,
    pub writable: bool,
    /// Changing it interrupts audio; writes need `allow_disruptive`.
    pub disruptive: bool,
}

/// Full device state, read from the device at call time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceView {
    pub summary: DeviceSummary,
    /// Router source groups.
    pub inputs: Vec<DevicePortGroup>,
    /// Router destination groups.
    pub outputs: Vec<DevicePortGroup>,
    /// One entry per output channel.
    pub routes: Vec<DeviceCrosspoint>,
    pub params: Vec<ParamView>,
}

// ─── Events ─────────────────────────────────────────────────────────────

/// What changed on a device.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DeviceEventKind {
    /// A parameter took a new (device-confirmed) value.
    ParamChanged {
        path: String,
        value: DeviceParamValue,
    },
    /// A router cell changed.
    RouteChanged { crosspoint: DeviceCrosspoint },
    /// The control connection came up.
    Online,
    /// The control connection dropped (the hub reconnects).
    Offline,
    /// State changed wholesale — re-read with `device(id)`.
    SnapshotReplaced,
}

/// One device event, tagged with the device it came from. Streamed via
/// `#[subscribe] device_events` (all devices; filter client-side).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceEventWire {
    /// Device id (`vendor:model:serial`).
    pub device: String,
    pub event: DeviceEventKind,
}

// SelfRef compatibility: no lifetime parameters, so `Ref<'a>` is `Self`
// (same as `GraphEvent`).
#[allow(unsafe_code)]
unsafe impl vox_types::Reborrow for DeviceEventWire {
    type Ref<'a> = Self;
}

// ─── Config (styx `devices` section) ────────────────────────────────────

/// One configured device (the `devices` config section).
///
/// ```styx
/// devices ({name galaxy32, kind antelope-galaxy32, serial "4202524000109"}
///          {name tf1, kind yamaha-tf, enabled false})
/// ```
///
/// With no `devices` section at all, the engine brings up the default
/// set — `system-audio`, `galaxy32` (antelope-galaxy32), `tf1`
/// (yamaha-tf) and `dante` — each auto-discovered and failing soft (set
/// `PATCHBAY_DEVICES=off` to disable the device layer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct DeviceConfig {
    /// Unique entry name (upsert key; also a CLI alias).
    pub name: String,
    /// Adapter kind, e.g. `antelope-galaxy32`.
    pub kind: String,
    /// Pin to one unit by serial (empty = first found).
    #[serde(default)]
    #[facet(default)]
    pub serial: String,
    /// Pin to one control endpoint `host:port` (empty = discover).
    #[serde(default)]
    #[facet(default)]
    pub addr: String,
    /// Keep the entry but don't connect (legacy spelling of
    /// `enabled false`).
    #[serde(default)]
    #[facet(default)]
    pub disabled: bool,
    /// `false` keeps the entry but doesn't connect. Unset = enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub enabled: Option<bool>,
}

impl DeviceConfig {
    /// An enabled, auto-discovering entry.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            serial: String::new(),
            addr: String::new(),
            disabled: false,
            enabled: None,
        }
    }

    /// Whether the hub should connect this entry (`enabled` not `false`
    /// and not `disabled`).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.disabled && self.enabled != Some(false)
    }
}

// ─── Snapshots (named device settings) ──────────────────────────────────

/// One saved parameter value, in a flat hand-editable form: exactly
/// one of the typed fields is set, e.g.
/// `{path mixer/1/strip/16/level, level -12.5}` or
/// `{path monitor/dim, toggle true}`.
///
/// (Flat rather than a [`DeviceParamValue`] field because styx's
/// tagged-enum encoding of newtype variants doesn't round-trip.) Build
/// with [`DeviceParamSetting::new`], read with
/// [`DeviceParamSetting::value`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceParamSetting {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub level: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub pan: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub toggle: Option<bool>,
    /// Enum index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub choice: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub int: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[facet(default, skip_serializing_if = Option::is_none)]
    pub text: Option<String>,
}

impl DeviceParamSetting {
    #[must_use]
    pub fn new(path: impl Into<String>, value: &DeviceParamValue) -> Self {
        let mut s = Self {
            path: path.into(),
            ..Self::default()
        };
        match value {
            DeviceParamValue::Level(x) => s.level = Some(*x),
            DeviceParamValue::Pan(x) => s.pan = Some(*x),
            DeviceParamValue::Toggle(b) => s.toggle = Some(*b),
            DeviceParamValue::Enum(i) => s.choice = Some(*i),
            DeviceParamValue::Int(n) => s.int = Some(*n),
            DeviceParamValue::Text(t) => s.text = Some(t.clone()),
        }
        s
    }

    /// The saved value (`None` if no typed field is set — a malformed
    /// hand edit). The first set field wins.
    #[must_use]
    pub fn value(&self) -> Option<DeviceParamValue> {
        self.level
            .map(DeviceParamValue::Level)
            .or_else(|| self.pan.map(DeviceParamValue::Pan))
            .or_else(|| self.toggle.map(DeviceParamValue::Toggle))
            .or_else(|| self.choice.map(DeviceParamValue::Enum))
            .or_else(|| self.int.map(DeviceParamValue::Int))
            .or_else(|| self.text.clone().map(DeviceParamValue::Text))
    }
}

/// One saved router cell: `path` = `route/<OUTPUT_GROUP>/<1-based ch>`,
/// `source` = `GROUP:N` (1-based) or empty for unpatched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct DeviceRouteSetting {
    pub path: String,
    #[serde(default)]
    #[facet(default)]
    pub source: String,
}

/// A named device snapshot, persisted in the config
/// (`device_snapshots` section). Only writable params are stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceSettingSnapshot {
    /// Unique name (upsert key).
    pub name: String,
    /// Device id it was taken from (and restores to).
    pub device: String,
    /// Unix seconds at save.
    #[serde(default)]
    #[facet(default)]
    pub created: u64,
    /// Path prefixes that were included (empty = everything).
    #[serde(default)]
    #[facet(default)]
    pub include: Vec<String>,
    /// Path prefixes that were excluded.
    #[serde(default)]
    #[facet(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    #[facet(default)]
    pub params: Vec<DeviceParamSetting>,
    #[serde(default)]
    #[facet(default)]
    pub routes: Vec<DeviceRouteSetting>,
}

/// Listing entry for a saved snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceSnapshotInfo {
    pub name: String,
    pub device: String,
    pub created: u64,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub params: u32,
    pub routes: u32,
}

impl DeviceSettingSnapshot {
    /// The listing form.
    #[must_use]
    pub fn info(&self) -> DeviceSnapshotInfo {
        DeviceSnapshotInfo {
            name: self.name.clone(),
            device: self.device.clone(),
            created: self.created,
            include: self.include.clone(),
            exclude: self.exclude.clone(),
            params: u32::try_from(self.params.len()).unwrap_or(u32::MAX),
            routes: u32::try_from(self.routes.len()).unwrap_or(u32::MAX),
        }
    }
}

/// Whether `path` is inside `prefix`, by whole path segments.
///
/// `mixer/1/strip/1` covers `mixer/1/strip/1/level` but NOT
/// `mixer/1/strip/16/level`. A trailing `/` on the prefix is ignored;
/// an empty prefix covers everything.
#[must_use]
pub fn path_in_prefix(path: &str, prefix: &str) -> bool {
    let p = prefix.trim().trim_end_matches('/');
    if p.is_empty() {
        return true;
    }
    path == p
        || path
            .strip_prefix(p)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Include/exclude filter: included by any `include` prefix (or
/// `include` empty) and by no `exclude` prefix.
#[must_use]
pub fn path_selected(path: &str, include: &[String], exclude: &[String]) -> bool {
    (include.is_empty() || include.iter().any(|p| path_in_prefix(path, p)))
        && !exclude.iter().any(|p| path_in_prefix(path, p))
}

/// Per-item status in a restore plan / report.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRestoreStatus {
    /// Differs from live; would be written (dry run / diff).
    Planned,
    /// Written and confirmed by the device.
    Applied,
    /// Write attempted and refused / not confirmed (see `error`).
    Failed,
    /// Disruptive param; needs `allow_disruptive`.
    SkippedDisruptive,
    /// The live device reports it read-only.
    SkippedReadOnly,
    /// The live device has no such param / output.
    SkippedMissing,
}

/// One snapshot item that differs from the live device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceRestoreItem {
    /// Param path or `route/<GROUP>/<N>`.
    pub path: String,
    /// Live value (display form).
    pub current: String,
    /// Snapshot value (display form).
    pub target: String,
    pub status: DeviceRestoreStatus,
    /// Error text for `Failed` (empty otherwise).
    pub error: String,
}

/// Outcome of `diff_device_snapshot` / `restore_device_snapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
pub struct DeviceRestoreReport {
    pub snapshot: String,
    pub device: String,
    /// Nothing was written.
    pub dry_run: bool,
    /// Snapshot items already equal to live (not listed).
    pub unchanged: u32,
    /// Every differing item, in snapshot order (params, then routes).
    pub items: Vec<DeviceRestoreItem>,
}

impl DeviceRestoreReport {
    /// Items with `status`.
    #[must_use]
    pub fn count(&self, status: DeviceRestoreStatus) -> usize {
        self.items.iter().filter(|i| i.status == status).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matching_is_segment_aware() {
        assert!(path_in_prefix("mixer/1/strip/16/level", "mixer/1/strip/16"));
        assert!(path_in_prefix(
            "mixer/1/strip/16/level",
            "mixer/1/strip/16/"
        ));
        assert!(path_in_prefix("mixer/1/strip/16", "mixer/1/strip/16"));
        assert!(!path_in_prefix("mixer/1/strip/16/level", "mixer/1/strip/1"));
        assert!(path_in_prefix("anything", ""));
        assert!(path_selected(
            "route/DIGI_OUT0/3",
            &["route/DIGI_OUT0".into()],
            &[]
        ));
        assert!(!path_selected("route/DIGI_OUT0/3", &[], &["route".into()]));
    }

    #[test]
    fn channel_labels_are_one_based() {
        let c = DeviceChannel::new("DIGI_OUT0", 16);
        assert_eq!(c.label(), "DIGI_OUT0:17");
        assert_eq!(DeviceChannel::parse_label("DIGI_OUT0:17"), Some(c.clone()));
        assert_eq!(DeviceChannel::parse_label("DIGI_OUT0:0"), None);
        assert_eq!(route_path(&c), "route/DIGI_OUT0/17");
        assert_eq!(parse_route_path("route/DIGI_OUT0/17"), Some(c));
        assert_eq!(parse_route_path("mixer/1"), None);
    }

    #[test]
    fn values_parse_per_kind() {
        let level = DeviceParamKind::Level {
            min_db: -96.0,
            max_db: 0.0,
        };
        assert_eq!(
            level.parse_value("-12dB"),
            Ok(DeviceParamValue::Level(-12.0))
        );
        assert_eq!(
            level.parse_value("-inf"),
            Ok(DeviceParamValue::Level(f64::NEG_INFINITY))
        );
        assert!(level.parse_value("loud").is_err());
        assert_eq!(
            DeviceParamKind::Toggle.parse_value("ON"),
            Ok(DeviceParamValue::Toggle(true))
        );
        assert_eq!(
            DeviceParamKind::Pan.parse_value("L"),
            Ok(DeviceParamValue::Pan(-1.0))
        );
        assert!(DeviceParamKind::Pan.parse_value("2").is_err());
        let e = DeviceParamKind::Enum {
            options: vec!["ALL".into(), "MANUAL".into()],
        };
        assert_eq!(e.parse_value("manual"), Ok(DeviceParamValue::Enum(1)));
        assert_eq!(e.parse_value("0"), Ok(DeviceParamValue::Enum(0)));
        assert!(e.parse_value("2").is_err());
        assert_eq!(
            DeviceParamValue::Enum(1).display(Some(&e)),
            "MANUAL (#1)".to_owned()
        );
    }

    #[test]
    fn infinite_levels_are_explicit_in_json() {
        let v = DeviceParamValue::Level(f64::NEG_INFINITY);
        let j = serde_json::to_string(&v).unwrap();
        assert_eq!(j, r#"{"type":"level","value":"-inf"}"#);
        assert_eq!(serde_json::from_str::<DeviceParamValue>(&j).unwrap(), v);
        let finite = serde_json::to_string(&DeviceParamValue::Level(-12.5)).unwrap();
        assert_eq!(finite, r#"{"type":"level","value":-12.5}"#);
        assert_eq!(
            serde_json::from_str::<DeviceParamValue>(r#"{"type":"level","value":null}"#).unwrap(),
            v
        );
        assert_eq!(
            serde_json::from_str::<DeviceParamValue>(r#"{"type":"level","value":-3}"#).unwrap(),
            DeviceParamValue::Level(-3.0)
        );
        let k = DeviceParamKind::Level {
            min_db: f64::NEG_INFINITY,
            max_db: 10.0,
        };
        let j = serde_json::to_string(&k).unwrap();
        assert_eq!(j, r#"{"kind":"level","min_db":"-inf","max_db":10.0}"#);
        assert_eq!(serde_json::from_str::<DeviceParamKind>(&j).unwrap(), k);
    }

    #[test]
    fn same_as_tolerates_quantization() {
        assert!(DeviceParamValue::Level(-12.0).same_as(&DeviceParamValue::Level(-12.01)));
        assert!(!DeviceParamValue::Level(-12.0).same_as(&DeviceParamValue::Level(-13.0)));
        assert!(
            DeviceParamValue::Level(f64::NEG_INFINITY)
                .same_as(&DeviceParamValue::Level(f64::NEG_INFINITY))
        );
        assert!(!DeviceParamValue::Toggle(true).same_as(&DeviceParamValue::Int(1)));
    }
}
