//! `ui::Palette` — the kit's command palette (KIT.md v3, batch K3).
//!
//! A centred floating panel, 560 wide: a search field (the `TextField`
//! styling — `PANEL_SOLID`, `RADIUS_M`, a `CONTROL_LINE` edge, a
//! leading `Search` icon and `type_::BODY` text) over grouped results
//! (`.group(title, rows)`). Keyboard: Up and Down move the selection
//! (skipping disabled rows, wrapping; the parent keeps it scrolled into
//! view through the body's scroll handle), Enter runs the selected
//! row's action and closes, Backspace on an **empty** query goes back
//! a level (`.on_back(…)`), Escape closes.
//!
//! The match of the query in each row's label is highlighted in
//! `CONTRAST_TEXT` (case-insensitive).
//!
//! The search field edits the query the parent owns (K3b): typed
//! characters append and Backspace deletes, through `.on_query(…)`, with
//! a `CONTRAST` caret. The palette centres itself over a click-blocking
//! scrim, its result list is constrained to the window and scrolls, and
//! every row is a direct child of the list, so the selected row is
//! scrolled into view. When K4's `SearchField` lands, the palette embeds
//! it (IME and selection come with it).
//!
//! The palette is one tab stop (the kit's `Tabs` contract): the parent
//! focuses its focus handle when it opens.

use gpui::{
    div, prelude::*, App, ClickEvent, ElementId, IntoElement, KeyDownEvent, Pixels, RenderOnce,
    ScrollHandle, SharedString, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::focus_ring::focus_ring_style_on;
use crate::ui::interaction::{child_id, ClickHandler, PALETTE, PALETTE_FIELD};

/// The palette width, in logical px before zoom (KIT.md § Palette:
/// "560 wide"). Part-local constant — the spec's number, on the
/// part's own face.
const WIDTH: f32 = theme::PALETTE_W;

/// The parent's move/close callbacks (KIT.md § Palette: the parent owns
/// the query, the selection and the close).
type MoveHandler = Box<dyn Fn(usize, &mut Window, &mut App) + 'static>;
type CloseHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;
/// The palette's query-edit handler (the new query).
type QueryHandler = Box<dyn Fn(String, &mut Window, &mut App) + 'static>;

/// A row of a palette result group (a label, an optional icon and
/// shortcut, an optional `.disabled(reason)`, and its action).
#[derive(Clone)]
pub struct PaletteRow {
    label: SharedString,
    icon: Option<Icon>,
    shortcut: Option<SharedString>,
    disabled_reason: Option<SharedString>,
    /// Chosen without the close step (see [`PaletteRow::keep_open`]).
    keep_open: bool,
    /// The action, shared through `Rc` so the palette's key handlers
    /// (which keep a copy of the groups) can still run it.
    on_click: Option<std::rc::Rc<ClickHandler>>,
}

impl std::fmt::Debug for PaletteRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteRow")
            .field("label", &self.label)
            .field("disabled", &self.is_disabled())
            .finish()
    }
}

impl PaletteRow {
    /// A result row with the given label (also its accessible name).
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            shortcut: None,
            disabled_reason: None,
            keep_open: false,
            on_click: None,
        }
    }

    /// A leading `ICON` icon.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A dim `type_::LABEL` shortcut at the right end.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        // Spelled out ("Ctrl+N") off macOS; identity on macOS.
        let shortcut: SharedString = shortcut.into();
        self.shortcut = Some(SharedString::from(
            crate::shortcuts::localize(&shortcut).into_owned(),
        ));
        self
    }

    /// Disable the row with its reason (dim, inert; the arrow keys skip
    /// it).
    pub fn disabled(mut self, reason: impl Into<SharedString>) -> Self {
        self.disabled_reason = Some(reason.into());
        self
    }

    /// Keep the palette open when this row is chosen: its action runs
    /// without the close step, so `.on_close(…)` is not called first. For a
    /// row that drills into a sub-page, which the palette must keep showing.
    /// Every other row closes before its action, as before.
    pub fn keep_open(mut self) -> Self {
        self.keep_open = true;
        self
    }

    /// The action, run when the row is chosen (a click or Enter, after
    /// the palette has closed unless the row is `.keep_open()`).
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(std::rc::Rc::new(Box::new(f)));
        self
    }

    /// The row's label.
    pub fn label_text(&self) -> &SharedString {
        &self.label
    }

    /// Whether the row is disabled (carries a reason).
    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    /// The disabled reason (the tooltip), if any.
    pub fn disabled_reason(&self) -> Option<SharedString> {
        self.disabled_reason.clone()
    }
}

/// A kit command palette. Build with [`Palette::new`] and chain
/// `.query(…)`, `.group(…)`, `.selected(…)` and the callbacks; the
/// parent owns the query, the groups, the selection and the open state.
#[derive(IntoElement)]
pub struct Palette {
    common: crate::ui::interaction::Common,
    query: SharedString,
    groups: Vec<(SharedString, Vec<PaletteRow>)>,
    selected: usize,
    /// Parent-owned identity for each open interval; a reopened palette must
    /// focus its SearchField even though keyed element state survives removal.
    open_generation: u64,
    scroll: Option<ScrollHandle>,
    on_move: Option<MoveHandler>,
    on_back: Option<CloseHandler>,
    on_close: Option<CloseHandler>,
    on_query: Option<QueryHandler>,
}

