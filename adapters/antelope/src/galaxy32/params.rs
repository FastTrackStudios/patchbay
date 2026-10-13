//! Galaxy32 ↔ generic parameter mapping: path grammar, descriptors, unit
//! conversions, and notification → [`DeviceEvent`] translation.
//!
//! Paths (1-based, like the panel):
//!
//! | path | kind |
//! |---|---|
//! | `mixer/{1-4}/strip/{1-32}/{level,pan,mute,solo,send}` | Level / Pan / Toggle |
//! | `mixer/{1-4}/master/{level,pan,mute,solo,send}` | same |
//! | `mixer/{1-4}/reverb/{room_size,…,on}` | Int 0..=255 (`on`: Toggle) |
//! | `monitor/{volume,mute,dim,mono}` | Int (raw) / Toggle |
//! | `trim/line_in/control` | Enum ALL/MANUAL |
//! | `trim/line_in/{1-32}` | Level (dBu, 22.0 = no attenuation) |
//! | `clock/{sync_source,sample_rate}` | Enum, **disruptive** |
//! | `clock/{measured_rate,locked}` | read-only |
//! | `afx/strip/{1-32}/slot/{1-8}/effect` | Enum (index = effect type id) |
//! | `afx/strip/{1-32}/slot/{1-8}/{field}` | Int/raw, write-only (see afx.rs) |

use patchbay_device::{ChannelRef, Crosspoint, DeviceEvent, ParamKind, ParamValue};
use serde_json::Value;

use super::afx::AfxCatalog;
use super::ops::{DeviceState, MonitorToggle, ReverbConfig, RouteSlot, TrimLevel};
use super::tables::{
    self, AFX_SLOTS, AFX_STRIPS, ATTENUATION_OFF, MIXER_STRIPS, MIXERS, PAN_CENTER, PAN_LEFT,
    PAN_RIGHT, ROUTING_SLOTS, SAMPLE_RATES, SYNC_SOURCES, TRIM_CONTROL_OPTIONS, TRIM_LINE_IN,
    TRIM_REFERENCE_DBU,
};
use crate::protocol::call::Call;

/// Mixer strip field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripField {
    Level,
    Pan,
    Mute,
    Solo,
    Send,
}

impl StripField {
    pub(crate) const ALL: [Self; 5] = [Self::Level, Self::Pan, Self::Mute, Self::Solo, Self::Send];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Level => "level",
            Self::Pan => "pan",
            Self::Mute => "mute",
            Self::Solo => "solo",
            Self::Send => "send",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == s)
    }
}

/// A parsed parameter path (indices 0-based internally).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParamPath {
    Strip {
        mixer: u8,
        ch: u8,
        field: StripField,
    },
    Reverb {
        mixer: u8,
        field: &'static str,
    },
    MonitorVolume,
    MonitorToggle(MonitorToggle),
    TrimControl,
    TrimLevel {
        ch: u8,
    },
    SyncSource,
    SampleRate,
    MeasuredRate,
    Locked,
    AfxEffect {
        strip: u8,
        slot: u8,
    },
    AfxField {
        strip: u8,
        slot: u8,
        field: String,
    },
}

/// Parse a 1-based index in `1..=max` into 0-based.
fn one_based(s: &str, max: u8) -> Option<u8> {
    let n: u8 = s.parse().ok()?;
    (1..=max).contains(&n).then(|| n.saturating_sub(1))
}

