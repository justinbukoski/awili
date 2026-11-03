//! gpui Element that renders a terminal cell grid.
//!
//! Reads from a shared `Term<ChannelListener>` (the same Arc the background
//! PTY thread writes to) and paints a monospace character grid with ANSI colors.

use std::sync::{Arc, Mutex};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Color as AnsiColor;

use gpui::{
    fill, point, prelude::*, size, App, Bounds, ElementId, GlobalElementId, Hsla,
    InspectorElementId, LayoutId, Pixels, Point, ShapedLine, SharedString, Style, TextRun, Window,
};

use crate::terminal::{lock_term, ChannelListener};
use crate::terminal_palette;
use crate::theme;

fn ansi_to_hsla(color: AnsiColor, is_fg: bool) -> Hsla {
    match color {
        AnsiColor::Named(named) => {
            let idx = named as usize;
            if idx < 16 {
                terminal_palette::rgb_to_hsla(terminal_palette::ANSI_COLORS[idx])
            } else if is_fg {
                theme::p().text.into()
            } else {
                Hsla::transparent_black()
            }
        }
        AnsiColor::Spec(rgb) => Hsla::from(gpui::Rgba {
            r: rgb.r as f32 / 255.0,
            g: rgb.g as f32 / 255.0,
            b: rgb.b as f32 / 255.0,
            a: 1.0,
        }),
        AnsiColor::Indexed(idx) => {
            if idx < 16 {
                terminal_palette::rgb_to_hsla(terminal_palette::ANSI_COLORS[idx as usize])
            } else if idx < 232 {
                // 216-color cube (6x6x6)
                let idx = idx - 16;
                let r = (idx / 36) as f32 / 5.0;
                let g = ((idx % 36) / 6) as f32 / 5.0;
                let b = (idx % 6) as f32 / 5.0;
                Hsla::from(gpui::Rgba { r, g, b, a: 1.0 })
            } else {
                // Grayscale ramp 232-255
                let v = (idx - 232) as f32 * 10.0 / 255.0 + 8.0 / 255.0;
                Hsla::from(gpui::Rgba {
                    r: v,
                    g: v,
                    b: v,
                    a: 1.0,
                })
            }
        }
    }
}

struct TermLine {
    shaped: ShapedLine,
    origin: Point<Pixels>,
    bg_runs: Vec<BgRun>,
}

struct BgRun {
    col_start: usize,
    col_end: usize,
    color: Hsla,
}

/// Prepaint state: shaped text + bg color runs.
pub struct TerminalPrepaint {
    lines: Vec<TermLine>,
    cell_width: Pixels,
    line_height: Pixels,
}

pub struct TerminalElement {
    term: Arc<Mutex<Term<ChannelListener>>>,
    /// RC2-RUN (round 3 Minor 17): when set, the grid takes the full width
    /// it is given and its columns follow that width.
    fit: Option<crate::terminal::TerminalFit>,
}

// Observe the actual layout/shaping path in window tests. The headless text
// backend may use uniform fake advances even for a proportional family, so
// effective family/weight assertions are load-bearing as well as geometry.
#[cfg(test)]
pub(crate) mod font_probe {
    use super::*;
    #[derive(Clone)]
    pub struct Sample {
        pub phase: &'static str,
        pub bounds: Option<Bounds<Pixels>>,
        pub font: gpui::Font,
        pub cell_width: Pixels,
        pub text: String,
        pub runs: Vec<(usize, gpui::Font)>,
        pub positions: Vec<Pixels>,
        pub backgrounds: Vec<(usize, usize)>,
    }
    std::thread_local! {
        static SAMPLES: std::cell::RefCell<Option<Vec<Sample>>> = const { std::cell::RefCell::new(None) };
    }
    pub fn begin() {
        SAMPLES.with(|s| *s.borrow_mut() = Some(Vec::new()));
    }
    pub fn record(sample: Sample) {
        SAMPLES.with(|s| {
            if let Some(samples) = s.borrow_mut().as_mut() {
                samples.push(sample);
            }
        });
    }
    pub fn take() -> Vec<Sample> {
        SAMPLES.with(|s| s.borrow_mut().take().unwrap_or_default())
    }
}

impl TerminalElement {
    pub fn new(term: Arc<Mutex<Term<ChannelListener>>>) -> Self {
        Self { term, fit: None }
    }