impl Palette {
    /// An empty palette.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            query: SharedString::from(""),
            groups: Vec::new(),
            selected: 0,
            open_generation: 0,
            scroll: None,
            on_move: None,
            on_back: None,
            on_close: None,
            on_query: None,
        }
    }

    /// The edit callback: the new query after a typed character or a
    /// Backspace (the parent stores it and re-renders with `.query(…)`).
    /// Backspace on an **empty** query calls `.on_back(…)` instead.
    pub fn on_query(mut self, f: impl Fn(String, &mut Window, &mut App) + 'static) -> Self {
        self.on_query = Some(Box::new(f));
        self
    }

    /// The current query (displayed in the search field; the parent
    /// owns it).
    pub fn query(mut self, query: impl Into<SharedString>) -> Self {
        self.query = query.into();
        self
    }

    /// Append a result group: a `type_::CAPTION` dim title and its rows.
    pub fn group(mut self, title: impl Into<SharedString>, rows: Vec<PaletteRow>) -> Self {
        self.groups.push((title.into(), rows));
        self
    }

    /// The keyboard selection: a flat index over all groups' rows (the
    /// parent re-renders the palette with the new value when
    /// `.on_move(…)` fires).
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// The parent's current open interval. A changed value focuses the
    /// SearchField again after the palette has been closed and reopened.
    pub fn open_generation(mut self, generation: u64) -> Self {
        self.open_generation = generation;
        self
    }

    /// The current keyboard selection (a flat index).
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The total number of rows, over all groups.
    pub fn row_count(&self) -> usize {
        self.groups.iter().map(|(_, rows)| rows.len()).sum()
    }

    /// The body's scroll handle (else the palette keeps its own): the
    /// palette scrolls the selected row into view itself.
    pub fn scroll_handle(mut self, handle: ScrollHandle) -> Self {
        self.scroll = Some(handle);
        self
    }

    /// The arrow-key callback: the new flat index (the parent re-renders
    /// the palette with `.selected(…)` and scrolls it into view).
    pub fn on_move(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_move = Some(Box::new(f));
        self
    }

    /// The back callback: Backspace on an **empty** query (the parent
    /// pops its history level).
    pub fn on_back(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_back = Some(Box::new(f));
        self
    }

    /// The close callback: Escape, and the close step of a choose.
    /// The parent sets its open state to closed there.
    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// The rows flattened over all groups (the `selected` index space).
    fn flat_rows(&self) -> Vec<&PaletteRow> {
        self.groups
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .collect()
    }

    /// The next (or previous, `dir = -1`) **enabled** row index from
    /// `from`, wrapping; `None` when there is no enabled row.
    pub fn step(&self, from: usize, dir: isize) -> Option<usize> {
        let rows = self.flat_rows();
        let n = rows.len();
        if n == 0 {
            return None;
        }
        let mut i = from;
        for _ in 0..n {
            i = (i as isize + dir).rem_euclid(n as isize) as usize;
            if !rows[i].is_disabled() {
                return Some(i);
            }
        }
        None
    }
}

crate::kit_shared!(Palette);

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Palette({})", self.common.selector_for(PALETTE))
    }
}

/// One character's case fold: upper-case, then lower-case, so the full
/// mappings meet (`ß` ↔ `SS` → `ss`, `İ` → `i̇`).
fn fold(c: char) -> String {
    c.to_uppercase().flat_map(char::to_lowercase).collect()
}

/// The match highlighting (unit-tested): the label split on the
/// case-insensitive query; `true` marks a matched run (painted in
/// `CONTRAST_TEXT`). An empty query or a miss yields one unmatched
/// segment.
///
/// Unicode-safe (K3b): matching runs over the **original label's
/// characters** — each folded on its own — and a match covers whole
/// characters only, so every slice is on a character boundary of the
/// label (never a byte offset taken from a lower-cased copy, whose
/// lengths can differ: `İ` is 2 bytes, its lower case 3).
pub fn highlight_parts(label: &str, query: &str) -> Vec<(String, bool)> {
    let needle: String = query.chars().map(fold).collect();
    if needle.is_empty() {
        return vec![(label.to_string(), false)];
    }
    // (byte offset in the label, the character's fold)
    let chars: Vec<(usize, String)> = label.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let byte_at = |ci: usize| chars.get(ci).map(|(b, _)| *b).unwrap_or(label.len());
    let mut parts = Vec::new();
    let (mut start, mut ci) = (0usize, 0usize);
    while ci < chars.len() {
        // Extend over whole characters while the folds still prefix the
        // needle; a match ends exactly at the needle's end.
        let mut acc = String::new();
        let mut end = None;
        for (cj, (_, f)) in chars.iter().enumerate().skip(ci) {
            acc.push_str(f);
            if acc == needle {
                end = Some(cj + 1);
                break;
            }
            if !needle.starts_with(acc.as_str()) {
                break;
            }
        }
        match end {
            Some(end) => {
                if byte_at(ci) > byte_at(start) {
                    parts.push((label[byte_at(start)..byte_at(ci)].to_string(), false));
                }
                parts.push((label[byte_at(ci)..byte_at(end)].to_string(), true));
                start = end;
                ci = end;
            }
            None => ci += 1,
        }
    }
    if byte_at(start) < label.len() {
        parts.push((label[byte_at(start)..].to_string(), false));
    }
    if parts.is_empty() {
        parts.push((label.to_string(), false));
    }
    parts
}

