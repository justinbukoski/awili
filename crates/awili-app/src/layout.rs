//! RC1-H6: the three columns' widths at any window width and text size (RC1
//! round 3, Major 6: at the narrowest window the middle column was about
//! 100 px and the composer wrapped one letter per line). The middle column
//! keeps [`CENTER_MIN_W`] whenever it can: the right panel yields first,
//! down to its own floor, then the sidebar. All widths are unscaled points;
//! [`columns`] scales them by the interface zoom.

/// The sidebar at full width.
pub(crate) const SIDEBAR_W: f32 = 240.;
/// The narrowest the sidebar gets.
pub(crate) const SIDEBAR_MIN_W: f32 = 168.;
/// The narrowest a right panel gets (a narrower panel keeps its width).
pub(crate) const PANEL_MIN_W: f32 = 240.;
/// What the middle column keeps whenever the window allows it.
pub(crate) const CENTER_MIN_W: f32 = 360.;

/// The side columns' widths in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Columns {
    pub(crate) sidebar: f32,
    pub(crate) panel: f32,
}

/// The side columns for a `window_w`-pixel window at interface zoom `scale`,
/// with a right panel of `panel_w` points (0 = none). Full widths when they
/// leave the middle column [`CENTER_MIN_W`]; otherwise the panel gives way
/// first (to [`PANEL_MIN_W`]), then the sidebar (to [`SIDEBAR_MIN_W`]).
pub(crate) fn columns(window_w: f32, scale: f32, panel_w: f32) -> Columns {
    let mut sidebar = SIDEBAR_W * scale;
    let mut panel = panel_w * scale;
    let mut short = sidebar + panel + CENTER_MIN_W * scale - window_w;
    if short > 0. {
        let give = short.min(panel - panel_w.min(PANEL_MIN_W) * scale).max(0.);
        panel -= give;
        short -= give;
    }
    if short > 0. {
        let give = short.min(sidebar - SIDEBAR_MIN_W * scale).max(0.);
        sidebar -= give;
    }
    Columns { sidebar, panel }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
