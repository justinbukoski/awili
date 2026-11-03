//! Pure geometry kernel for terminal text selection.
//!
//! Maps a pixel point — local to the grid's top-left origin — onto the
//! alacritty grid cell it lands in, plus which side of that cell the point
//! falls on. This is the GPUI-specific half of terminal copy: alacritty owns
//! text *extraction* (`Term::bounds_to_string` / `Term::selection_to_string`)
//! and the `Selection` state machine (`Selection::new`/`update`/`to_range`),
//! but it has no notion of pixels — the host has to translate a mouse position
//! into a `(Point, Side)` before it can drive a selection.
//!
//! Kept pure and separately testable on purpose: the cell-mapping arithmetic
//! (clamping an off-the-edge drag to the nearest valid cell, deriving the
//! cell side from the sub-cell fraction) is the part most likely to be subtly
//! wrong, and it is verifiable without a live window. The mouse-drag wiring,
//! the selection highlight render, and the Cmd+C copy path are built on top of
//! this in a later slice — those need eyes on the real terminal to confirm the
//! highlight looks right, this does not.

use alacritty_terminal::index::{Column, Line, Point, Side};

/// Translate a pixel offset (relative to the grid's top-left corner) into the
/// grid cell under it and which horizontal half of the cell the point sits on.
///
/// The `Side` matters because alacritty's `Selection` anchors on a cell *edge*:
/// a point past a cell's horizontal midpoint should anchor on that cell's
/// trailing (right) edge, so dragging right through a cell includes it — the
/// same behaviour iTerm2 / alacritty's own frontend use.
///
/// The result is clamped into `[0, cols-1] x [0, rows-1]`. A drag that leaves
/// the element (negative coordinates, or past the right/bottom edge) still
/// resolves to the nearest valid edge cell rather than vanishing, because a
/// real terminal keeps extending the selection to the grid boundary instead of
/// dropping it when the cursor slips outside.
///
/// `cell_width` and `line_height` are the painted cell metrics and must be
/// positive. A zero-sized grid (`cols == 0 || rows == 0`) yields the origin
/// cell on the left side rather than dividing by an empty dimension.
#[allow(dead_code)] // consumed by the mouse-drag / highlight slice (needs eyes)
pub fn cell_at_pixel(
    x: f32,
    y: f32,
    cell_width: f32,
    line_height: f32,
    cols: usize,
    rows: usize,
) -> (Point, Side) {
    debug_assert!(cell_width > 0.0, "cell_width must be positive");
    debug_assert!(line_height > 0.0, "line_height must be positive");

    if cols == 0 || rows == 0 {
        return (Point::new(Line(0), Column(0)), Side::Left);
    }

    // Column. `raw_col` is the fractional column position; floor gives the cell
    // index, the fractional remainder gives the side. Both are derived *before*
    // clamping so an off-the-right drag reports the last cell's right edge (a
    // full selection through it) and an off-the-left drag its left edge.
    let raw_col = (x / cell_width).max(0.0);
    let col_floor = raw_col.floor();
    let col = (col_floor as usize).min(cols - 1);
    let side = if x <= 0.0 {
        // Left of the grid entirely: anchor on the first cell's leading edge.
        Side::Left
    } else if (col_floor as usize) >= cols {
        // Right of the grid entirely: trailing edge of the last cell.
        Side::Right
    } else if raw_col - col_floor >= 0.5 {
        Side::Right
    } else {
        Side::Left
    };

    // Line. Clamp the same way; the side has no vertical analogue.
    let raw_line = (y / line_height).max(0.0);
    let line = (raw_line.floor() as usize).min(rows - 1) as i32;

    (Point::new(Line(line), Column(col)), side)
}