/// The palette's row face (the kit `Row` geometry): `RADIUS_M`, 12 px
/// horizontal and 8 px vertical padding, the `target()` minimum height;
/// the selected row washes with `FILL_HOVER`. The label's matched runs
/// paint in `CONTRAST_TEXT`.
fn row_element(
    id: ElementId,
    row: &PaletteRow,
    query: &str,
    selected: bool,
    disabled: bool,
    allow_double: bool,
    on_choose: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let p = palette();
    let fill = if disabled {
        p.transparent
    } else if selected {
        p.fill_hover
    } else {
        p.transparent
    };
    let title_color = if disabled { p.text_dim } else { p.text };

    // The label, with the query's match in CONTRAST_TEXT (the segments
    // sit inline in a row; each segment is single-line, so a long label
    // truncates the way a kit text slot does).
    let mut label_row = div()
        .flex()
        .items_center()
        .flex_grow(1.)
        .min_w(Pixels::ZERO);
    let mut row_div = div()
        .flex()
        .w_full()
        .items_center()
        .gap(theme::z(theme::SP_12))
        .px(theme::z(theme::SP_12))
        .py(theme::z(theme::SP_8))
        .min_h(theme::target())
        .rounded(theme::z(theme::RADIUS_M))
        .bg(fill)
        .id(id.clone())
        .debug_selector(|| format!("ui-palette-row-{id}"));
    if let Some(icon) = row.icon {
        row_div = row_div.child(icons::icon(
            icon,
            theme::ICON,
            if disabled { p.text_dim } else { p.text },
        ));
    }
    for (part, matched) in highlight_parts(&row.label, query) {
        // A disabled row dims everything, its matches included.
        let color = if disabled {
            p.text_dim
        } else if matched {
            p.contrast_text
        } else {
            title_color
        };
        label_row = label_row.child(crate::ui::text::ui_text(part).text_color_override(color));
    }
    row_div = row_div.child(label_row);
    if let Some(shortcut) = &row.shortcut {
        // On the selected row's `FILL_HOVER` the shortcut lifts to `TEXT_2`
        // (the brighten rule: `TEXT_DIM` is below 4.5:1 there).
        // Enabled only: a disabled row's shortcut has no id, so no tooltip
        // of its own covers the reason.
        let mut s = crate::ui::text::caption(shortcut.clone());
        if !disabled {
            s = s.id(crate::ui::interaction::child_id(&id, "shortcut"));
        }
        row_div = row_div.child(if selected && !disabled {
            s.text_color_override(p.text_2)
        } else {
            s.dim()
        });
    }
    if !disabled {
        row_div = row_div.focusable();
        row_div = row_div.on_click(move |ev, window, cx| {
            if allow_double || ev.click_count() <= 1 {
                on_choose(ev, window, cx);
            }
        });
    }
    // The tooltip: the disabled reason, else the full label (a long label
    // truncates, KIT.md § Long text).
    match &row.disabled_reason {
        Some(reason) => row_div = row_div.tooltip(icons::tip(reason.to_string())),
        None => row_div = row_div.tooltip(icons::tip(row.label.to_string())),
    }
    row_div
}

/// The next enabled flat index from `from` in direction `dir`, wrapping.
fn palette_step(rows: &[PaletteRow], from: usize, dir: isize) -> Option<usize> {
    let n = rows.len();
    let mut i = from;
    for _ in 0..n {
        i = (i as isize + dir).rem_euclid(n as isize) as usize;
        if !rows[i].is_disabled() {
            return Some(i);
        }
    }
    None
}

impl RenderOnce for Palette {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        // The palette's double-click setting reaches its rows and its scrim.
        let allow_double = common.allow_double_click;
        let selector = common.selector_for(PALETTE);
        let query = self.query.clone();
        let selected = self.selected;
        // The callbacks are shared by several closures (the rows'
        // choose, the keyboard handlers): one closure each, shared
        // through `Rc` (the handler itself is not `Clone`).
        let on_move = self.on_move.map(std::rc::Rc::new);
        let on_back = self.on_back.map(std::rc::Rc::new);
        let on_close = self.on_close.map(std::rc::Rc::new);
        let on_query = self.on_query.map(std::rc::Rc::new);
        let disabled = common.is_disabled();
        // The body's scroll handle: the parent's, else the palette's own
        // (element state under its id), so the scroll survives re-render.
        let scroll = self.scroll.clone().unwrap_or_else(|| {
            window
                .use_keyed_state(child_id(&common.id, "scroll"), cx, |_, _| {
                    ScrollHandle::new()
                })
                .read(cx)
                .clone()
        });

