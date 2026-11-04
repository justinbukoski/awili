//! Persisted window bounds (polish slice A): the window reopens where the
//! user left it instead of a small centered default every launch. Same
//! file-per-value pattern as `zoom` — pure parse/format here so corruption
//! can only ever fall back to the default, never brick the window.

use std::path::PathBuf;

/// Persisted geometry in logical pixels: origin x/y, size w/h.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavedBounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The smallest restorable size — anything below this is treated as corrupt
/// (a 5px window is unusable, not a preference).
pub const MIN_W: f32 = 480.0;
pub const MIN_H: f32 = 360.0;

/// A roomy Linux first-run window, capped to leave space for desktop panels.
#[cfg(target_os = "linux")]
pub fn linux_launch_size(display: Option<(f32, f32)>) -> (f32, f32) {
    match display {
        Some((w, h)) => (
            1440.0_f32.min((w - 80.0).max(1.0)),
            960.0_f32.min((h - 120.0).max(1.0)),
        ),
        None => (1440.0, 960.0),
    }
}

/// Parse "x y w h". Any missing/garbage/non-finite field, or an implausible
/// size, is `None` — the caller uses the default centered bounds.
pub fn parse(raw: &str) -> Option<SavedBounds> {
    let mut it = raw.split_whitespace().map(|t| t.parse::<f32>().ok());
    let (x, y, w, h) = (it.next()??, it.next()??, it.next()??, it.next()??);
    let all = [x, y, w, h];
    if all.iter().any(|v| !v.is_finite()) || w < MIN_W || h < MIN_H {
        return None;
    }
    Some(SavedBounds { x, y, w, h })
}

/// Serialize for persistence.
pub fn format(b: SavedBounds) -> String {
    format!("{:.0} {:.0} {:.0} {:.0}", b.x, b.y, b.w, b.h)
}

/// `<data_dir>/Awili/window_bounds`, alongside `ui_scale`.
pub fn default_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(awili_config::APP_DIR_NAME).join("window_bounds"))
}

/// Load the saved bounds, if any and plausible.
pub fn load() -> Option<SavedBounds> {
    let path = default_path()?;
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Whether at least `MIN_VISIBLE` px of the saved bounds intersects any
/// connected display (Codex night-review MUST-3): geometry saved on a
/// since-disconnected monitor must not reopen the app off-screen.
pub const MIN_VISIBLE: f32 = 100.0;

pub fn visible_on_any(displays: &[(f32, f32, f32, f32)], b: SavedBounds) -> bool {
    displays.iter().any(|&(dx, dy, dw, dh)| {
        let ix = (b.x + b.w).min(dx + dw) - b.x.max(dx);
        let iy = (b.y + b.h).min(dy + dh) - b.y.max(dy);
        ix >= MIN_VISIBLE && iy >= MIN_VISIBLE
    })
}

/// Best-effort save; failures are ignored (a bounds write must never block
/// quit). Deduped and written OFF the UI thread (Codex night-review
/// SHOULD-5): move/resize streams dozens of notifications per second, and
/// the GPUI path stays I/O-free.
pub fn save(b: SavedBounds) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<SavedBounds>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if *last == Some(b) {
            return;
        }
        *last = Some(b);
    }
    std::thread::spawn(move || {
        if let Some(path) = default_path() {
            if let Some(dir) = path.parent() {
                let _ = awili_config::ensure_private_app_dir(dir);
            }
            let _ = std::fs::write(path, format(b));
        }
    });
}

/// Windows: what the window frame takes outside the client bounds GPUI
/// reports (title bar, borders), in logical pixels: `(title, bottom, sides)`.
/// The saved and default bounds are client bounds, so a window that is as tall
/// as the work area still pokes its frame under the taskbar.
#[cfg_attr(not(windows), allow(dead_code))]
pub const FRAME_TITLE: f32 = 32.0;
#[cfg_attr(not(windows), allow(dead_code))]
pub const FRAME_BOTTOM: f32 = 16.0;
#[cfg_attr(not(windows), allow(dead_code))]
pub const FRAME_SIDES: f32 = 16.0;

/// Fit `b` (client bounds) inside `area` (x, y, w, h: the display's work area,
/// i.e. without the taskbar) with the frame allowance above: the size shrinks
/// to fit and the position moves until the whole framed window is inside. A
/// work area too small for the frame leaves the size at 1 px minimum rather
/// than panicking. Pure.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn fit_to_work_area(b: SavedBounds, area: (f32, f32, f32, f32)) -> SavedBounds {
    let (ax, ay, aw, ah) = area;
    let w = b.w.min((aw - FRAME_SIDES).max(1.0));
    let h = b.h.min((ah - FRAME_TITLE - FRAME_BOTTOM).max(1.0));
    let x_max = (ax + aw - w - FRAME_SIDES / 2.0).max(ax);
    let y_max = (ay + ah - h - FRAME_BOTTOM).max(ay + FRAME_TITLE);
    SavedBounds {
        x: b.x
            .clamp(ax + FRAME_SIDES / 2.0, x_max.max(ax + FRAME_SIDES / 2.0)),
        y: b.y.clamp(ay + FRAME_TITLE, y_max),
        w,
        h,
    }
}

