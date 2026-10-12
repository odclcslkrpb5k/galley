//! Numeric conversions. Positions and lengths are whole logical pixels in `i64`; em arithmetic
//! and GTK's adjustments use `f64`; graphene takes `f32` and Pango `i32` units and `u16` colours.
//!
//! This is the only module where `as` is allowed (docs/plan.md §7): std has no conversion
//! between floats and integers (or from f64 to f32) without `as`.
#![expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "float/integer conversions (f64 to i64, i32 and f32; i64 and usize to f64; f32 to u16) have no std alternative"
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

/// Logical pixels rounded to a whole pixel. Saturates at the i64 range.
pub(crate) fn round_px(px: f64) -> i64 {
    px.round() as i64
}

/// Whole logical pixels as f64; exact for any on-screen length.
pub(crate) fn px_to_f64(px: i64) -> f64 {
    px as f64
}

/// A byte index into a leaf's text as Pango's u32. Leaf text is capped at 16 KiB, so this never
/// saturates in practice.
pub(crate) fn text_index(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// A colour's channels as Pango's 16-bit values.
pub(crate) fn rgba_u16(color: gtk::gdk::RGBA) -> (u16, u16, u16, u16) {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 65535.0).round() as u16;
    (
        channel(color.red()),
        channel(color.green()),
        channel(color.blue()),
        channel(color.alpha()),
    )
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

    #[test]
    fn pixels_round_to_the_nearest_whole() {
        assert_eq!(round_px(2.4), 2);
        assert_eq!(round_px(2.6), 3);
        assert_eq!(round_px(-2.6), -3);
        assert!((px_to_f64(-7) + 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn text_indices_saturate() {
        assert_eq!(text_index(5), 5);
        assert_eq!(text_index(usize::MAX), u32::MAX);
    }

    #[test]
    fn colours_scale_and_clamp_to_sixteen_bits() {
        let color = gtk::gdk::RGBA::new(1.0, 0.5, -1.0, 2.0);
        assert_eq!(rgba_u16(color), (65_535, 32_768, 0, 65_535));
    }
}