impl ParamPath {
    pub(crate) fn parse(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').collect();
        Some(match parts.as_slice() {
            ["mixer", m, "strip", ch, f] => Self::Strip {
                mixer: one_based(m, MIXERS)?,
                // Strips are 1-based on the wire too (0 = master).
                ch: one_based(ch, MIXER_STRIPS)?.saturating_add(1),
                field: StripField::parse(f)?,
            },
            ["mixer", m, "master", f] => Self::Strip {
                mixer: one_based(m, MIXERS)?,
                ch: tables::MIXER_MASTER,
                field: StripField::parse(f)?,
            },
            ["mixer", m, "reverb", f] => Self::Reverb {
                mixer: one_based(m, MIXERS)?,
                field: ReverbConfig::FIELDS.into_iter().find(|x| x == f)?,
            },
            ["monitor", "volume"] => Self::MonitorVolume,
            ["monitor", "dim"] => Self::MonitorToggle(MonitorToggle::Dim),
            ["monitor", "mute"] => Self::MonitorToggle(MonitorToggle::Mute),
            ["monitor", "mono"] => Self::MonitorToggle(MonitorToggle::Mono),
            ["trim", "line_in", "control"] => Self::TrimControl,
            ["trim", "line_in", ch] => Self::TrimLevel {
                ch: one_based(ch, 32)?,
            },
            ["clock", "sync_source"] => Self::SyncSource,
            ["clock", "sample_rate"] => Self::SampleRate,
            ["clock", "measured_rate"] => Self::MeasuredRate,
            ["clock", "locked"] => Self::Locked,
            ["afx", "strip", s, "slot", k, "effect"] => Self::AfxEffect {
                strip: one_based(s, AFX_STRIPS)?,
                slot: one_based(k, u8::try_from(AFX_SLOTS).ok()?)?,
            },
            ["afx", "strip", s, "slot", k, f] => Self::AfxField {
                strip: one_based(s, AFX_STRIPS)?,
                slot: one_based(k, u8::try_from(AFX_SLOTS).ok()?)?,
                field: (*f).to_owned(),
            },
            _ => return None,
        })
    }
}

// ── Path builders (1-based output) ────────────────────────────────────

pub(crate) fn strip_path(mixer: u8, ch: u8, f: StripField) -> String {
    let m = u16::from(mixer).saturating_add(1);
    if ch == tables::MIXER_MASTER {
        format!("mixer/{m}/master/{}", f.name())
    } else {
        format!("mixer/{m}/strip/{ch}/{}", f.name())
    }
}

pub(crate) fn reverb_path(mixer: u8, field: &str) -> String {
    format!(
        "mixer/{}/reverb/{field}",
        u16::from(mixer).saturating_add(1)
    )
}

pub(crate) fn trim_path(ch: u8) -> String {
    format!("trim/line_in/{}", u16::from(ch).saturating_add(1))
}

pub(crate) fn afx_effect_path(strip: u8, slot: u8) -> String {
    format!(
        "afx/strip/{}/slot/{}/effect",
        u16::from(strip).saturating_add(1),
        u16::from(slot).saturating_add(1)
    )
}

pub(crate) fn monitor_path(t: MonitorToggle) -> String {
    format!("monitor/{}", t.state_key())
}

// ── Descriptors ───────────────────────────────────────────────────────

/// Static facts about a parameter.
pub(crate) struct Descriptor {
    pub(crate) label: String,
    pub(crate) kind: ParamKind,
    pub(crate) writable: bool,
    pub(crate) disruptive: bool,
}

const fn level_kind() -> ParamKind {
    ParamKind::Level {
        min_db: -96.0,
        max_db: 0.0,
    }
}

fn names(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_owned()).collect()
}

/// AFX effect enum options: index == effect type id (0 = none, gaps are
/// `type N`).
pub(crate) fn afx_options(cat: &AfxCatalog) -> Vec<String> {
    let max = cat
        .insertable()
        .filter_map(|e| e.type_id)
        .max()
        .unwrap_or(0);
    (0..=max)
        .map(|id| {
            if id == 0 {
                "none".to_owned()
            } else {
                cat.by_type(id)
                    .map_or_else(|| format!("type {id}"), |e| e.name.clone())
            }
        })
        .collect()
}

/// Clock params: writes interrupt audio, measured values are read-only.
fn clock_descriptor(p: &ParamPath) -> Descriptor {
    match p {
        ParamPath::SyncSource => Descriptor {
            label: "Clock source".into(),
            kind: ParamKind::Enum {
                options: names(&SYNC_SOURCES),
            },
            writable: true,
            disruptive: true,
        },
        ParamPath::SampleRate => Descriptor {
            label: "Sample rate".into(),
            kind: ParamKind::Enum {
                options: SAMPLE_RATES.iter().map(ToString::to_string).collect(),
            },
            writable: true,
            disruptive: true,
        },
        ParamPath::MeasuredRate => Descriptor {
            label: "Measured sample rate (Hz)".into(),
            kind: ParamKind::Int {
                min: 0,
                max: i64::from(u32::MAX),
            },
            writable: false,
            disruptive: false,
        },
        ParamPath::Locked => Descriptor {
            label: "Clock locked".into(),
            kind: ParamKind::Toggle,
            writable: false,
            disruptive: false,
        },
        _ => Descriptor {
            label: String::new(),
            kind: ParamKind::Text,
            writable: false,
            disruptive: false,
        },
    }
}