/// What a `c` keypress should do in the embedded terminal, given the active
/// modifiers and whether the grid currently holds a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKeyAction {
    /// Send ETX (0x03) — the Unix process interrupt. This is `Ctrl+C`, the
    /// SIGINT convention, and it fires regardless of any selection.
    Interrupt,
    /// Copy the current selection to the clipboard, then clear it. This is
    /// `Cmd+C` (macOS copy) when a selection exists.
    CopySelection,
    /// Swallow the key and do nothing. `Cmd+C` with no selection must NOT fall
    /// through to ETX — that would interrupt the running program on a *copy*
    /// keystroke, the pre-selection bug. macOS `Cmd+C` is a no-op when there is
    /// nothing to copy; it never interrupts.
    Ignore,
}

/// Decide how the terminal should treat a `c` keypress.
///
/// macOS draws a line the pre-selection code conflated: `Ctrl+C` is the process
/// interrupt (ETX) and `Cmd+C` is clipboard copy. The old `platform || control`
/// branch sent ETX for both, so `Cmd+C` interrupted the running program instead
/// of copying. This is the single decision point that fixes that:
///
/// - `Cmd+C` (`platform`) → [`CopySelection`] when a selection exists, else
///   [`Ignore`] (never ETX).
/// - `Ctrl+C` (`control`, no `platform`) → [`Interrupt`] (ETX), selection or not.
/// - `Cmd` wins if somehow both modifiers are held (copy beats interrupt).
/// - With `shift` held, or with neither modifier, returns `None` so the caller
///   falls through to normal key handling — matching the existing `!shift` gate
///   (`Ctrl+Shift+C` still reaches `keystroke_to_bytes`, which emits ETX there).
///
/// [`CopySelection`]: CopyKeyAction::CopySelection
/// [`Ignore`]: CopyKeyAction::Ignore
/// [`Interrupt`]: CopyKeyAction::Interrupt
pub fn classify_copy_key(
    platform: bool,
    control: bool,
    shift: bool,
    has_selection: bool,
) -> Option<CopyKeyAction> {
    if shift {
        return None;
    }
    if platform {
        return Some(if has_selection {
            CopyKeyAction::CopySelection
        } else {
            CopyKeyAction::Ignore
        });
    }
    if control {
        return Some(CopyKeyAction::Interrupt);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 10x4 grid of 8.0-wide, 16.0-tall cells used by most cases.
    const CW: f32 = 8.0;
    const LH: f32 = 16.0;
    const COLS: usize = 10;
    const ROWS: usize = 4;

    fn at(x: f32, y: f32) -> (Point, Side) {
        cell_at_pixel(x, y, CW, LH, COLS, ROWS)
    }

    #[test]
    fn left_half_of_first_cell_is_col0_left() {
        let (p, side) = at(2.0, 4.0);
        assert_eq!(p, Point::new(Line(0), Column(0)));
        assert_eq!(side, Side::Left);
    }

    #[test]
    fn right_half_of_first_cell_is_col0_right() {
        let (p, side) = at(6.0, 4.0);
        assert_eq!(p, Point::new(Line(0), Column(0)));
        assert_eq!(side, Side::Right);
    }

    #[test]
    fn exact_midpoint_rounds_to_right() {
        // frac == 0.5 belongs to the trailing edge (>= 0.5 -> Right).
        let (_, side) = at(4.0, 0.0);
        assert_eq!(side, Side::Right);
    }

    #[test]
    fn cell_boundary_is_leading_edge_of_next_cell() {
        // x == one full cell width -> column 1, fraction 0 -> Left.
        let (p, side) = at(8.0, 0.0);
        assert_eq!(p.column, Column(1));
        assert_eq!(side, Side::Left);
    }

    #[test]
    fn third_column_third_row() {
        // Zero-based: x in the 3rd cell (index 2), y in the 3rd row (index 2).
        let (p, _) = at(CW * 2.25, LH * 2.5);
        assert_eq!(p, Point::new(Line(2), Column(2)));
    }

    #[test]
    fn past_right_edge_clamps_to_last_col_right_side() {
        let (p, side) = at(CW * (COLS as f32 + 5.0), 0.0);
        assert_eq!(p.column, Column(COLS - 1));
        assert_eq!(side, Side::Right);
    }

    #[test]
    fn exactly_at_right_edge_is_last_col_right_side() {
        // x == cols * cell_width: floor == cols, off the grid -> right edge.
        let (p, side) = at(CW * COLS as f32, 0.0);
        assert_eq!(p.column, Column(COLS - 1));
        assert_eq!(side, Side::Right);
    }

    #[test]
    fn past_bottom_edge_clamps_to_last_row() {
        let (p, _) = at(0.0, LH * (ROWS as f32 + 3.0));
        assert_eq!(p.line, Line((ROWS - 1) as i32));
    }

    #[test]
    fn negative_coords_clamp_to_origin_left() {
        let (p, side) = at(-50.0, -50.0);
        assert_eq!(p, Point::new(Line(0), Column(0)));
        assert_eq!(side, Side::Left);
    }

    #[test]
    fn negative_x_only_clamps_column_keeps_row() {
        let (p, side) = at(-1.0, LH * 1.5);
        assert_eq!(p, Point::new(Line(1), Column(0)));
        assert_eq!(side, Side::Left);
    }

    #[test]
    fn empty_grid_yields_origin_without_panicking() {
        let (p, side) = cell_at_pixel(5.0, 5.0, CW, LH, 0, 0);
        assert_eq!(p, Point::new(Line(0), Column(0)));
        assert_eq!(side, Side::Left);
        // Zero rows but non-zero cols still short-circuits.
        let (p2, _) = cell_at_pixel(5.0, 5.0, CW, LH, COLS, 0);
        assert_eq!(p2, Point::new(Line(0), Column(0)));
    }

    #[test]
    fn last_cell_interior_is_not_clamped_off() {
        // A point inside the final column (not past it) keeps its real side.
        let (p, side) = at(CW * (COLS as f32 - 1.0) + 2.0, 0.0);
        assert_eq!(p.column, Column(COLS - 1));
        assert_eq!(side, Side::Left); // left third of the last cell
    }

    // ---- classify_copy_key ----

    #[test]
    fn ctrl_c_interrupts_with_no_selection() {
        // Ctrl+C, nothing selected → ETX (classic SIGINT).
        assert_eq!(
            classify_copy_key(false, true, false, false),
            Some(CopyKeyAction::Interrupt)
        );
    }

    #[test]
    fn ctrl_c_interrupts_even_with_selection() {
        // Ctrl+C is the interrupt regardless of selection; copy is Cmd+C's job.
        assert_eq!(
            classify_copy_key(false, true, false, true),
            Some(CopyKeyAction::Interrupt)
        );
    }

    #[test]
    fn cmd_c_with_selection_copies() {
        assert_eq!(
            classify_copy_key(true, false, false, true),
            Some(CopyKeyAction::CopySelection)
        );
    }

    #[test]
    fn cmd_c_without_selection_is_ignored_not_interrupt() {
        // The bug fix: Cmd+C with nothing selected must NOT send ETX.
        assert_eq!(
            classify_copy_key(true, false, false, false),
            Some(CopyKeyAction::Ignore)
        );
    }

    #[test]
    fn cmd_beats_ctrl_when_both_held() {
        // Platform (copy) wins over control (interrupt).
        assert_eq!(
            classify_copy_key(true, true, false, true),
            Some(CopyKeyAction::CopySelection)
        );
        assert_eq!(
            classify_copy_key(true, true, false, false),
            Some(CopyKeyAction::Ignore)
        );
    }

    #[test]
    fn shift_falls_through() {
        // Shift held → None: caller handles it as a normal keystroke, preserving
        // the existing `!shift` gate (Ctrl+Shift+C still emits ETX downstream).
        assert_eq!(classify_copy_key(false, true, true, false), None);
        assert_eq!(classify_copy_key(true, false, true, true), None);
    }

    #[test]
    fn no_modifier_falls_through() {
        // A bare `c` is not a copy/interrupt chord.
        assert_eq!(classify_copy_key(false, false, false, true), None);
    }
}
