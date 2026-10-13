//! Linear gain values.

use crate::HostError;

/// Upper bound for any linear gain (≈ +12 dB). Anything louder is almost
/// certainly a unit mistake (dB passed as linear) and would clip hard.
pub const MAX_GAIN: f32 = 4.0;

/// Linear gain helpers. Gains are plain `f32` on the wire; this namespace
/// holds the validation every backend applies.
pub struct Gain;

impl Gain {
    /// Unity gain.
    pub const UNITY: f32 = 1.0;

    /// Accept `gain` if it is finite and within `0.0..=MAX_GAIN`.
    ///
    /// # Errors
    /// [`HostError::InvalidSpec`] naming `what`.
    pub fn validate(what: &str, gain: f32) -> Result<f32, HostError> {
        if gain.is_finite() && (0.0..=MAX_GAIN).contains(&gain) {
            Ok(gain)
        } else {
            Err(HostError::InvalidSpec(format!(
                "{what}: gain {gain} outside 0.0..={MAX_GAIN}"
            )))
        }
    }

    /// Decibels → linear. `-inf` dB (or anything below -144) is silence.
    #[must_use]
    pub fn from_db(db: f32) -> f32 {
        if db <= -144.0 {
            0.0
        } else {
            10.0_f32.powf(db / 20.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_range() {
        assert!(Gain::validate("x", 0.0).is_ok());
        assert!(Gain::validate("x", MAX_GAIN).is_ok());
        assert!(Gain::validate("x", -0.1).is_err());
        assert!(Gain::validate("x", f32::NAN).is_err());
        assert!(Gain::validate("x", 5.0).is_err());
    }

    #[test]
    fn db_conversion() {
        assert!((Gain::from_db(0.0) - 1.0).abs() < 1e-6);
        assert!((Gain::from_db(-6.0) - 0.501).abs() < 1e-3);
        assert!(Gain::from_db(f32::NEG_INFINITY) <= 0.0);
    }
}
