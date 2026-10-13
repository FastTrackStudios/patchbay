//! Parameters: path-addressed, typed device controls and metadata.

use serde::{Deserialize, Serialize};

/// What a parameter is and which values it accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamKind {
    /// A gain in dB. Values are [`ParamValue::Level`]; `min_db` is the
    /// bottom of the range (adapters document whether it means -inf).
    Level {
        /// Lowest value in dB.
        min_db: f64,
        /// Highest value in dB.
        max_db: f64,
    },
    /// Stereo position, [`ParamValue::Pan`] in `-1.0 (L) ..= 1.0 (R)`.
    Pan,
    /// On/off (mute, solo, dim, phantom…), [`ParamValue::Toggle`].
    Toggle,
    /// One of a fixed set, [`ParamValue::Enum`] holding the index.
    Enum {
        /// Display labels, indexed by the value.
        options: Vec<String>,
    },
    /// An integer in `min..=max`, [`ParamValue::Int`].
    Int {
        /// Inclusive minimum.
        min: i64,
        /// Inclusive maximum.
        max: i64,
    },
    /// Free text (channel names, colours as `#rrggbb`), [`ParamValue::Text`].
    Text,
}

/// A parameter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ParamValue {
    /// dB.
    Level(f64),
    /// `-1.0 ..= 1.0`.
    Pan(f64),
    /// On/off.
    Toggle(bool),
    /// Index into [`ParamKind::Enum::options`].
    Enum(u32),
    /// Integer.
    Int(i64),
    /// Text.
    Text(String),
}

impl ParamValue {
    /// Whether this value's variant fits `kind` (range is not checked).
    #[must_use]
    pub const fn matches(&self, kind: &ParamKind) -> bool {
        matches!(
            (self, kind),
            (Self::Level(_), ParamKind::Level { .. })
                | (Self::Pan(_), ParamKind::Pan)
                | (Self::Toggle(_), ParamKind::Toggle)
                | (Self::Enum(_), ParamKind::Enum { .. })
                | (Self::Int(_), ParamKind::Int { .. })
                | (Self::Text(_), ParamKind::Text)
        )
    }
}

/// One device parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    /// Slash path, unique per device, e.g. `mixer/1/strip/16/level`.
    pub path: String,
    /// Display label.
    pub label: String,
    /// Type and accepted range.
    pub kind: ParamKind,
    /// Current value (as last read from the device).
    pub value: ParamValue,
    /// Whether [`crate::DeviceAdapter::set_param`] can change it at all.
    pub writable: bool,
    /// Whether changing it interrupts audio (clock source, sample rate,
    /// …). Such writes are refused unless made with
    /// [`WriteGuard::AllowDisruptive`].
    pub disruptive: bool,
}

/// Write permission level for [`crate::DeviceAdapter::apply_param`].
///
/// Disruptive parameters (clock/sample rate) drop audio on a live rig,
/// so they need an explicit opt-in at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WriteGuard {
    /// Refuse writes to parameters flagged [`Param::disruptive`].
    #[default]
    Normal,
    /// The caller has confirmed an audio-interrupting change.
    AllowDisruptive,
}