/// Windows: the primary display's work area in logical pixels (taskbar
/// excluded), from `SPI_GETWORKAREA` and the system DPI.
#[cfg(windows)]
pub fn primary_work_area() -> Option<(f32, f32, f32, f32)> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: r is a valid RECT out buffer, as SPI_GETWORKAREA requires.
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut _ as *mut _, 0) };
    if ok == 0 || r.right <= r.left || r.bottom <= r.top {
        return None;
    }
    // SAFETY: no preconditions.
    let dpi = unsafe { GetDpiForSystem() };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    Some((
        r.left as f32 / scale,
        r.top as f32 / scale,
        (r.right - r.left) as f32 / scale,
        (r.bottom - r.top) as f32 / scale,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_launch_size_fits_laptop_and_large_displays() {
        assert_eq!(linux_launch_size(None), (1440., 960.));
        assert_eq!(linux_launch_size(Some((3072., 1728.))), (1440., 960.));
        assert_eq!(linux_launch_size(Some((1366., 768.))), (1286., 648.));
    }

    #[test]
    fn roundtrip_and_garbage() {
        let b = SavedBounds {
            x: 120.0,
            y: 80.0,
            w: 1200.0,
            h: 800.0,
        };
        assert_eq!(parse(&format(b)), Some(b));
        assert_eq!(parse(""), None);
        assert_eq!(parse("1 2 3"), None);
        assert_eq!(parse("nan 2 900 700"), None);
        assert_eq!(parse("0 0 10 10"), None); // implausibly small
        assert_eq!(parse("0 0 481 361").map(|b| b.w), Some(481.0));
    }

    #[test]
    fn offscreen_bounds_are_rejected() {
        let main = [(0.0, 0.0, 1728.0, 1117.0)];
        let on = SavedBounds {
            x: 100.0,
            y: 100.0,
            w: 900.0,
            h: 700.0,
        };
        assert!(visible_on_any(&main, on));
        // Saved on a disconnected external monitor to the right.
        let off = SavedBounds {
            x: 2000.0,
            y: 0.0,
            w: 900.0,
            h: 700.0,
        };
        assert!(!visible_on_any(&main, off));
        // A sliver (<100px) is as good as off-screen.
        let sliver = SavedBounds {
            x: 1700.0,
            y: 0.0,
            w: 900.0,
            h: 700.0,
        };
        assert!(!visible_on_any(&main, sliver));
        // Negative-origin (display arranged left) still counts.
        let left = [(-1920.0, 0.0, 1920.0, 1080.0)];
        let neg = SavedBounds {
            x: -1800.0,
            y: 50.0,
            w: 900.0,
            h: 700.0,
        };
        assert!(visible_on_any(&left, neg));
    }

    // 1280x800 display with a 48 px taskbar: the work area is 1280x752.
    const AREA: (f32, f32, f32, f32) = (0.0, 0.0, 1280.0, 752.0);

    #[test]
    fn the_default_window_fits_a_small_work_area() {
        // 900x680 centred on the full 800 px display poked under the taskbar.
        let b = SavedBounds {
            x: 190.0,
            y: 60.0,
            w: 900.0,
            h: 680.0,
        };
        let f = fit_to_work_area(b, AREA);
        assert!(f.y + f.h + FRAME_BOTTOM <= AREA.3, "{f:?}");
        assert!(f.y - FRAME_TITLE >= AREA.1, "{f:?}");
        assert!(f.w <= 900.0 && f.h <= 680.0);
    }

    #[test]
    fn a_window_that_already_fits_is_left_alone() {
        let b = SavedBounds {
            x: 100.0,
            y: 100.0,
            w: 900.0,
            h: 500.0,
        };
        assert_eq!(fit_to_work_area(b, AREA), b);
    }

    #[test]
    fn an_oversized_restore_shrinks_and_moves_inside() {
        let b = SavedBounds {
            x: 900.0,
            y: 400.0,
            w: 2000.0,
            h: 1500.0,
        };
        let f = fit_to_work_area(b, AREA);
        assert!(f.w <= AREA.2 - FRAME_SIDES && f.h <= AREA.3 - FRAME_TITLE - FRAME_BOTTOM);
        assert!(f.x >= AREA.0 && f.x + f.w <= AREA.0 + AREA.2);
        assert!(f.y - FRAME_TITLE >= AREA.1 && f.y + f.h + FRAME_BOTTOM <= AREA.1 + AREA.3);
    }

    #[test]
    fn a_secondary_origin_work_area_is_respected() {
        let area = (-1920.0, 0.0, 1920.0, 1032.0);
        let b = SavedBounds {
            x: -2500.0,
            y: -50.0,
            w: 900.0,
            h: 700.0,
        };
        let f = fit_to_work_area(b, area);
        assert!(f.x >= area.0 && f.y - FRAME_TITLE >= area.1);
    }

    #[test]
    fn a_work_area_smaller_than_the_frame_does_not_panic() {
        let b = SavedBounds {
            x: 0.0,
            y: 0.0,
            w: 900.0,
            h: 700.0,
        };
        let f = fit_to_work_area(b, (0.0, 0.0, 10.0, 10.0));
        assert!(f.w >= 1.0 && f.h >= 1.0);
    }
}