    /// RC2-RUN (round 3 Minor 17): the grid fills the width it is given
    /// and the terminal is resized to the columns that fit there, so a long
    /// line wraps instead of running off the card.
    pub fn fitted(mut self, fit: crate::terminal::TerminalFit) -> Self {
        self.fit = Some(fit);
        self
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let term = lock_term(&self.term);
        let grid = term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        drop(term);

        let text_style = window.text_style();
        let font = text_style.font();
        #[cfg(test)]
        let effective_font = font.clone();
        let font_size = text_style.font_size.to_pixels(window.rem_size());

        // Measure a reference character to get monospace cell width.
        let ref_text: SharedString = "M".into();
        let ref_run = TextRun {
            len: 1,
            font,
            color: Hsla::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let ref_shaped = window
            .text_system()
            .shape_line(ref_text, font_size, &[ref_run], None);
        let cell_width = ref_shaped.width;
        let line_height = window.line_height();
        #[cfg(test)]
        font_probe::record(font_probe::Sample {
            phase: "layout",
            bounds: None,
            font: effective_font,
            cell_width,
            text: String::new(),
            runs: Vec::new(),
            positions: Vec::new(),
            backgrounds: Vec::new(),
        });

        let mut style = Style::default();
        style.size.width = if self.fit.is_some() {
            gpui::relative(1.).into()
        } else {
            (cell_width * cols as f32).into()
        };
        style.size.height = (line_height * rows as f32).into();

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        // Measure cell width.
        let ref_text: SharedString = "M".into();
        let ref_run = TextRun {
            len: 1,
            font: font.clone(),
            color: Hsla::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let cell_width = window
            .text_system()
            .shape_line(ref_text, font_size, &[ref_run], None)
            .width;
        #[cfg(test)]
        font_probe::record(font_probe::Sample {
            phase: "grid",
            bounds: Some(bounds),
            font: font.clone(),
            cell_width,
            text: String::new(),
            runs: Vec::new(),
            positions: Vec::new(),
            backgrounds: Vec::new(),
        });

        // RC2-RUN (round 3 Minor 17): fit the columns to the width given.
        if let Some(fit) = &self.fit {
            let rows = lock_term(&self.term).grid().screen_lines();
            let cols =
                crate::terminal::fit_cols(f32::from(bounds.size.width), f32::from(cell_width));
            fit.fit(cols, u16::try_from(rows).unwrap_or(u16::MAX));
        }

        // RC2-APP (F0242): the screen is read under its lock, and the lock is
        // let go before any text is shaped — shaping is the slow part, and
        // the PTY reader waits on the same lock to write new output.
        let row_paints = {
            let term = lock_term(&self.term);
            build_rows(term.grid(), &font)
        };

        let mut lines = Vec::with_capacity(row_paints.len());
        for (line_idx, row) in row_paints.into_iter().enumerate() {
            let origin = point(
                bounds.origin.x,
                bounds.origin.y + line_height * line_idx as f32,
            );
            let shaped_text: SharedString = row.text.into();
            let shaped = window
                .text_system()
                .shape_line(shaped_text, font_size, &row.runs, None);

            #[cfg(test)]
            if shaped.text.starts_with("iM iM") || shaped.text.trim() == "X" {
                font_probe::record(font_probe::Sample {
                    phase: "row",
                    bounds: Some(bounds),
                    font: font.clone(),
                    cell_width,
                    text: shaped.text.to_string(),
                    runs: row.runs.iter().map(|r| (r.len, r.font.clone())).collect(),
                    positions: (0..=shaped.text.len())
                        .map(|i| shaped.x_for_index(i))
                        .collect(),
                    backgrounds: row
                        .bg_runs
                        .iter()
                        .map(|r| (r.col_start, r.col_end))
                        .collect(),
                });
            }

            lines.push(TermLine {
                shaped,
                origin,
                bg_runs: row.bg_runs,
            });
        }

        TerminalPrepaint {
            lines,
            cell_width,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let cell_width = prepaint.cell_width;
        let line_height = prepaint.line_height;

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            // Paint background color runs.
            for tl in &prepaint.lines {
                for bg in &tl.bg_runs {
                    let x = bounds.origin.x + cell_width * bg.col_start as f32;
                    let w = cell_width * (bg.col_end - bg.col_start) as f32;
                    window.paint_quad(fill(
                        Bounds::new(point(x, tl.origin.y), size(w, line_height)),
                        bg.color,
                    ));
                }
            }
            // Paint text.
            for tl in &prepaint.lines {
                tl.shaped
                    .paint(
                        tl.origin,
                        line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
            }
        });
    }
}

/// One screen row, read from the grid: its text, its glyph runs and its
/// background runs (shaped later, without the screen lock).
struct RowPaint {
    text: String,
    runs: Vec<TextRun>,
    bg_runs: Vec<BgRun>,
}

/// RC2-APP (F0251): a cell's glyph and background colours. Inverse video
/// swaps them; a DEFAULT background swapped onto the glyph is the
/// terminal's own background colour, never transparent (the glyph used to
/// disappear).
fn cell_colors(fg: AnsiColor, bg: AnsiColor, flags: CellFlags) -> (Hsla, Hsla) {
    let fg_color = ansi_to_hsla(fg, true);
    let bg_color = ansi_to_hsla(bg, false);
    let mut glyph = fg_color;
    if flags.contains(CellFlags::DIM) {
        glyph.l *= 0.7;
    }
    if flags.contains(CellFlags::INVERSE) {
        let swapped = if bg_color.a < 0.01 {
            Hsla::from(theme::p().bg)
        } else {
            bg_color
        };
        (swapped, fg_color)
    } else {
        (glyph, bg_color)
    }
}

/// Read every screen row of `grid` into its text, glyph runs and
/// background runs (RC2-APP F0242: done under the screen lock, shaped
/// after it).
fn build_rows(
    grid: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
    font: &gpui::Font,
) -> Vec<RowPaint> {
    let rows = grid.screen_lines();
    let cols = grid.columns();
    let mut out = Vec::with_capacity(rows);
    for line_idx in 0..rows {
        let row = &grid[alacritty_terminal::index::Line(line_idx as i32)];

        // Build text string + per-char fg runs + bg color runs.
        let mut text = String::with_capacity(cols);
        let mut runs: Vec<TextRun> = Vec::new();
        let mut bg_runs: Vec<BgRun> = Vec::new();
        let mut cur_bg: Option<(usize, Hsla)> = None;

        for col_idx in 0..cols {
            let cell = &row[alacritty_terminal::index::Column(col_idx)];
            let c = cell.c;
            text.push(if c == '\0' || c == ' ' { ' ' } else { c });

            let (color, effective_bg) = cell_colors(cell.fg, cell.bg, cell.flags);

            let bold = cell.flags.contains(CellFlags::BOLD);
            let underline = if cell.flags.contains(CellFlags::UNDERLINE) {
                Some(gpui::UnderlineStyle {
                    // Decoration thickness follows the interface zoom (A4).
                    thickness: theme::z(1.),
                    color: Some(color),
                    wavy: false,
                })
            } else {
                None
            };
            let strikethrough = if cell.flags.contains(CellFlags::STRIKEOUT) {
                Some(gpui::StrikethroughStyle {
                    thickness: theme::z(1.),
                    color: Some(color),
                })
            } else {
                None
            };

            let mut run_font = font.clone();
            if bold {
                run_font.weight = gpui::FontWeight::BOLD;
            }
            // Font changes are attributes too: adjacent normal/bold cells
            // with the same color must not silently share the first font.
            let can_extend = runs.last().is_some_and(|r| {
                r.font == run_font
                    && r.color == color
                    && r.underline == underline
                    && r.strikethrough == strikethrough
            });
            if can_extend {
                runs.last_mut().unwrap().len += c.len_utf8();
            } else {
                runs.push(TextRun {
                    len: c.len_utf8(),
                    font: run_font,
                    color,
                    background_color: None,
                    underline,
                    strikethrough,
                });
            }

            // Background color runs (skip transparent).
            match &mut cur_bg {
                Some((_, prev_color)) if *prev_color == effective_bg => {
                    // extend
                }
                Some((start, prev_color)) => {
                    if prev_color.a >= 0.01 {
                        bg_runs.push(BgRun {
                            col_start: *start,
                            col_end: col_idx,
                            color: *prev_color,
                        });
                    }
                    cur_bg = Some((col_idx, effective_bg));
                }
                None => {
                    cur_bg = Some((col_idx, effective_bg));
                }
            }
        }
        // Flush final bg run.
        if let Some((start, color)) = cur_bg {
            if color.a >= 0.01 {
                bg_runs.push(BgRun {
                    col_start: start,
                    col_end: cols,
                    color,
                });
            }
        }
        out.push(RowPaint {
            text,
            runs,
            bg_runs,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};

    /// The reference color, built through the exact `gpui::Rgba -> Hsla` path the
    /// palette helpers use — so equality is bit-exact and needs no epsilon.
    fn ref_color(r: f32, g: f32, b: f32) -> Hsla {
        Hsla::from(gpui::Rgba { r, g, b, a: 1.0 })
    }

    #[test]
    fn indexed_low_16_hits_the_ansi_palette() {
        // Indices 0..16 map straight to the 16-color palette, never the cube.
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(1), true),
            ref_color(0xcc as f32 / 255.0, 0.0, 0.0),
            "1 = palette red 0xcc0000"
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(2), true),
            ref_color(
                0x4e as f32 / 255.0,
                0x9a as f32 / 255.0,
                0x06 as f32 / 255.0
            ),
            "2 = palette green 0x4e9a06"
        );
    }

    #[test]
    fn indexed_216_cube_arithmetic() {
        // The 6x6x6 cube (indices 16..232): origin is black, the far corner is
        // white, 196 is pure red and 21 is pure blue — the classic xterm-256
        // truths that pin the `/36`, `%36/6`, `%6` channel decomposition and the
        // `/5.0` normalisation. A swapped divisor silently misrenders every
        // 256-color program (which is most of them).
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(16), true),
            ref_color(0.0, 0.0, 0.0),
            "cube origin = black"
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(231), true),
            ref_color(1.0, 1.0, 1.0),
            "cube corner = white"
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(196), true),
            ref_color(1.0, 0.0, 0.0),
            "196 = pure red"
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(21), true),
            ref_color(0.0, 0.0, 1.0),
            "21 = pure blue"
        );
    }

    #[test]
    fn indexed_grayscale_ramp_matches_xterm_spec() {
        // Grayscale ramp (232..256): the xterm spec defines level 232 = 0x08 and
        // level 255 = 0xee on all three channels. Guards the `*10/255 + 8/255`
        // step-and-offset against an off-by-one regression.
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(232), true),
            ref_color(8.0 / 255.0, 8.0 / 255.0, 8.0 / 255.0),
            "232 = 0x08 gray"
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Indexed(255), true),
            ref_color(238.0 / 255.0, 238.0 / 255.0, 238.0 / 255.0),
            "255 = 0xee gray"
        );
    }

    #[test]
    fn named_default_respects_foreground_vs_background() {
        // A named color past the 16-slot palette (`Foreground = 256`) is the
        // terminal's *default* ink/paper: as fg it takes the theme body color,
        // as bg it stays transparent so the canvas shows through. This fg/bg
        // split is the one branch a regression would silently invert.
        assert_eq!(
            ansi_to_hsla(AnsiColor::Named(NamedColor::Foreground), true),
            Hsla::from(theme::p().text),
            "default fg = theme body text"
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Named(NamedColor::Foreground), false),
            Hsla::transparent_black(),
            "default bg = transparent"
        );
        // A palette-range named color ignores is_fg and hits the palette.
        assert_eq!(
            ansi_to_hsla(AnsiColor::Named(NamedColor::Red), false),
            ref_color(0xcc as f32 / 255.0, 0.0, 0.0),
            "named red = palette red regardless of fg/bg"
        );
    }

    #[test]
    fn spec_is_passed_through_channel_for_channel() {
        // A 24-bit truecolor Spec is used verbatim (each channel / 255).
        assert_eq!(
            ansi_to_hsla(
                AnsiColor::Spec(Rgb {
                    r: 10,
                    g: 20,
                    b: 30
                }),
                true
            ),
            ref_color(10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0),
        );
    }

    /// RC2-APP (F0251): inverse video on the default colours shows the
    /// glyph in the terminal's background colour on the text colour — never
    /// an invisible (transparent) glyph.
    #[test]
    fn rc2app_inverse_default_colours_keep_the_glyph_visible() {
        let (glyph, back) = cell_colors(
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
            CellFlags::INVERSE,
        );
        assert!(glyph.a > 0.99, "the glyph is opaque: {glyph:?}");
        assert_eq!(glyph, Hsla::from(theme::p().bg));
        assert_eq!(back, Hsla::from(theme::p().text));
        // A real background colour is still swapped as it is.
        let red = AnsiColor::Indexed(1);
        let (glyph, _) = cell_colors(
            AnsiColor::Named(NamedColor::Foreground),
            red,
            CellFlags::INVERSE,
        );
        assert_eq!(glyph, ansi_to_hsla(red, false));
        // Without inverse nothing changes.
        let (glyph, back) = cell_colors(
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
            CellFlags::empty(),
        );
        assert_eq!(glyph, Hsla::from(theme::p().text));
        assert!(back.a < 0.01, "the default background stays transparent");
    }
}
