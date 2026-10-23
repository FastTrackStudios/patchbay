//! Level scales: dBFS meters and dB faders.
//!
//! Pure maths, no dioxus — the widgets in [`super`] render what these
//! return, and the views feed them raw peaks from the engine.

/// Lowest fader position with a finite value.
pub const FADER_MIN_DB: f64 = -60.0;
pub const FADER_MAX_DB: f64 = 12.0;
/// The slot below [`FADER_MIN_DB`] — "-∞" (off).
pub const FADER_OFF_SLOT: f64 = -61.0;
/// What "-∞" sends: finite (it rides the wire), far below audibility.
pub const OFF_DB: f64 = -144.0;
/// Meter scale bottom (dBFS); the top is 0.
pub const METER_FLOOR_DB: f64 = -60.0;
/// Where the meter turns yellow / red (dBFS).
pub const METER_WARN_DB: f64 = -20.0;
pub const METER_HOT_DB: f64 = -6.0;
/// Meter fall per poll (≈ 30 dB/s at 10 Hz) — peaks jump, decay smooth.
pub const METER_FALL_DB: f64 = 3.0;
/// Below this the source is silent, not merely quiet.
pub const SILENT_DB: f64 = METER_FLOOR_DB - 1.0;

/// Loudest channel of a peak set, in dBFS (floored below the scale).
#[must_use]
pub fn peak_db(peaks: &[f32]) -> f64 {
    let p = f64::from(peaks.iter().copied().fold(0.0_f32, f32::max));
    if p > 0.0 {
        (20.0 * p.log10()).max(SILENT_DB)
    } else {
        SILENT_DB
    }
}

/// Ballistics: jump up to a new peak, fall at most [`METER_FALL_DB`].
#[must_use]
pub fn fall(prev: Option<f64>, new: f64) -> f64 {
    prev.map_or(new, |p| new.max(p - METER_FALL_DB))
}

/// Height of a dBFS value on the meter scale, in percent.
#[must_use]
pub fn meter_pct(db: f64) -> f64 {
    ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0) * 100.0
}

/// Fader position for a gain.
#[must_use]
pub fn fader_slot(gain_db: f64) -> f64 {
    if gain_db < FADER_MIN_DB {
        FADER_OFF_SLOT
    } else {
        gain_db.min(FADER_MAX_DB)
    }
}

/// Gain for a fader position (the bottom slot is off).
#[must_use]
pub fn slot_db(slot: f64) -> f64 {
    if slot < FADER_MIN_DB - 0.25 {
        OFF_DB
    } else {
        slot.clamp(FADER_MIN_DB, FADER_MAX_DB)
    }
}

/// A gain for display: `-∞ dB`, `0.0 dB`, `+3.5 dB`.
#[must_use]
pub fn fmt_db(gain_db: f64) -> String {
    if gain_db < FADER_MIN_DB {
        "-∞ dB".to_owned()
    } else if gain_db.abs() < 0.05 {
        "0.0 dB".to_owned()
    } else {
        format!("{gain_db:+.1} dB")
    }
}

/// A meter reading for display.
#[must_use]
pub fn fmt_peak(db: f64) -> String {
    if db < METER_FLOOR_DB {
        "silent".to_owned()
    } else {
        format!("peak {db:.1} dBFS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_round_trips_and_bottom_is_off() {
        assert!((fader_slot(-6.0) - -6.0).abs() < 1e-9);
        assert!((fader_slot(40.0) - FADER_MAX_DB).abs() < 1e-9);
        assert!((fader_slot(OFF_DB) - FADER_OFF_SLOT).abs() < 1e-9);
        assert!((slot_db(FADER_OFF_SLOT) - OFF_DB).abs() < 1e-9);
        assert!((slot_db(-60.0) - -60.0).abs() < 1e-9);
        assert_eq!(fmt_db(OFF_DB), "-∞ dB");
        assert_eq!(fmt_db(0.0), "0.0 dB");
        assert_eq!(fmt_db(-6.0), "-6.0 dB");
    }

    #[test]
    fn meters_scale_and_fall() {
        assert!((peak_db(&[0.0, 1.0]) - 0.0).abs() < 1e-6);
        assert!((peak_db(&[0.5]) - -6.02).abs() < 0.01);
        assert!(peak_db(&[]) < METER_FLOOR_DB);
        assert!((meter_pct(0.0) - 100.0).abs() < 1e-9);
        assert!((meter_pct(-120.0)).abs() < 1e-9);
        assert!((fall(Some(-10.0), -60.0) - -13.0).abs() < 1e-9);
        assert!((fall(Some(-10.0), -3.0) - -3.0).abs() < 1e-9);
    }

    #[test]
    fn readings_read_plainly() {
        assert_eq!(fmt_peak(SILENT_DB), "silent");
        assert_eq!(fmt_peak(-12.34), "peak -12.3 dBFS");
    }
}
