//! Interface-zoom scale factor: the persisted, clamped model behind View ▸ Zoom
//! In / Zoom Out / Actual Size (A4).
//!
//! The *application* of the scale lives in [`crate::theme`] (`z()` builds every
//! view dimension through the live factor). This module owns the pure policy —
//! the clamp range, the step, and load/save — so it can be unit-tested without a
//! window, and so a corrupt or missing persisted value can only ever fall back
//! to actual size, never to an unreadable UI.

use std::path::PathBuf;

/// Actual size — the identity scale.
pub const ACTUAL: f32 = 1.0;

/// The smallest / largest interface zoom. The range is an accessibility knob for
/// the app's only user (Foreman memory 2079): comfortably readable at the top
/// end without the fixed-size window becoming unusable.
pub const MIN_SCALE: f32 = 0.7;
pub const MAX_SCALE: f32 = 2.5;

/// One Zoom In / Zoom Out notch.
pub const STEP: f32 = 0.1;

/// Clamp an arbitrary scale into the supported range, rounded to a clean notch
/// so repeated stepping cannot drift (0.1 + 0.1 + … accumulating float error).
pub fn clamp_scale(scale: f32) -> f32 {
    let rounded = (scale * 100.0).round() / 100.0;
    rounded.clamp(MIN_SCALE, MAX_SCALE)
}

/// The next scale one notch larger, clamped at [`MAX_SCALE`].
pub fn step_in(scale: f32) -> f32 {
    clamp_scale(scale + STEP)
}

/// The next scale one notch smaller, clamped at [`MIN_SCALE`].
pub fn step_out(scale: f32) -> f32 {
    clamp_scale(scale - STEP)
}

/// Parse a persisted scale string, clamped into range. Any garbage — a bad
/// number, a blank file, an out-of-range value — resolves to a usable scale
/// (clamped, or [`ACTUAL`] when unparseable), so the UI can never be bricked by
/// a corrupt file.
pub fn parse_scale(raw: &str) -> f32 {
    match raw.trim().parse::<f32>() {
        Ok(v) if v.is_finite() => clamp_scale(v),
        _ => ACTUAL,
    }
}

/// Serialize a scale for persistence — two decimals, matching the notch grid.
pub fn format_scale(scale: f32) -> String {
    format!("{:.2}", clamp_scale(scale))
}

/// The persisted-scale path: `<data_dir>/Awili/ui_scale`, alongside the
/// database (see `awili_persist::Store::default_path`). `None` when the platform
/// has no data dir.
pub fn default_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(awili_config::APP_DIR_NAME).join("ui_scale"))
}

/// Load the persisted scale from `path`, clamped. A missing or unreadable file
/// is actual size — a fresh install opens at 1.0.
pub fn load_from(path: &PathBuf) -> f32 {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse_scale(&raw),
        Err(_) => ACTUAL,
    }
}

/// Save `scale` to `path`, creating the parent directory if needed. Best-effort:
/// a write failure is returned so the caller can log it, never panics.
pub fn save_to(path: &PathBuf, scale: f32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        awili_config::ensure_private_app_dir(parent)?;
    }
    std::fs::write(path, format_scale(scale))
}

/// Load from the default path (actual size when there is none).
pub fn load() -> f32 {
    match default_path() {
        Some(p) => load_from(&p),
        None => ACTUAL,
    }
}

/// Save to the default path; a missing data dir or write error is swallowed
/// (zoom persistence is a convenience, not load-bearing for the session).
pub fn save(scale: f32) {
    if let Some(p) = default_path() {
        let _ = save_to(&p, scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_holds_the_range() {
        assert_eq!(clamp_scale(0.1), MIN_SCALE, "below min clamps up");
        assert_eq!(clamp_scale(9.0), MAX_SCALE, "above max clamps down");
        assert_eq!(clamp_scale(ACTUAL), ACTUAL, "actual size is in range");
    }

    #[test]
    fn stepping_is_monotonic_and_clamps_at_the_ends() {
        // Zoom in from actual size grows by one notch.
        assert!((step_in(ACTUAL) - 1.1).abs() < 1e-6);
        // Zoom out shrinks by one notch.
        assert!((step_out(ACTUAL) - 0.9).abs() < 1e-6);
        // Repeated zoom-in saturates at MAX and never overshoots.
        let mut s = ACTUAL;
        for _ in 0..50 {
            s = step_in(s);
        }
        assert_eq!(s, MAX_SCALE);
        // Repeated zoom-out saturates at MIN.
        let mut s = ACTUAL;
        for _ in 0..50 {
            s = step_out(s);
        }
        assert_eq!(s, MIN_SCALE);
    }

    #[test]
    fn stepping_does_not_drift() {
        // Ten in then ten out returns exactly to actual size (no float creep).
        let mut s = ACTUAL;
        for _ in 0..10 {
            s = step_in(s);
        }
        for _ in 0..10 {
            s = step_out(s);
        }
        assert_eq!(s, ACTUAL);
    }

    #[test]
    fn parse_clamps_and_defaults_safely() {
        assert_eq!(parse_scale("1.25"), 1.25);
        assert_eq!(parse_scale("  1.50 \n"), 1.5, "whitespace tolerated");
        assert_eq!(parse_scale("99"), MAX_SCALE, "out-of-range clamps");
        assert_eq!(parse_scale("0.01"), MIN_SCALE);
        assert_eq!(
            parse_scale("not-a-number"),
            ACTUAL,
            "garbage is actual size"
        );
        assert_eq!(parse_scale(""), ACTUAL, "empty is actual size");
        assert_eq!(parse_scale("NaN"), ACTUAL, "non-finite is actual size");
    }

    #[test]
    fn format_round_trips_through_parse() {
        for &s in &[MIN_SCALE, 0.9, ACTUAL, 1.3, 1.75, MAX_SCALE] {
            assert_eq!(parse_scale(&format_scale(s)), clamp_scale(s));
        }
    }

    #[test]
    fn save_then_load_round_trips_a_clamped_scale() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "awili-zoom-test-{}-{}.scale",
            std::process::id(),
            // A per-call suffix so parallel test runs don't collide.
            MAX_SCALE.to_bits()
        ));
        save_to(&path, 1.4).expect("write");
        assert_eq!(load_from(&path), 1.4);
        // An out-of-range value is clamped on the way in and back out.
        save_to(&path, 5.0).expect("write");
        assert_eq!(load_from(&path), MAX_SCALE);
        let _ = std::fs::remove_file(&path);
        // A missing file is actual size.
        assert_eq!(load_from(&path), ACTUAL);
    }
}