pub(crate) fn describe(p: &ParamPath, afx_options: &[String]) -> Descriptor {
    let d = |label: String, kind: ParamKind| Descriptor {
        label,
        kind,
        writable: true,
        disruptive: false,
    };
    match p {
        ParamPath::Strip { mixer, ch, field } => {
            let who = if *ch == tables::MIXER_MASTER {
                "master".to_owned()
            } else {
                format!("ch {ch}")
            };
            let label = format!(
                "MIX{} {who} {}",
                u16::from(*mixer).saturating_add(1),
                field.name()
            );
            let kind = match field {
                StripField::Level | StripField::Send => level_kind(),
                StripField::Pan => ParamKind::Pan,
                StripField::Mute | StripField::Solo => ParamKind::Toggle,
            };
            d(label, kind)
        }
        ParamPath::Reverb { mixer, field } => d(
            format!("MIX{} reverb {field}", u16::from(*mixer).saturating_add(1)),
            if *field == "on" {
                ParamKind::Toggle
            } else {
                ParamKind::Int { min: 0, max: 255 }
            },
        ),
        ParamPath::MonitorVolume => d(
            "Monitor volume (raw)".into(),
            ParamKind::Int {
                min: 0,
                max: i64::from(u16::MAX),
            },
        ),
        ParamPath::MonitorToggle(t) => d(format!("Monitor {}", t.state_key()), ParamKind::Toggle),
        ParamPath::TrimControl => d(
            "LINE IN trim mode".into(),
            ParamKind::Enum {
                options: names(&TRIM_CONTROL_OPTIONS),
            },
        ),
        ParamPath::TrimLevel { ch } => d(
            format!("LINE IN {} trim (dBu)", u16::from(*ch).saturating_add(1)),
            // TODO(trim-range): bottom of the trim range is unconfirmed.
            ParamKind::Level {
                min_db: 0.0,
                max_db: TRIM_REFERENCE_DBU,
            },
        ),
        ParamPath::SyncSource
        | ParamPath::SampleRate
        | ParamPath::MeasuredRate
        | ParamPath::Locked => clock_descriptor(p),
        ParamPath::AfxEffect { strip, slot } => d(
            format!(
                "AFX {} slot {} effect",
                u16::from(*strip).saturating_add(1),
                u16::from(*slot).saturating_add(1)
            ),
            ParamKind::Enum {
                options: afx_options.to_vec(),
            },
        ),
        ParamPath::AfxField { strip, slot, field } => d(
            format!(
                "AFX {} slot {} {field}",
                u16::from(*strip).saturating_add(1),
                u16::from(*slot).saturating_add(1)
            ),
            ParamKind::Int {
                min: i64::from(i32::MIN),
                max: i64::from(u32::MAX),
            },
        ),
    }
}

// ── Unit conversions (no `as`: search-based float → int) ─────────────

/// Round a finite float to a `u8` in `lo..=hi` (clamped).
pub(crate) fn round_clamped_u8(x: f64, lo: u8, hi: u8) -> Option<u8> {
    if x.is_nan() {
        return None;
    }
    let r = x.round().clamp(f64::from(lo), f64::from(hi));
    (lo..=hi).find(|n| (f64::from(*n) - r).abs() < 0.5)
}

