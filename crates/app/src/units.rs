//! Numeric conversions between our logical pixels (f64) and what GTK and Pango take.
//!
//! This is the only module where `as` is allowed (docs/plan.md §7): std has no conversion from
//! f64 to f32 or i32 without `as`.
#![expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "GTK takes f32 and Pango takes i32; std offers no other conversion from f64"
)]

use gtk::pango;

/// Logical pixels as graphene's f32.
pub(crate) fn to_f32(px: f64) -> f32 {
    px as f32
}

/// Logical pixels as Pango units (px × `pango::SCALE`), rounded. Saturates at the i32 range.
pub(crate) fn to_pango_units(px: f64) -> i32 {
    (px * f64::from(pango::SCALE)).round() as i32
}

/// Pango units as logical pixels.
pub(crate) fn from_pango_units(units: i32) -> f64 {
    f64::from(units) / f64::from(pango::SCALE)
}

/// A frame-clock interval in microseconds as seconds.
pub(crate) fn micros_to_seconds(micros: i64) -> f64 {
    micros as f64 / 1e6
}

/// A count as f64; exact up to 2^53, far beyond any document.
pub(crate) fn count_to_f64(count: usize) -> f64 {
    count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pango_units_round_trip() {
        assert_eq!(to_pango_units(12.5), 12_800);
        assert!((from_pango_units(12_800) - 12.5).abs() < f64::EPSILON);
    }

    #[test]
    fn graphene_f32_keeps_the_value() {
        assert!((to_f32(12.5) - 12.5_f32).abs() < f32::EPSILON);
        assert!((to_f32(-3.25) + 3.25_f32).abs() < f32::EPSILON);
    }

    #[test]
    fn micros_become_seconds() {
        assert!((micros_to_seconds(1_500_000) - 1.5).abs() < f64::EPSILON);
        assert!((micros_to_seconds(16_667) - 0.016_667).abs() < 1e-12);
        assert!(micros_to_seconds(0).abs() < f64::EPSILON);
    }

    #[test]
    fn counts_become_f64() {
        assert!(count_to_f64(0).abs() < f64::EPSILON);
        assert!((count_to_f64(3) - 3.0).abs() < f64::EPSILON);
        assert!((count_to_f64(1 << 40) - 1_099_511_627_776.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pango_units_saturate() {
        assert_eq!(to_pango_units(1e12), i32::MAX);
        assert_eq!(to_pango_units(f64::NAN), 0);
    }
}
