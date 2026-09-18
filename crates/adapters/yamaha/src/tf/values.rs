//! Wire value ↔ generic value conversion.

use patchbay_device::{ParamKind, ParamValue};

use crate::rcp::command::RcpValue;
use crate::tf::table::ValueKind;
use crate::tf::vocab::Vocab;

/// Wire sentinel for -inf dB.
pub const NEG_INF_RAW: i64 = -32768;
/// Lowest finite fader/send step in dB (-138.00 dB = -13800).
pub const MIN_FINITE_DB: f64 = -138.0;
/// Highest level in dB (+10.00 dB = 1000).
pub const MAX_DB: f64 = 10.0;
/// Pan/balance extent on the wire (-63 = L63 … +63 = R63).
pub const PAN_RANGE: i32 = 63;
/// `PrePost` options (index = wire value).
pub const PRE_POST: [&str; 2] = ["post", "pre"];

/// Round a finite `f64` to an `i32` without `as` (fails when non-finite
/// or out of range).
#[must_use]
pub fn round_i32(v: f64) -> Option<i32> {
    if !v.is_finite() || v > f64::from(i32::MAX) || v < f64::from(i32::MIN) {
        return None;
    }
    format!("{:.0}", v.round()).parse().ok()
}

/// dB×100 → dB; -32768 (and anything below the representable range) → -inf.
#[must_use]
pub fn raw_to_db(raw: i64) -> f64 {
    if raw <= NEG_INF_RAW {
        return f64::NEG_INFINITY;
    }
    i32::try_from(raw).map_or(f64::NEG_INFINITY, |r| f64::from(r) / 100.0)
}

/// dB → dB×100. `-inf` and anything below [`MIN_FINITE_DB`] → -32768.
/// `None` for NaN or above [`MAX_DB`].
#[must_use]
pub fn db_to_raw(db: f64) -> Option<i64> {
    if db.is_nan() || db > MAX_DB {
        return None;
    }
    if db < MIN_FINITE_DB {
        return Some(NEG_INF_RAW);
    }
    round_i32(db * 100.0).map(i64::from)
}

/// The generic [`ParamKind`] for a value kind.
#[must_use]
pub fn param_kind(kind: ValueKind, vocab: &Vocab) -> ParamKind {
    match kind {
        ValueKind::Level => ParamKind::Level {
            min_db: MIN_FINITE_DB,
            max_db: MAX_DB,
        },
        ValueKind::Toggle => ParamKind::Toggle,
        ValueKind::Pan => ParamKind::Pan,
        ValueKind::PrePost => ParamKind::Enum {
            options: PRE_POST.iter().map(|s| (*s).to_owned()).collect(),
        },
        ValueKind::Int { min, max } => ParamKind::Int { min, max },
        ValueKind::Text { .. } | ValueKind::Opaque => ParamKind::Text,
        ValueKind::Label(v) => ParamKind::Enum {
            options: vocab.options(v).to_vec(),
        },
    }
}

/// Wire → generic. Unknown label strings are appended to `vocab`.
#[must_use]
pub fn decode(kind: ValueKind, raw: &RcpValue, vocab: &mut Vocab) -> Option<ParamValue> {
    Some(match (kind, raw) {
        (ValueKind::Level, RcpValue::Int(n)) => ParamValue::Level(raw_to_db(*n)),
        (ValueKind::Toggle, RcpValue::Int(n)) => ParamValue::Toggle(*n != 0),
        (ValueKind::Pan, RcpValue::Int(n)) => {
            let n = i32::try_from(*n).ok()?;
            ParamValue::Pan((f64::from(n) / f64::from(PAN_RANGE)).clamp(-1.0, 1.0))
        }
        (ValueKind::PrePost, RcpValue::Int(n)) => ParamValue::Enum(u32::try_from(*n).ok()?),
        (ValueKind::Int { .. }, RcpValue::Int(n)) => ParamValue::Int(*n),
        (ValueKind::Text { .. }, RcpValue::Str(s)) => ParamValue::Text(s.clone()),
        (ValueKind::Label(v), RcpValue::Str(s)) => ParamValue::Enum(vocab.intern(v, s)?),
        (ValueKind::Opaque, v) => ParamValue::Text(match v {
            RcpValue::Int(n) => n.to_string(),
            RcpValue::Str(s) => s.clone(),
        }),
        _ => return None,
    })
}

/// Generic → wire, validating range and type. `Err` carries the reason.
///
/// # Errors
/// A human-readable reason when the value doesn't fit.
pub fn encode(kind: ValueKind, value: &ParamValue, vocab: &Vocab) -> Result<RcpValue, String> {
    match (kind, value) {
        (ValueKind::Level, ParamValue::Level(db)) => db_to_raw(*db)
            .map(RcpValue::Int)
            .ok_or_else(|| format!("level must be -inf..={MAX_DB} dB")),
        (ValueKind::Toggle, ParamValue::Toggle(b)) => Ok(RcpValue::Int(i64::from(*b))),
        (ValueKind::Pan, ParamValue::Pan(p)) => {
            if !(-1.0..=1.0).contains(p) {
                return Err("pan must be -1.0..=1.0".to_owned());
            }
            round_i32(p * f64::from(PAN_RANGE))
                .map(|n| RcpValue::Int(i64::from(n)))
                .ok_or_else(|| "bad pan".to_owned())
        }
        (ValueKind::PrePost, ParamValue::Enum(i)) if *i <= 1 => Ok(RcpValue::Int(i64::from(*i))),
        (ValueKind::PrePost, ParamValue::Enum(_)) => Err("expected 0 = post, 1 = pre".to_owned()),
        (ValueKind::Int { min, max }, ParamValue::Int(n)) => {
            if (min..=max).contains(n) {
                Ok(RcpValue::Int(*n))
            } else {
                Err(format!("expected {min}..={max}"))
            }
        }
        (ValueKind::Text { max_len }, ParamValue::Text(s)) => {
            if !s.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
                return Err("names must be printable ASCII".to_owned());
            }
            if s.len() > max_len {
                return Err(format!("at most {max_len} characters"));
            }
            Ok(RcpValue::Str(s.clone()))
        }
        (ValueKind::Label(v), ParamValue::Enum(i)) => vocab
            .name(v, *i)
            .map(|s| RcpValue::Str(s.to_owned()))
            .ok_or_else(|| format!("option index {i} out of range")),
        (ValueKind::Opaque, _) => Err("read-only".to_owned()),
        _ => Err("value type doesn't fit the parameter".to_owned()),
    }
}