/// Round a finite, non-negative float to a `u16`.
pub(crate) fn round_u16(x: f64) -> Option<u16> {
    if !x.is_finite() || x < 0.0 || x > f64::from(u16::MAX) {
        return None;
    }
    // Binary search over the integer range: no lossy casts.
    let r = x.round();
    let (mut lo, mut hi) = (0u16, u16::MAX);
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if f64::from(mid) < r {
            lo = mid.saturating_add(1);
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// Wire attenuation → dB (0 → 0.0, 26 → -26.0).
pub(crate) fn attenuation_db(att: u8) -> f64 {
    -f64::from(att)
}

/// dB → wire attenuation, clamped to `0..=96` (-inf → 96).
pub(crate) fn db_attenuation(db: f64) -> Option<u8> {
    if db == f64::NEG_INFINITY {
        return Some(ATTENUATION_OFF);
    }
    round_clamped_u8(-db, 0, ATTENUATION_OFF)
}

/// Wire pan (2..=62, 32 centre) → -1.0..=1.0.
pub(crate) fn pan_f64(pan: u8) -> f64 {
    ((f64::from(pan) - f64::from(PAN_CENTER)) / 30.0).clamp(-1.0, 1.0)
}

/// -1.0..=1.0 → wire pan.
pub(crate) fn f64_pan(p: f64) -> Option<u8> {
    if !p.is_finite() {
        return None;
    }
    round_clamped_u8(
        f64::from(PAN_CENTER) + p.clamp(-1.0, 1.0) * 30.0,
        PAN_LEFT,
        PAN_RIGHT,
    )
}

/// dBu → trim level (tenths assumed for `fract`).
pub(crate) fn dbu_trim(dbu: f64) -> Option<TrimLevel> {
    if !dbu.is_finite() || dbu > TRIM_REFERENCE_DBU {
        return None;
    }
    let tenths = round_u16((TRIM_REFERENCE_DBU - dbu) * 10.0)?;
    Some(TrimLevel {
        whole: u8::try_from(tenths.checked_div(10)?).ok()?,
        fract: u8::try_from(tenths.checked_rem(10)?).ok()?,
    })
}

pub(crate) fn strip_value(field: StripField, s: super::ops::MixerStrip) -> ParamValue {
    match field {
        StripField::Level => ParamValue::Level(attenuation_db(s.level)),
        StripField::Send => ParamValue::Level(attenuation_db(s.send)),
        StripField::Pan => ParamValue::Pan(pan_f64(s.pan)),
        StripField::Mute => ParamValue::Toggle(s.mute != 0),
        StripField::Solo => ParamValue::Toggle(s.solo != 0),
    }
}

// ── Router mapping ────────────────────────────────────────────────────

/// Wire slot → generic source (`None` for type 18 / unknown types).
pub(crate) fn slot_source(slot: RouteSlot) -> Option<ChannelRef> {
    tables::source_type(slot.ty).map(|s| ChannelRef::new(s.id, u16::from(slot.ch)))
}

/// Crosspoints of one page (only its meaningful channels).
pub(crate) fn page_crosspoints(page: u8, slots: &[RouteSlot]) -> Vec<Crosspoint> {
    let Some(p) = tables::output_page(page) else {
        return Vec::new();
    };
    slots
        .iter()
        .zip(0..p.channels)
        .map(|(slot, ch)| Crosspoint {
            output: ChannelRef::new(p.id, ch),
            source: slot_source(*slot),
        })
        .collect()
}

// ── State / notification → events ────────────────────────────────────

const fn changed(path: String, value: ParamValue) -> DeviceEvent {
    DeviceEvent::ParamChanged { path, value }
}

/// Params derived from cyclic state (monitor + clock).
pub(crate) fn state_params(s: &DeviceState) -> Vec<(String, ParamValue)> {
    let mut v = vec![
        (
            "monitor/volume".to_owned(),
            ParamValue::Int(i64::from(s.monitor.volume)),
        ),
        (
            monitor_path(MonitorToggle::Mute),
            ParamValue::Toggle(s.monitor.mute),
        ),
        (
            monitor_path(MonitorToggle::Dim),
            ParamValue::Toggle(s.monitor.dim),
        ),
        (
            monitor_path(MonitorToggle::Mono),
            ParamValue::Toggle(s.monitor.mono),
        ),
        (
            "clock/sync_source".to_owned(),
            ParamValue::Enum(u32::from(s.sync_source)),
        ),
        (
            "clock/measured_rate".to_owned(),
            ParamValue::Int(i64::from(s.sample_rate)),
        ),
        ("clock/locked".to_owned(), ParamValue::Toggle(s.locked)),
    ];
    if let Some(i) = tables::sample_rate_index(s.sample_rate).and_then(|i| u32::try_from(i).ok()) {
        v.push(("clock/sample_rate".to_owned(), ParamValue::Enum(i)));
    }
    v
}

/// Events for params that differ between two cyclic states.
pub(crate) fn state_events(prev: Option<&DeviceState>, next: &DeviceState) -> Vec<DeviceEvent> {
    let old = prev.map(state_params).unwrap_or_default();
    state_params(next)
        .into_iter()
        .filter(|(p, v)| !old.iter().any(|(op, ov)| op == p && ov == v))
        .map(|(p, v)| changed(p, v))
        .collect()
}

fn arg_u8(call: &Call, i: usize) -> Option<u8> {
    call.args
        .get(i)?
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
}

fn cell_pair(v: &Value) -> Option<(u8, u8)> {
    let a = v.as_array()?;
    let x = u8::try_from(a.first()?.as_u64()?).ok()?;
    let y = u8::try_from(a.get(1)?.as_u64()?).ok()?;
    Some((x, y))
}

/// Translate a rebroadcast write (another client's `set_*`) into events.
pub(crate) fn notification_events(call: &Call) -> Vec<DeviceEvent> {
    let mut out = Vec::new();
    match call.method.as_str() {
        "set_mixer" => {
            let (Some(mixer), Some(ch)) = (arg_u8(call, 0), arg_u8(call, 1)) else {
                return out;
            };
            for f in StripField::ALL {
                let Some(raw) = call
                    .kwarg_value(f.name())
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok())
                else {
                    continue;
                };
                let strip = super::ops::MixerStrip {
                    level: raw,
                    pan: raw,
                    mute: raw,
                    solo: raw,
                    send: raw,
                };
                out.push(changed(strip_path(mixer, ch, f), strip_value(f, strip)));
            }
        }
        "set_routing" => {
            let (Some(page), Some(cells)) =
                (arg_u8(call, 0), call.args.get(1).and_then(Value::as_array))
            else {
                return out;
            };
            let slots: Vec<RouteSlot> = cells
                .iter()
                .filter_map(cell_pair)
                .map(|(ty, ch)| RouteSlot { ty, ch })
                .collect();
            out.extend(
                page_crosspoints(page, &slots)
                    .into_iter()
                    .map(DeviceEvent::RouteChanged),
            );
        }
        "set_trim_config" => {
            let (Some(TRIM_LINE_IN), Some(control), Some(cells)) = (
                arg_u8(call, 0),
                arg_u8(call, 1),
                call.args.get(2).and_then(Value::as_array),
            ) else {
                return out;
            };
            out.push(changed(
                "trim/line_in/control".into(),
                ParamValue::Enum(u32::from(control)),
            ));
            for (ch, (whole, fract)) in
                (0..).zip(cells.iter().filter_map(cell_pair).take(ROUTING_SLOTS))
            {
                let l = TrimLevel { whole, fract };
                out.push(changed(trim_path(ch), ParamValue::Level(l.dbu())));
            }
        }
        "set_afx_order" => {
            let (Some(strip), Some(slots)) =
                (arg_u8(call, 0), call.args.get(1).and_then(Value::as_array))
            else {
                return out;
            };
            let types: Vec<u8> = slots
                .iter()
                .filter_map(|s| s.get("type")?.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect();
            for slot in 0..u8::try_from(AFX_SLOTS).unwrap_or(u8::MAX) {
                let ty = types.get(usize::from(slot)).copied().unwrap_or(0);
                out.push(changed(
                    afx_effect_path(strip, slot),
                    ParamValue::Enum(u32::from(ty)),
                ));
            }
        }
        "set_reverb_config" => {
            let Some(mixer) = arg_u8(call, 0) else {
                return out;
            };
            for (i, f) in ReverbConfig::FIELDS.iter().enumerate() {
                let Some(v) = arg_u8(call, i.saturating_add(1)) else {
                    continue;
                };
                let value = if *f == "on" {
                    ParamValue::Toggle(v != 0)
                } else {
                    ParamValue::Int(i64::from(v))
                };
                out.push(changed(reverb_path(mixer, f), value));
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn path_roundtrip() {
        for (m, ch) in [(0, 0), (0, 16), (3, 32)] {
            for f in StripField::ALL {
                let p = strip_path(m, ch, f);
                assert_eq!(
                    ParamPath::parse(&p),
                    Some(ParamPath::Strip {
                        mixer: m,
                        ch,
                        field: f
                    }),
                    "{p}"
                );
            }
        }
        assert_eq!(
            strip_path(0, 16, StripField::Level),
            "mixer/1/strip/16/level"
        );
        assert_eq!(strip_path(0, 0, StripField::Pan), "mixer/1/master/pan");
        assert_eq!(
            ParamPath::parse(&trim_path(31)),
            Some(ParamPath::TrimLevel { ch: 31 })
        );
        assert_eq!(
            ParamPath::parse(&afx_effect_path(15, 0)),
            Some(ParamPath::AfxEffect { strip: 15, slot: 0 })
        );
        assert_eq!(
            ParamPath::parse("afx/strip/16/slot/1/gain"),
            Some(ParamPath::AfxField {
                strip: 15,
                slot: 0,
                field: "gain".into()
            })
        );
        for bad in [
            "mixer/0/strip/1/level",
            "mixer/1/strip/33/level",
            "mixer/1/strip/1/x",
            "afx/strip/1/slot/9/effect",
            "",
        ] {
            assert_eq!(ParamPath::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn conversions() {
        assert_eq!(db_attenuation(-26.0), Some(26));
        assert_eq!(db_attenuation(0.0), Some(0));
        assert_eq!(db_attenuation(3.0), Some(0), "no gain above unity");
        assert_eq!(db_attenuation(f64::NEG_INFINITY), Some(ATTENUATION_OFF));
        assert_eq!(db_attenuation(-200.0), Some(ATTENUATION_OFF));
        assert_eq!(db_attenuation(f64::NAN), None);
        assert!((attenuation_db(26) + 26.0).abs() < 1e-9);
        assert_eq!(f64_pan(0.0), Some(PAN_CENTER));
        assert_eq!(f64_pan(-1.0), Some(PAN_LEFT));
        assert_eq!(f64_pan(1.0), Some(PAN_RIGHT));
        assert!((pan_f64(62) - 1.0).abs() < 1e-9);
        assert!((pan_f64(2) + 1.0).abs() < 1e-9);
        assert_eq!(dbu_trim(22.0), Some(TrimLevel { whole: 0, fract: 0 }));
        assert_eq!(dbu_trim(21.0), Some(TrimLevel { whole: 1, fract: 0 }));
        assert_eq!(dbu_trim(12.5), Some(TrimLevel { whole: 9, fract: 5 }));
        assert_eq!(dbu_trim(22.5), None);
        assert_eq!(round_u16(48_000.4), Some(48_000));
        assert_eq!(round_u16(-1.0), None);
        assert_eq!(round_clamped_u8(300.0, 0, 255), Some(255));
    }

    #[test]
    fn routing_notification_events() {
        let mut cells: Vec<Value> = (0..32).map(|c| serde_json::json!([3, c])).collect();
        cells[0] = serde_json::json!([18, 0]);
        let call = Call::new("set_routing").arg(10).arg(cells);
        let ev = notification_events(&call);
        // ADAT OUT has 8 meaningful slots.
        assert_eq!(ev.len(), 8);
        assert_eq!(
            ev[0],
            DeviceEvent::RouteChanged(Crosspoint {
                output: ChannelRef::new("ADAT_OUT0", 0),
                source: None
            })
        );
        assert_eq!(
            ev[1],
            DeviceEvent::RouteChanged(Crosspoint {
                output: ChannelRef::new("ADAT_OUT0", 1),
                source: Some(ChannelRef::new("DANTE_IN0", 1))
            })
        );
    }

    #[test]
    fn state_diff_events() {
        let base = DeviceState {
            sync_source: 0,
            sample_rate: 48_000,
            locked: true,
            monitor: super::super::ops::MonitorState {
                volume: 33,
                mute: false,
                dim: false,
                mono: false,
            },
        };
        let mut dimmed = base;
        dimmed.monitor.dim = true;
        assert_eq!(
            state_events(Some(&base), &dimmed),
            vec![DeviceEvent::ParamChanged {
                path: "monitor/dim".into(),
                value: ParamValue::Toggle(true)
            }]
        );
        assert!(state_events(Some(&base), &base).is_empty());
    }
}