        // The search field is the kit's `SearchField` (K4b): it is the
        // palette's focus (the parent's handle, else the palette's own,
        // focused when the palette first shows), it edits the query
        // through `.on_query(…)`, and Backspace on an empty query goes
        // back a level. The arrows, Enter and Escape bubble up to the
        // panel's handler below.
        let handle = common.focus_handle.clone().unwrap_or_else(|| {
            window
                .use_keyed_state(child_id(&common.id, "search-focus"), cx, |_, cx| {
                    cx.focus_handle()
                })
                .read(cx)
                .clone()
        });
        let opened_generation = window
            .use_keyed_state(child_id(&common.id, "opened"), cx, |_, _| {
                std::rc::Rc::new(std::cell::Cell::new(None::<u64>))
            })
            .read(cx)
            .clone();
        if !disabled
            && opened_generation.replace(Some(self.open_generation)) != Some(self.open_generation)
        {
            window.focus(&handle, cx);
        }
        let mut search =
            crate::ui::search_field::SearchField::new(child_id(&common.id, "search"), "Search…")
                .selector(PALETTE_FIELD)
                .query(query.clone())
                .focus_handle(&handle)
                // The palette's double-click setting reaches its field's clear.
                .allow_double_click_if(allow_double);
        if let Some(reason) = common.disabled_reason.clone() {
            search = search.disabled(reason);
        }
        if let Some(on_query) = on_query.clone() {
            search = search.on_query(move |q, w, cx| on_query(q, w, cx));
        }
        if let Some(on_back) = on_back.clone() {
            search = search.on_empty_backspace(move |w, cx| on_back(w, cx));
        }
        let field = div()
            .flex()
            .w_full()
            .flex_col()
            .flex_none()
            .px(theme::z(theme::SP_16))
            .pt(theme::z(theme::SP_16))
            .pb(theme::z(theme::SP_8))
            .child(search);

        // The results: every group title and row is a **direct child** of
        // the scroll area, so a row's child index addresses it
        // (`scroll_to_item`) and the selection is kept in view.
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        let mut selected_child = None;
        let mut flat = 0usize;
        for (title, rows) in &self.groups {
            items.push(
                div()
                    .flex()
                    .w_full()
                    .pt(theme::z(theme::SP_12))
                    .pb(theme::z(theme::SP_4))
                    // A group title wraps: a long one is never cut off.
                    .child(
                        crate::ui::text::caption(title.clone())
                            .dim()
                            .strong()
                            .wrap(),
                    )
                    .into_any_element(),
            );
            for row in rows {
                if flat == selected {
                    selected_child = Some(items.len());
                }
                let row_id = child_id(&common.id, &format!("row-{flat}"));
                let row_choose = row.clone();
                let palette_focus = handle.clone();
                let on_close_in = on_close.clone();
                items.push(
                    row_element(
                        row_id,
                        row,
                        &query,
                        flat == selected,
                        row.is_disabled(),
                        allow_double,
                        move |ev, window, cx| {
                            if row_choose.is_disabled() {
                                return;
                            }
                            // The choose: close, then run the action (a
                            // `keep_open` row runs it with the palette up).
                            if let Some(on_close) =
                                on_close_in.as_ref().filter(|_| !row_choose.keep_open)
                            {
                                on_close(window, cx);
                            }
                            if let Some(on_click) = &row_choose.on_click {
                                on_click(ev, window, cx);
                            }
                            // A kept-open pointer choice (for example, a
                            // sub-page drill-in) must leave the palette ready
                            // for typing. The row is focusable, so GPUI may
                            // have focused it on mouse down before this action.
                            if row_choose.keep_open {
                                window.focus(&palette_focus, cx);
                            }
                        },
                    )
                    .into_any_element(),
                );
                flat += 1;
            }
        }
        if let Some(child) = selected_child {
            scroll.scroll_to_item(child);
        }

        // The panel never outgrows the window (less `EDGE` above and
        // below); the result list takes the rest and scrolls.
        let max_h = (window.bounds().size.height - theme::z(theme::EDGE) * 2.).max(Pixels::ZERO);
        let list = div()
            .id(child_id(&common.id, "body"))
            .flex()
            .flex_col()
            .w_full()
            .flex_grow(1.)
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .px(theme::z(theme::SP_8))
            .pb(theme::z(theme::SP_12))
            .debug_selector(|| format!("ui-palette-{id}-list", id = common.id))
            .children(items);

        let mut panel = super::panel::float_face(
            div()
                .flex()
                .flex_col()
                .w_full()
                .max_h(max_h)
                .child(field)
                .child(list),
        )
        .debug_selector(|| format!("ui-palette-{id}-panel", id = common.id))
        .id(child_id(&common.id, "panel"))
        .w(theme::z(WIDTH))
        .max_h(max_h);
        if !disabled {
            let rows: Vec<PaletteRow> = self
                .groups
                .iter()
                .flat_map(|(_, rows)| rows.iter().cloned())
                .collect();
            let on_close = on_close.clone();
            panel = panel.on_key_down(move |ev, window, cx| {
                let stop = |window: &mut Window, cx: &mut App| {
                    window.prevent_default();
                    cx.stop_propagation();
                };
                match ev.keystroke.key.as_str() {
                    "escape" => {
                        if let Some(on_close) = &on_close {
                            stop(window, cx);
                            on_close(window, cx);
                        }
                    }
                    "up" | "down" => {
                        let dir = if ev.keystroke.key == "up" { -1 } else { 1 };
                        if let Some(next) = palette_step(&rows, selected, dir) {
                            stop(window, cx);
                            if let Some(on_move) = &on_move {
                                on_move(next, window, cx);
                            }
                        }
                    }
                    "enter" => {
                        let Some(row) = rows.get(selected).cloned() else {
                            return;
                        };
                        if row.is_disabled() {
                            return;
                        }
                        stop(window, cx);
                        if let Some(on_close) = on_close.as_ref().filter(|_| !row.keep_open) {
                            on_close(window, cx);
                        }
                        let ev = ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
                        if let Some(on_click) = &row.on_click {
                            on_click(&ev, window, cx);
                        }
                    }
                    _ => {}
                }
            });
        }

        // The palette centres itself over a click-blocking scrim (a tap
        // outside closes it, and never reaches the app behind).
        let scrim = div()
            .id(child_id(&common.id, "scrim"))
            .absolute()
            .top(Pixels::ZERO)
            .left(Pixels::ZERO)
            .size_full()
            .bg(p.scrim)
            .occlude()
            .debug_selector(|| "ui-palette-scrim".to_string());
        let scrim = match on_close {
            Some(h) => scrim.on_click(move |ev, w, cx| {
                if allow_double || ev.click_count() <= 1 {
                    h(w, cx);
                }
            }),
            None => scrim,
        };
        div()
            .size_full()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .debug_selector(|| selector.clone())
            .child(scrim)
            .child(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// A long group title wraps: the palette grows rather than cutting it off.
    #[gpui::test]
    fn long_palette_group_title_wraps(cx: &mut TestAppContext) {
        let height = |cx: &mut TestAppContext, title: &'static str| {
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .size_full()
                    .child(Palette::new("cmd").group(title, vec![PaletteRow::new("Run")]))
            });
            wcx.debug_bounds("ui-palette-cmd-panel")
                .unwrap()
                .size
                .height
        };
        let short = height(cx, "Actions");
        let long = height(
            cx,
            "Actions for the open conversation, the selected reply, the running job and every connected agent",
        );
        assert!(
            long > short,
            "the long group title wrapped: {long:?} vs {short:?}"
        );
    }

    /// A long label truncates, so an enabled row tooltips its full label;
    /// the selected row's shortcut lifts to `TEXT_2` on its wash, another
    /// row's stays `TEXT_DIM`.
    #[gpui::test]
    fn palette_rows_tooltip_full_labels_and_brighten_selected_shortcuts(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{painted, same_color, tooltip_at};
        const LONG: &str = "Open the job log for the nightly backup check and every run before it";
        let (_root, wcx) = render_part(cx, || {
            div().size_full().child(
                Palette::new("cmd")
                    .group(
                        "Actions",
                        vec![
                            PaletteRow::new(LONG).shortcut("⌘L"),
                            PaletteRow::new("Ask the council").shortcut("⌘K"),
                            PaletteRow::new("Export the log")
                                .shortcut("⌘E")
                                .disabled("Nothing to export yet"),
                        ],
                    )
                    .selected(0),
            )
        });
        wcx.run_until_parked();
        assert_eq!(
            tooltip_at(wcx, "ui-palette-row-cmd-row-1").as_deref(),
            Some("Ask the council")
        );
        assert_eq!(
            tooltip_at(wcx, "ui-palette-row-cmd-row-0").as_deref(),
            Some(LONG)
        );
        let shortcut = |i: usize| {
            child_id(
                &child_id(&ElementId::from("cmd"), &format!("row-{i}")),
                "shortcut",
            )
        };
        let p = theme::palette();
        assert!(
            same_color(painted(&shortcut(0)).unwrap(), p.text_2),
            "selected row"
        );
        assert!(
            same_color(painted(&shortcut(1)).unwrap(), p.text_dim),
            "resting row"
        );
        // Palette rows have no hover wash (only the selected row has one), so
        // a hovered row's shortcut stays `TEXT_DIM` on the palette's own
        // surface, where it holds 4.5:1 (theme tests).
        crate::ui::interaction::test_util::hover(wcx, "ui-palette-row-cmd-row-1");
        assert!(
            same_color(painted(&shortcut(1)).unwrap(), p.text_dim),
            "hovered row"
        );
        // A disabled row's shortcut has no tooltip of its own: over it, the
        // row's reason opens.
        let dead = wcx.debug_bounds("ui-palette-row-cmd-row-2").unwrap();
        let over_shortcut = gpui::point(dead.right() - theme::z(theme::SP_24), dead.center().y);
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at_point(wcx, over_shortcut).as_deref(),
            Some("Nothing to export yet")
        );
    }

    /// The match highlighting: case-insensitive, multiple matches, a
    /// miss and the empty query.
    #[test]
    fn highlight_parts_split_the_label_on_the_query() {
        let parts = highlight_parts("Run as a job", "as");
        assert_eq!(
            parts,
            vec![
                ("Run ".to_string(), false),
                ("as".to_string(), true),
                (" a job".to_string(), false),
            ],
            "the match is a separate, marked segment"
        );
        // Case-insensitive.
        let parts = highlight_parts("Run as a job", "AS");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], ("as".to_string(), true));
        // Multiple matches.
        let parts = highlight_parts("as as as", "as");
        assert_eq!(
            parts.iter().filter(|(_, m)| *m).count(),
            3,
            "every occurrence is a matched segment"
        );
        // A miss: the whole label, unmatched.
        assert_eq!(
            highlight_parts("Run as a job", "zzz"),
            vec![("Run as a job".to_string(), false)]
        );
        // The empty query: one unmatched segment.
        assert_eq!(
            highlight_parts("Run as a job", ""),
            vec![("Run as a job".to_string(), false)]
        );
    }

    /// The palette: 560 wide, with its search field and groups.
    #[gpui::test]
    fn palette_is_560_wide_with_field_and_groups(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            Palette::new("cmd")
                .query("as")
                .group("Actions", vec![PaletteRow::new("Run as a job")])
                .group("Jobs", vec![PaletteRow::new("Open job")])
        });
        let panel = wcx
            .debug_bounds("ui-palette-cmd-panel")
            .unwrap_or_else(|| panic!("the palette panel recorded no bounds"));
        assert_eq!(
            f32::from(panel.size.width),
            f32::from(theme::z(WIDTH)),
            "the palette is 560 wide: {panel:?}",
        );
    }

    /// Arrow keys move the selection (skipping disabled rows, wrapping)
    /// through the parent's `on_move`; Enter runs the selected row's
    /// action and closes; Backspace on an empty query goes back a
    /// level; Escape closes.
    #[gpui::test]
    fn palette_keyboard_moves_enters_backs_and_closes(cx: &mut TestAppContext) {
        let moves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let moves_in = moves.clone();
        let events_in = events.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let sel = std::rc::Rc::new(std::cell::RefCell::new(0usize));
        let sel_in = sel.clone();
        let (_root, wcx) = render_part(cx, move || {
            let moves_in = moves_in.clone();
            let events_in = events_in.clone();
            let sel_in = sel_in.clone();
            let h = h.clone();
            let selected = *sel_in.borrow();
            div().flex().child(
                Palette::new("cmd")
                    .query(SharedString::from(""))
                    .focus_handle(&h)
                    .selected(selected)
                    .group(
                        "Actions",
                        vec![
                            PaletteRow::new("Copy").on_click({
                                let events_in = events_in.clone();
                                move |_e, _w, _c| {
                                    events_in.borrow_mut().push("action".to_string());
                                }
                            }),
                            PaletteRow::new("Locked").disabled("Read only"),
                            PaletteRow::new("Run as a job"),
                        ],
                    )
                    .on_move(move |next, _w, _c| {
                        *sel_in.borrow_mut() = next;
                        moves_in.borrow_mut().push(next);
                    })
                    .on_back({
                        let events_in = events_in.clone();
                        move |_w, _c| {
                            events_in.borrow_mut().push("back".to_string());
                        }
                    })
                    .on_close({
                        let events_in = events_in.clone();
                        move |_w, _c| {
                            events_in.borrow_mut().push("close".to_string());
                        }
                    }),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));

        fn key(wcx: &mut gpui::VisualTestContext, k: &str) {
            let ks = gpui::Keystroke::parse(k).unwrap();
            wcx.simulate_event(gpui::KeyDownEvent {
                keystroke: ks.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
            wcx.update(|w, _cx| w.refresh());
            wcx.run_until_parked();
        }

        // Down skips the disabled row 1 and lands on row 2; down wraps
        // to row 0.
        key(wcx, "down");
        key(wcx, "down");
        assert_eq!(
            moves.borrow().as_slice(),
            [2usize, 0usize],
            "down skips the disabled row and wraps"
        );

        // Backspace on the empty query goes back a level.
        key(wcx, "backspace");
        assert!(
            events.borrow().iter().any(|e| e == "back"),
            "Backspace on an empty query backs a level"
        );

        // Enter on row 0 (Copy): close, then the action.
        key(wcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            ["back", "close", "action"],
            "Enter closes and runs the selected row's action"
        );
    }

    /// Escape closes the palette (and runs nothing).
    #[gpui::test]
    fn palette_escape_closes(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            div().flex().child(
                Palette::new("cmd")
                    .focus_handle(&h)
                    .group("Actions", vec![PaletteRow::new("Copy")])
                    .on_close(move |_w, _c| {
                        closed_in.set(1);
                    }),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        let ks = gpui::Keystroke::parse("escape").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: ks.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
        assert_eq!(closed.get(), 1, "Escape closes the palette");
    }

    /// A click on a row runs the same choose (close, then the action);
    /// a disabled row is inert.
    #[gpui::test]
    fn palette_row_click_runs_the_choose(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_in = events.clone();
        let (_root, wcx) = render_part(cx, move || {
            let events_in = events_in.clone();
            div().flex().child(
                Palette::new("cmd")
                    .group(
                        "Actions",
                        vec![
                            PaletteRow::new("Locked").disabled("Read only"),
                            PaletteRow::new("Copy").on_click({
                                let events_in = events_in.clone();
                                move |_e, _w, _c| {
                                    events_in.borrow_mut().push("action".to_string());
                                }
                            }),
                        ],
                    )
                    .on_close({
                        let events_in = events_in.clone();
                        move |_w, _c| {
                            events_in.borrow_mut().push("close".to_string());
                        }
                    }),
            )
        });

        // The disabled row is inert.
        let dead = wcx
            .debug_bounds("ui-palette-row-cmd-row-0")
            .unwrap_or_else(|| panic!("the disabled row recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: dead.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: dead.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert!(events.borrow().is_empty(), "the disabled row is inert");

        // The enabled row: close, then the action.
        let row = wcx
            .debug_bounds("ui-palette-row-cmd-row-1")
            .unwrap_or_else(|| panic!("the enabled row recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: row.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: row.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(
            events.borrow().as_slice(),
            ["close", "action"],
            "a row click closes and runs the action"
        );
    }

    /// K3b item 8: highlighting is Unicode-safe — it slices the original
    /// label only on character boundaries, whatever the case folding does
    /// to byte lengths.
    #[test]
    fn highlight_is_unicode_safe() {
        let joined =
            |parts: &[(String, bool)]| parts.iter().map(|(s, _)| s.as_str()).collect::<String>();
        // `İ` is 2 bytes; its lower case is 3 — the old byte-offset code
        // sliced `3..4` out of a 3-byte label and panicked.
        let p = highlight_parts("İx", "x");
        assert_eq!(p, vec![("İ".to_string(), false), ("x".to_string(), true)]);
        // Full case folding: `ß` matches `SS` (and `ss`), as one character.
        let p = highlight_parts("straße", "SS");
        assert_eq!(
            p,
            vec![
                ("stra".to_string(), false),
                ("ß".to_string(), true),
                ("e".to_string(), false)
            ]
        );
        assert_eq!(joined(&highlight_parts("straße", "ss")), "straße");
        // A query that would end mid-character does not match.
        assert_eq!(
            highlight_parts("straße", "s"),
            vec![("s".to_string(), true), ("traße".to_string(), false)]
        );
        // Emoji: matched whole, and text after them still matches.
        let p = highlight_parts("🚀 Launch the job", "launch");
        assert_eq!(joined(&p), "🚀 Launch the job");
        assert!(p.contains(&("Launch".to_string(), true)));
        let p = highlight_parts("🚀 Launch", "🚀");
        assert_eq!(p[0], ("🚀".to_string(), true));
        // Every case reassembles to the label.
        for (label, q) in [
            ("İİx", "i̇x"),
            ("ﬁle ﬁx", "fi"),
            ("ÉCOLE", "école"),
            ("", "x"),
        ] {
            assert_eq!(
                joined(&highlight_parts(label, q)),
                label,
                "{label:?} / {q:?}"
            );
        }
    }

    fn press(wcx: &mut gpui::VisualTestContext, k: &str) {
        wcx.simulate_keystrokes(k);
        wcx.run_until_parked();
    }

    /// K3b item 7: real search — typed characters append to the query
    /// (`.on_query`), Backspace on a non-empty query deletes the last
    /// character, Backspace on an empty one goes back (`.on_back`); the
    /// parent owns the query and re-renders with it.
    #[gpui::test]
    fn palette_search_edits_the_query(cx: &mut TestAppContext) {
        let query = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let backs = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let (q, b) = (query.clone(), backs.clone());
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let (q2, b2) = (q.clone(), b.clone());
            Palette::new("cmd")
                .query(q.borrow().clone())
                .focus_handle(&h)
                .group("Actions", vec![PaletteRow::new("Run as a job")])
                .on_query(move |new, _w, _cx| *q2.borrow_mut() = new)
                .on_back(move |_w, _cx| b2.set(b2.get() + 1))
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        press(wcx, "r u n");
        assert_eq!(query.borrow().as_str(), "run", "typing edits the query");
        press(wcx, "backspace");
        assert_eq!(
            query.borrow().as_str(),
            "ru",
            "Backspace deletes a character"
        );
        press(wcx, "backspace backspace");
        assert_eq!(query.borrow().as_str(), "");
        assert_eq!(backs.get(), 0, "no back while the query had text");
        press(wcx, "backspace");
        assert_eq!(backs.get(), 1, "Backspace on an empty query goes back");
        press(wcx, "cmd-a");
        assert_eq!(query.borrow().as_str(), "", "a command chord does not type");
    }

    /// K3b item 7: the palette centres itself over a click-blocking scrim
    /// (a tap outside closes it and never reaches the app behind), and its
    /// result list is constrained and keeps the selected row in view.
    #[gpui::test]
    fn palette_centres_and_keeps_the_selection_in_view(cx: &mut TestAppContext) {
        let behind = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let closes = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let (bh, cl) = (behind.clone(), closes.clone());
        let rows: Vec<PaletteRow> = (0..60)
            .map(|i| PaletteRow::new(format!("Row {i}")))
            .collect();
        let (_handle, wcx) = crate::ui::interaction::test_util::render_part_at(
            cx,
            gpui::size(gpui::px(900.), gpui::px(500.)),
            move || {
                let (bh, cl) = (bh.clone(), cl.clone());
                div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .id("behind")
                            .absolute()
                            .size_full()
                            .on_click(move |_e, _w, _cx| bh.set(bh.get() + 1)),
                    )
                    .child(
                        div().absolute().size_full().child(
                            Palette::new("cmd")
                                .group("Rows", rows.clone())
                                .selected(55)
                                .on_close(move |_w, _cx| cl.set(cl.get() + 1)),
                        ),
                    )
            },
        );
        // The settle frame (the scroll-to-item applies at layout).
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let panel = wcx.debug_bounds("ui-palette-cmd-panel").unwrap();
        let (cx_mid, cy_mid) = (
            f32::from(panel.origin.x) + f32::from(panel.size.width) / 2.,
            f32::from(panel.origin.y) + f32::from(panel.size.height) / 2.,
        );
        assert!(
            (cx_mid - 450.).abs() < 1.0 && (cy_mid - 250.).abs() < 1.0,
            "centred: {panel:?}"
        );
        assert!(
            f32::from(panel.size.height) <= 500. - 2. * f32::from(theme::z(theme::EDGE)) + 0.5,
            "the list is constrained: {panel:?}"
        );
        let list = wcx.debug_bounds("ui-palette-cmd-list").unwrap();
        let row = wcx.debug_bounds("ui-palette-row-cmd-row-55").unwrap();
        assert!(
            row.origin.y >= list.origin.y
                && row.origin.y + row.size.height
                    <= list.origin.y + list.size.height + gpui::px(0.5),
            "the selected row is scrolled into view: row {row:?} in list {list:?}"
        );
        // A tap outside the panel: closes, and never reaches the app.
        let at = gpui::point(gpui::px(10.), gpui::px(10.));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        wcx.run_until_parked();
        assert_eq!(
            (behind.get(), closes.get()),
            (0, 1),
            "scrim: closes, blocks"
        );
    }

    /// A real left click at the centre of the palette's row `i`.
    fn click_palette_row(wcx: &mut gpui::VisualTestContext, i: usize) {
        let key: &'static str = Box::leak(format!("ui-palette-row-cmd-row-{i}").into_boxed_str());
        let at = wcx
            .debug_bounds(key)
            .unwrap_or_else(|| panic!("row {i} recorded no bounds"))
            .center();
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        wcx.run_until_parked();
    }

    /// The opt-in close path, pointer: a `keep_open` row runs its action and
    /// never calls `.on_close(…)`; a default row in the same palette still
    /// closes first and then runs its action, as before.
    #[gpui::test]
    fn palette_keep_open_row_click_runs_without_closing(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let query = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let handle = cx.update(|cx| cx.focus_handle());
        let (events_in, query_in, palette_handle) = (events.clone(), query.clone(), handle.clone());
        let (_root, wcx) = render_part(cx, move || {
            let events_in = events_in.clone();
            let query_in = query_in.clone();
            div().flex().child(
                Palette::new("cmd")
                    .focus_handle(&palette_handle)
                    .group(
                        "Actions",
                        vec![
                            PaletteRow::new("Open colour mode").keep_open().on_click({
                                let events_in = events_in.clone();
                                move |_e, _w, _c| {
                                    events_in.borrow_mut().push("keep".to_string());
                                }
                            }),
                            PaletteRow::new("Copy").on_click({
                                let events_in = events_in.clone();
                                move |_e, _w, _c| {
                                    events_in.borrow_mut().push("action".to_string());
                                }
                            }),
                        ],
                    )
                    .on_close({
                        let events_in = events_in.clone();
                        move |_w, _c| {
                            events_in.borrow_mut().push("close".to_string());
                        }
                    })
                    .on_query(move |new, _w, _c| *query_in.borrow_mut() = new),
            )
        });

        click_palette_row(wcx, 0);
        assert_eq!(
            events.borrow().as_slice(),
            ["keep"],
            "a keep-open row runs its action without closing"
        );
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(handle.clone()),
            "a kept-open pointer choice restores focus to the search field"
        );
        press(wcx, "x");
        assert_eq!(query.borrow().as_str(), "x", "typing still edits the query");
        click_palette_row(wcx, 1);
        assert_eq!(
            events.borrow().as_slice(),
            ["keep", "close", "action"],
            "a default row still closes before its action"
        );
    }

    /// The opt-in close path, keyboard: Enter on a `keep_open` row runs its
    /// action with the palette still up; Enter on a default row closes first.
    #[gpui::test]
    fn palette_keep_open_row_enter_runs_without_closing(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sel = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let handle = cx.update(|cx| cx.focus_handle());
        let (events_in, sel_in, h) = (events.clone(), sel.clone(), handle.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (keep_log, plain_log, close_log) =
                (events_in.clone(), events_in.clone(), events_in.clone());
            let move_sel = sel_in.clone();
            Palette::new("cmd")
                .focus_handle(&h)
                .selected(sel_in.get())
                .group(
                    "Actions",
                    vec![
                        PaletteRow::new("Open colour mode").keep_open().on_click(
                            move |_e, _w, _c| {
                                keep_log.borrow_mut().push("keep".to_string());
                            },
                        ),
                        PaletteRow::new("Copy").on_click(move |_e, _w, _c| {
                            plain_log.borrow_mut().push("action".to_string());
                        }),
                    ],
                )
                .on_move(move |next, _w, _c| move_sel.set(next))
                .on_close(move |_w, _c| {
                    close_log.borrow_mut().push("close".to_string());
                })
        });
        wcx.update(|w, cx| w.focus(&handle, cx));

        press(wcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            ["keep"],
            "Enter on a keep-open row runs its action without closing"
        );

        press(wcx, "down");
        wcx.update(|w, _cx| w.refresh());
        wcx.run_until_parked();
        assert_eq!(sel.get(), 1, "down moves to the default row");
        press(wcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            ["keep", "close", "action"],
            "Enter on a default row still closes before its action"
        );
    }
}
