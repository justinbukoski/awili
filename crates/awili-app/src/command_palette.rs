//! Cowork command-palette UI + navigation engine (plan unit 29, Phase F,
//! review-05 §Command Palette).
//!
//! Unit 28 ([`crate::command_catalog`]) built the PURE half of the Cowork/Hermes
//! Cmd+K palette: the hand-authored static [`CATALOG`], the word-AND substring
//! filter [`every_word_matches`], and the [`search`] aggregation that folds the
//! static rows together with the four lazy provider result sets (recent
//! sessions, settings fields, MCP servers, archived chats). This module is the
//! UI half: a pure keyboard/navigation state machine over that result set —
//! including the **nested sub-pages** review-05 calls for — plus the GPUI
//! overlay that renders it (grouped rows, keybind hints, a focused-row
//! highlight, and the sub-page drill-down).
//!
//! ## Nested sub-pages
//!
//! review-05 line 32 lists the Cowork palette's nested sub-pages as
//! theme / color-mode / a VS Code theme *marketplace* (the marketplace is an
//! Electron-IPC affordance, explicitly "droppable" and not portable to Awili).
//! Awili ships a single hand-tuned theme, so there is no theme LIST to drill
//! into — but the light/dark/system **color mode** choice is universal, so
//! that is the one concrete nested page implemented here ([`Page::ColorMode`]).
//! The `appearance.color_mode` catalog row drills into it; every other catalog
//! row and every dynamic provider row is a direct action. The page machinery
//! ([`Page`], [`CommandPaletteState::back`]) is general, so a future theme list
//! (or any settings sub-page) slots in without reworking navigation.
//!
//! ## What is code-verifiable vs. Owner-gated
//!
//! Everything in this module is `#[test]`-covered and window-free: the row
//! resolution, the wrap-around selection, the drill-in/drill-out transitions,
//! and the action each Enter yields. The GPUI [`command_palette`] render is a
//! side-effect-free consumer of that state (the usage_chart / palette
//! precedent). What is **NOT** here — and is intentionally Owner-gated — is
//! *wiring* this palette into the app window: whether Cmd+K opens this Cowork
//! catalog or Awili's native conversation palette ([`crate::palette`]) is a UX
//! decision, and the four provider fetches (`listAllProfileSessions`, settings
//! fields, MCP servers from config, archived chats) are live calls. Until that
//! integration lands, the module carries a `dead_code` allow exactly like
//! [`crate::usage_chart`] and [`crate::command_catalog`]; the engine tests
//! exercise it regardless.
#![allow(dead_code)]

use crate::command_catalog::{
    every_word_matches, search, CatalogEntry, CatalogGroup, DynEntry, PaletteHit, ProviderKind,
};
use crate::ui::{Palette, PaletteRow};
use gpui::IntoElement;

/// The light/dark/system color-mode choice — the one concrete nested sub-page
/// (see the module docs). Portable across any windowing backend, unlike the
/// droppable VS Code theme marketplace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorModeChoice {
    Light,
    Dark,
    System,
}

impl ColorModeChoice {
    /// All choices in display order.
    pub const ALL: [ColorModeChoice; 3] = [
        ColorModeChoice::Light,
        ColorModeChoice::Dark,
        ColorModeChoice::System,
    ];

    /// Stable action id the app maps to a color-mode command.
    pub fn id(self) -> &'static str {
        match self {
            ColorModeChoice::Light => "appearance.color_mode.light",
            ColorModeChoice::Dark => "appearance.color_mode.dark",
            ColorModeChoice::System => "appearance.color_mode.system",
        }
    }

    /// Displayed label.
    pub fn label(self) -> &'static str {
        match self {
            ColorModeChoice::Light => "Light",
            ColorModeChoice::Dark => "Dark",
            ColorModeChoice::System => "System (match OS)",
        }
    }
}

/// The catalog id whose Enter drills into the color-mode sub-page rather than
/// firing a direct action. Kept as a named constant so the engine and any
/// future wiring agree on the one special row.
pub const COLOR_MODE_ID: &str = "appearance.color_mode";

/// Which page of the palette is showing. The palette is a shallow stack: the
/// root catalog, or one nested sub-page. `back` pops a sub-page to `Root`, and
/// pops `Root` by closing the palette (Cowork's Escape behavior).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// The aggregated static-catalog-plus-providers list.
    Root,
    /// The light/dark/system color-mode drill-down.
    ColorMode,
}

/// One selectable, rendered row of the current page. Headers are NOT rows (they
/// are derived during rendering from adjacent rows' groups), so `focused_idx`
/// only ever indexes real, actionable rows — the palette can never highlight a
/// non-actionable header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelRow {
    /// A static catalog entry (root page).
    Catalog(&'static CatalogEntry),
    /// A dynamic provider result (root page).
    Dyn { kind: ProviderKind, entry: DynEntry },
    /// A color-mode choice (color-mode sub-page).
    ColorMode(ColorModeChoice),
}

impl SelRow {
    /// The label shown for this row.
    pub fn label(&self) -> &str {
        match self {
            SelRow::Catalog(e) => e.label,
            SelRow::Dyn { entry, .. } => &entry.text,
            SelRow::ColorMode(c) => c.label(),
        }
    }

    /// The optional keybind hint shown at the row's right edge (static catalog
    /// rows only; dynamic and sub-page rows have none).
    pub fn keybind(&self) -> Option<&'static str> {
        match self {
            SelRow::Catalog(e) => e.keybind,
            _ => None,
        }
    }

    /// True when Entering this row drills into a nested sub-page rather than
    /// firing an action (the color-mode catalog row). The render shows a `›`
    /// affordance for these.
    pub fn opens_sub_page(&self) -> bool {
        matches!(self, SelRow::Catalog(e) if e.id == COLOR_MODE_ID)
    }
}

/// What an Enter on the focused row resolves to. The caller (the window layer)
/// executes it: `Run*` fire the mapped command and should close the palette;
/// `OpenedSubPage` mutated the state in place and the palette stays open;
/// `None` is a no-op (empty list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    /// Nothing focused / empty list.
    None,
    /// A sub-page was pushed; state mutated, keep the palette open.
    OpenedSubPage,
    /// Fire the static catalog action with this stable id, then close.
    RunCatalog(&'static str),
    /// Fire the dynamic provider action (kind + opaque id), then close.
    RunDynamic(ProviderKind, String),
    /// Apply the color mode, then close.
    SetColorMode(ColorModeChoice),
}

/// The Cowork command-palette view state — open flag, live query, focused row,
/// and current page. Pure; the GPUI layer mirrors the query from its search
/// input and reads `focused_idx` to paint the highlight, exactly like
/// [`crate::palette::PaletteState`].
#[derive(Clone, Debug)]
pub struct CommandPaletteState {
    /// The overlay is showing.
    pub open: bool,
    /// Increments on each open so the rendered Palette can restore its search
    /// focus after the component was absent for a closed interval.
    pub open_generation: u64,
    /// Live filter text, mirrored from the search input.
    pub query: String,
    /// Index of the focused row within the current page's [`visible_rows`].
    pub focused_idx: usize,
    /// Root catalog or a nested sub-page.
    pub page: Page,
}

impl Default for CommandPaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPaletteState {
    pub fn new() -> Self {
        Self {
            open: false,
            open_generation: 0,
            query: String::new(),
            focused_idx: 0,
            page: Page::Root,
        }
    }

    /// Open fresh at the root page: a stale query, focus, or sub-page from the
    /// last open would make Cmd+K land somewhere surprising (the native
    /// palette's `open` makes the same choice).
    pub fn open(&mut self) {
        if self.open {
            return;
        }
        self.query.clear();
        self.focused_idx = 0;
        self.page = Page::Root;
        self.open_generation = self.open_generation.wrapping_add(1);
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Update the query. A changed query rebuilds the row list, so focus snaps
    /// back to the top — which also keeps `focused_idx` in range as the list
    /// narrows (it can never exceed a length it was reset for).
    pub fn set_query(&mut self, q: &str) {
        if self.query != q {
            self.query = q.to_string();
            self.focused_idx = 0;
        }
    }

    /// Append typed text to the live query. The overlay draws a query *mirror*
    /// (there is no embedded text field), so while the palette owns keyboard
    /// input the window feeds printable keys through here — routed via
    /// [`set_query`] so focus still snaps to the top on the change.
    pub fn push_query(&mut self, s: &str) {
        let mut q = self.query.clone();
        q.push_str(s);
        self.set_query(&q);
    }

    /// Delete the last character of the live query (Backspace), snapping focus
    /// to the top like any other query change. A no-op on an empty query.
    pub fn backspace(&mut self) {
        let mut q = self.query.clone();
        q.pop();
        self.set_query(&q);
    }

    /// Focus the previous row, wrapping from top to bottom (native-palette
    /// semantics). `list_len` is the current [`visible_rows`] length. An
    /// out-of-range `focused_idx` — which happens when a provider fetch lands
    /// or shrinks the list without a query change (only `set_query` resets
    /// focus) — is recovered by wrapping to the last row rather than blindly
    /// decrementing into a still-invalid index.
    pub fn move_up(&mut self, list_len: usize) {
        if list_len == 0 {
            self.focused_idx = 0;
        } else if self.focused_idx == 0 || self.focused_idx >= list_len {
            self.focused_idx = list_len - 1;
        } else {
            self.focused_idx -= 1;
        }
    }

    /// Focus the next row, wrapping from bottom to top. The `>= list_len - 1`
    /// guard both wraps at the bottom and recovers a stale out-of-range index
    /// (and avoids a `usize::MAX + 1` overflow that a modulo form would hit).
    pub fn move_down(&mut self, list_len: usize) {
        if list_len == 0 || self.focused_idx >= list_len - 1 {
            self.focused_idx = 0;
        } else {
            self.focused_idx += 1;
        }
    }

    /// The rendered/selectable rows for the current page, given the four lazily
    /// fetched provider result sets (empty slices are fine — the static catalog
    /// still resolves, so the palette is useful before any fetch completes).
    ///
    /// - [`Page::Root`] delegates to [`search`], preserving its static-then-
    ///   provider ordering.
    /// - [`Page::ColorMode`] lists the color-mode choices, filtered by the same
    ///   word-AND substring matcher for consistency with the root page.
    pub fn visible_rows(
        &self,
        recent_sessions: &[DynEntry],
        settings_fields: &[DynEntry],
        mcp_servers: &[DynEntry],
        archived_chats: &[DynEntry],
    ) -> Vec<SelRow> {
        match self.page {
            Page::Root => search(
                &self.query,
                recent_sessions,
                settings_fields,
                mcp_servers,
                archived_chats,
            )
            .into_iter()
            .map(|hit| match hit {
                PaletteHit::Static(e) => SelRow::Catalog(e),
                PaletteHit::Dynamic { kind, entry } => SelRow::Dyn { kind, entry },
            })
            .collect(),
            Page::ColorMode => ColorModeChoice::ALL
                .into_iter()
                .filter(|c| every_word_matches(c.label(), &self.query))
                .map(SelRow::ColorMode)
                .collect(),
        }
    }

    /// Execute the focused row. Drilling into a sub-page mutates the state in
    /// place (clears the query, resets focus) and returns [`PaletteAction::
    /// OpenedSubPage`]; every other row returns the action for the caller to
    /// fire and then close on. `rows` must be the current [`visible_rows`].
    pub fn enter(&mut self, rows: &[SelRow]) -> PaletteAction {
        let Some(row) = rows.get(self.focused_idx) else {
            return PaletteAction::None;
        };
        match row {
            SelRow::Catalog(e) if e.id == COLOR_MODE_ID => {
                self.page = Page::ColorMode;
                self.query.clear();
                self.focused_idx = 0;
                PaletteAction::OpenedSubPage
            }
            SelRow::Catalog(e) => PaletteAction::RunCatalog(e.id),
            SelRow::Dyn { kind, entry } => PaletteAction::RunDynamic(*kind, entry.id.clone()),
            SelRow::ColorMode(c) => PaletteAction::SetColorMode(*c),
        }
    }

    /// Escape / back-navigation. From a sub-page it pops to the root (clearing
    /// the query and focus); from the root it closes the palette. Returns
    /// `true` iff the palette was closed — mirrors Cowork, where Escape steps
    /// back a page before it dismisses.
    pub fn back(&mut self) -> bool {
        match self.page {
            Page::Root => {
                self.close();
                true
            }
            _ => {
                self.page = Page::Root;
                self.query.clear();
                self.focused_idx = 0;
                false
            }
        }
    }
}

// ---- window-input helpers -------------------------------------------------

/// A raw keystroke's effect on the query when the overlay owns keyboard input
/// directly. Navigation / Run / Dismiss keys are classified upstream by
/// [`crate::palette::classify_key`]; this interprets only what remains — the
/// printable characters that extend the query and Backspace that shortens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryKey {
    /// Append this string to the query (a printable character, or a space).
    Append(String),
    /// Delete the last character (Backspace / Delete).
    Backspace,
    /// Not a query edit (a chord, an arrow, a function key) — ignore it.
    Ignore,
}

/// Interpret a gpui keystroke `key` (already lowercased by gpui) as a query
/// edit, given whether a command/ctrl/alt/fn `chord` is held. Only lone
/// printable characters and the space bar extend the query; Backspace/Delete
/// shorten it; everything else is ignored (arrows are handled by the shared
/// classifier, chords are not text). Single-character keys cover the whole
/// visible catalog vocabulary, and the substring filter is case-insensitive so
/// the un-shifted key name is enough.
/// RC2-UX (D-PUNCH F15: "model" was entered as "aaaaa"): the typed
/// character wins over the key's name — the key name is the key, not what
/// it typed (another layout, Shift, an input tool). Backspace/Delete and
/// chords as [`query_key`].
pub fn query_input(key: &str, key_char: Option<&str>, chord: bool) -> QueryKey {
    if chord {
        return QueryKey::Ignore;
    }
    match key_char {
        Some(c)
            if !matches!(key, "backspace" | "delete")
                && !c.is_empty()
                && !c.chars().any(char::is_control) =>
        {
            QueryKey::Append(c.to_string())
        }
        _ => query_key(key, chord),
    }
}

pub fn query_key(key: &str, chord: bool) -> QueryKey {
    if chord {
        return QueryKey::Ignore;
    }
    match key {
        "backspace" | "delete" => QueryKey::Backspace,
        "space" => QueryKey::Append(" ".to_string()),
        k if k.chars().count() == 1 => QueryKey::Append(k.to_string()),
        _ => QueryKey::Ignore,
    }
}

/// The Awili window command a [`PaletteAction::RunCatalog`] id triggers. The
/// id→command mapping is pure and unit-tested here so the window layer's match
/// stays exhaustive and a new/renamed catalog id can never silently resolve to
/// the wrong surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteNav {
    /// Start a fresh conversation (`goto.new_conversation`).
    NewConversation,
    /// Open the Feed / Needs-You panel (`goto.feed`).
    OpenFeed,
    /// Switch to the Command Center dashboard (`goto.command_center`, `cc.*`).
    GoCommandCenter,
    /// Switch to the Skills view (`goto.skills`).
    GoSkills,
    /// Switch to the Messaging connectors pane (`goto.messaging`).
    GoMessaging,
    /// Open the Settings modal (`goto.settings`, `settings.*`).
    OpenSettings,
    /// Open Settings on its Appearance page.
    OpenAppearance,
    /// An unrecognised id closes with no side effect.
    Noop,
}

/// Map a static catalog id (from [`PaletteAction::RunCatalog`]) to the Awili
/// window command it fires. The `cc.*` Command-Center deep-links all resolve to
/// the single Command Center dashboard (Awili has no sub-tabs to deep-link
/// into), and every `settings.*` field row opens the Settings modal (Awili's
/// settings is one modal, not per-section pages). Appearance rows and anything
/// unrecognised are [`PaletteNav::Noop`].
pub fn catalog_nav(id: &str) -> PaletteNav {
    match id {
        "goto.new_conversation" => PaletteNav::NewConversation,
        "goto.feed" => PaletteNav::OpenFeed,
        "goto.command_center" | "cc.sessions" | "cc.system" | "cc.usage" => {
            PaletteNav::GoCommandCenter
        }
        "goto.skills" => PaletteNav::GoSkills,
        "goto.messaging" => PaletteNav::GoMessaging,
        "goto.settings"
        | "settings.general"
        | "settings.model"
        | "settings.providers"
        | "settings.messaging"
        | "settings.environment" => PaletteNav::OpenSettings,
        "appearance.theme" | "appearance.color_mode" => PaletteNav::OpenAppearance,
        _ => PaletteNav::Noop,
    }
}

/// Where a dynamic provider row's [`PaletteAction::RunDynamic`] navigates,
/// the provider-side parallel to [`catalog_nav`] for the static rows. Kept pure
/// (no window) so the routing is unit-testable; the window performs the actual
/// open as a side effect. The `id` is the opaque provider id the mapper stored
/// ([`command_catalog::recent_sessions_to_entries`] keys it to the Hermes
/// session id, [`command_catalog::archived_chats_to_entries`] to the local
/// conversation id) — the window opens exactly that item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynNav {
    /// Open the Sessions browser focused on this session id (`RecentSessions`).
    OpenSession(String),
    /// Open this archived conversation from history (`ArchivedChats`).
    OpenArchived(String),
    /// Jump to the Settings modal (`SettingsFields`). No live rows ship for this
    /// provider yet (the decoded schema isn't retained on the window), so this
    /// is unreachable today — but the routing is fixed for when it is wired.
    OpenSettings,
    /// No backing surface yet (`McpServers` is sourceless). Close cleanly.
    Noop,
}

/// Map a fired dynamic-provider action (kind + opaque id) to the Awili window
/// command it opens. Total over [`ProviderKind`] so a newly-wired provider is a
/// compile error here until it is routed.
pub fn dynamic_nav(kind: ProviderKind, id: String) -> DynNav {
    match kind {
        ProviderKind::RecentSessions => DynNav::OpenSession(id),
        ProviderKind::ArchivedChats => DynNav::OpenArchived(id),
        ProviderKind::SettingsFields => DynNav::OpenSettings,
        ProviderKind::McpServers => DynNav::Noop,
    }
}

// ---- rendering ------------------------------------------------------------

/// Display header for a static catalog group.
fn group_header(g: CatalogGroup) -> &'static str {
    match g {
        CatalogGroup::GoTo => "Go to",
        CatalogGroup::CommandCenter => "Command Center",
        CatalogGroup::Appearance => "Appearance",
        CatalogGroup::Settings => "Settings",
    }
}

/// Display header for a dynamic provider group.
fn provider_header(k: ProviderKind) -> &'static str {
    match k {
        ProviderKind::RecentSessions => "Recent sessions",
        ProviderKind::SettingsFields => "Settings fields",
        ProviderKind::McpServers => "MCP servers",
        ProviderKind::ArchivedChats => "Archived chats",
    }
}

/// The group title a row renders under: its catalog group, its provider, or
/// the one color-mode page's group.
fn row_section(row: &SelRow) -> &'static str {
    match row {
        SelRow::Catalog(e) => group_header(e.group),
        SelRow::Dyn { kind, .. } => provider_header(*kind),
        SelRow::ColorMode(_) => "Color mode",
    }
}

/// The palette's element id; its rows' ids and selectors derive from it.
const PALETTE_ID: &str = "cowork-command-palette";

/// The row click-handler trait object (user-test N2): the boxed `dyn Fn`
/// spelling factored out so the signature stays under the type-complexity
/// limit.
type PaletteClickHandler = dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static;
/// The close callback: the scrim click, and a default row's close step.
type CloseHandler = dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static;
/// The query callback: the new query after a typed character or Backspace.
type QueryHandler = dyn Fn(String, &mut gpui::Window, &mut gpui::App) + 'static;

/// The live provider results assembled by the app for this render. The
/// renderer borrows them and keeps their ordering/filtering in the state.
pub(crate) struct PaletteSources<'a> {
    pub(crate) recent_sessions: &'a [DynEntry],
    pub(crate) settings_fields: &'a [DynEntry],
    pub(crate) mcp_servers: &'a [DynEntry],
    pub(crate) archived_chats: &'a [DynEntry],
}

/// The Cmd+K overlay. Returns `None` when the palette is closed. Renders the
/// kit [`Palette`] over the current page: the rows grouped under section titles
/// (catalog group, provider, or `Color mode`), the focused row selected, keybind
/// hints right-aligned, and a `›` on the color-mode drill-in. Pure: it reads
/// `state` and the provider slices and fires no effects. The window owns
/// the state, the actions and the provider fetches, and hears back through:
///
/// - `row_listener(i)`: the action for flat row `i` (the index `focused_idx`
///   points at). A pointer choose runs it; Enter does too, once the window routes
///   Enter to the focused row.
/// - `on_close`: dismisses the palette (the scrim click, and the close step a
///   default row takes before its action). The color-mode drill-in opts out of
///   that step, so choosing it keeps the palette open.
/// - `on_query`: the new query after each typed character or Backspace. The
///   window stores it with [`CommandPaletteState::set_query`].
pub fn command_palette<FR>(
    state: &CommandPaletteState,
    sources: PaletteSources<'_>,
    mut row_listener: FR,
    on_close: Box<CloseHandler>,
    on_query: Box<QueryHandler>,
) -> Option<impl IntoElement>
where
    FR: FnMut(usize) -> Box<PaletteClickHandler>,
{
    if !state.open {
        return None;
    }

    let rows = state.visible_rows(
        sources.recent_sessions,
        sources.settings_fields,
        sources.mcp_servers,
        sources.archived_chats,
    );

    // Group the rows under their section titles. A title opens wherever the
    // section changes, and the reducer's order is kept: a row's position in
    // `rows` is its flat index, so `selected` lines up with `focused_idx`.
    let mut groups: Vec<(&'static str, Vec<PaletteRow>)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut palette_row = PaletteRow::new(row.label()).on_click(row_listener(i));
        if row.opens_sub_page() {
            // The drill-in pushes a sub-page, so it must keep the palette up.
            palette_row = palette_row.shortcut("›").keep_open();
        } else if let Some(kb) = row.keybind() {
            palette_row = palette_row.shortcut(kb);
        }
        let section = row_section(row);
        match groups.last_mut() {
            Some((title, group)) if *title == section => group.push(palette_row),
            _ => groups.push((section, vec![palette_row])),
        }
    }
    if groups.is_empty() {
        groups.push(("No matches", Vec::new()));
    }

    let mut palette = Palette::new(PALETTE_ID)
        .open_generation(state.open_generation)
        .query(state.query.clone())
        .selected(state.focused_idx)
        .on_query(on_query)
        .on_close(on_close);
    for (title, group) in groups {
        palette = palette.group(title, group);
    }
    Some(palette)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::interaction::child_id;
    use crate::ui::interaction::test_util::{painted, render_part_at, same_color, tooltip_at};
    use gpui::{div, prelude::*, ElementId, TestAppContext, VisualTestContext};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    type ClickFn = Box<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static>;

    fn test_noop() -> ClickFn {
        Box::new(|_, _, _| {})
    }

    fn test_row_listener(_ix: usize) -> ClickFn {
        test_noop()
    }

    fn test_close() -> Box<CloseHandler> {
        Box::new(|_, _| {})
    }

    fn test_query() -> Box<QueryHandler> {
        Box::new(|_, _, _| {})
    }

    fn dyn_rows() -> (Vec<DynEntry>, Vec<DynEntry>, Vec<DynEntry>, Vec<DynEntry>) {
        (
            vec![DynEntry::new("s1", "recent session alpha")],
            vec![DynEntry::new("f1", "settings field beta")],
            vec![DynEntry::new("m1", "mcp server gamma")],
            vec![DynEntry::new("a1", "archived chat delta")],
        )
    }

    #[test]
    fn open_resets_to_clean_root() {
        let mut s = CommandPaletteState::new();
        s.query = "stale".into();
        s.focused_idx = 5;
        s.page = Page::ColorMode;
        s.open();
        assert!(s.open);
        assert_eq!(s.open_generation, 1);
        assert_eq!(s.query, "");
        assert_eq!(s.focused_idx, 0);
        assert_eq!(s.page, Page::Root);
        s.close();
        s.open();
        assert_eq!(s.open_generation, 2, "each open has a fresh focus interval");
    }

    #[test]
    fn opening_an_open_palette_preserves_its_session() {
        let mut s = CommandPaletteState::new();
        s.open();
        s.set_query("typed");
        s.page = Page::ColorMode;
        s.focused_idx = 2;
        let generation = s.open_generation;
        s.open();
        assert_eq!(s.query, "typed");
        assert_eq!(s.page, Page::ColorMode);
        assert_eq!(s.focused_idx, 2);
        assert_eq!(s.open_generation, generation);
    }

    #[test]
    fn set_query_snaps_focus_to_top() {
        let mut s = CommandPaletteState::new();
        s.focused_idx = 3;
        s.set_query("theme");
        assert_eq!(s.focused_idx, 0);
        // No-op when unchanged: focus is preserved.
        s.focused_idx = 2;
        s.set_query("theme");
        assert_eq!(s.focused_idx, 2);
    }

    #[test]
    fn move_wraps_both_directions() {
        let mut s = CommandPaletteState::new();
        s.move_up(3); // 0 -> 2
        assert_eq!(s.focused_idx, 2);
        s.move_down(3); // 2 -> 0
        assert_eq!(s.focused_idx, 0);
        s.move_down(3); // 0 -> 1
        assert_eq!(s.focused_idx, 1);
    }

    #[test]
    fn move_recovers_from_stale_out_of_range_index() {
        // A provider fetch shrank the list under a focus that was valid before
        // (only set_query resets focus, so this index is genuinely stale).
        let mut s = CommandPaletteState::new();
        s.focused_idx = 9;
        s.move_up(3); // 9 is out of range for len 3 -> wrap to last (2)
        assert_eq!(s.focused_idx, 2);

        s.focused_idx = 9;
        s.move_down(3); // out of range -> wrap to top (0)
        assert_eq!(s.focused_idx, 0);

        // usize::MAX must not overflow move_down's increment.
        s.focused_idx = usize::MAX;
        s.move_down(3);
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn move_on_empty_list_stays_zero() {
        let mut s = CommandPaletteState::new();
        s.focused_idx = 4;
        s.move_up(0);
        assert_eq!(s.focused_idx, 0);
        s.move_down(0);
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn root_visible_rows_are_catalog_then_providers() {
        let s = CommandPaletteState::new();
        let (rs, sf, mcp, arch) = dyn_rows();
        let rows = s.visible_rows(&rs, &sf, &mcp, &arch);
        // Static catalog rows come first (all of them for an empty query)…
        assert!(matches!(rows[0], SelRow::Catalog(_)));
        // …then the four provider rows in fixed order.
        let dyns: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                SelRow::Dyn { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            dyns,
            vec![
                ProviderKind::RecentSessions,
                ProviderKind::SettingsFields,
                ProviderKind::McpServers,
                ProviderKind::ArchivedChats,
            ]
        );
    }

    #[test]
    fn query_filters_root_rows() {
        let mut s = CommandPaletteState::new();
        s.set_query("gamma");
        let (rs, sf, mcp, arch) = dyn_rows();
        let rows = s.visible_rows(&rs, &sf, &mcp, &arch);
        // Only the MCP provider row matches "gamma"; no catalog row does.
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            SelRow::Dyn {
                kind: ProviderKind::McpServers,
                ..
            }
        ));
    }

    #[test]
    fn enter_color_mode_drills_into_sub_page() {
        let mut s = CommandPaletteState::new();
        s.set_query("color mode");
        let (rs, sf, mcp, arch) = dyn_rows();
        let rows = s.visible_rows(&rs, &sf, &mcp, &arch);
        // The single match is the color-mode catalog row.
        assert_eq!(rows.len(), 1);
        assert!(rows[0].opens_sub_page());
        let action = s.enter(&rows);
        assert_eq!(action, PaletteAction::OpenedSubPage);
        assert_eq!(s.page, Page::ColorMode);
        assert_eq!(s.query, "");
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn color_mode_sub_page_lists_and_selects() {
        let mut s = CommandPaletteState::new();
        s.page = Page::ColorMode;
        let rows = s.visible_rows(&[], &[], &[], &[]);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], SelRow::ColorMode(ColorModeChoice::Light)));
        // Select "Dark".
        s.focused_idx = 1;
        let action = s.enter(&rows);
        assert_eq!(action, PaletteAction::SetColorMode(ColorModeChoice::Dark));
    }

    #[test]
    fn color_mode_sub_page_is_filterable() {
        let mut s = CommandPaletteState::new();
        s.page = Page::ColorMode;
        s.query = "system".into();
        let rows = s.visible_rows(&[], &[], &[], &[]);
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            SelRow::ColorMode(ColorModeChoice::System)
        ));
    }

    #[test]
    fn enter_catalog_row_runs_action() {
        let mut s = CommandPaletteState::new();
        s.set_query("feed");
        let rows = s.visible_rows(&[], &[], &[], &[]);
        assert_eq!(rows.len(), 1);
        let action = s.enter(&rows);
        assert_eq!(action, PaletteAction::RunCatalog("goto.feed"));
        // Firing an action does NOT itself change the page (the caller closes).
        assert_eq!(s.page, Page::Root);
    }

    #[test]
    fn enter_dynamic_row_runs_with_kind_and_id() {
        let mut s = CommandPaletteState::new();
        s.set_query("gamma");
        let (rs, sf, mcp, arch) = dyn_rows();
        let rows = s.visible_rows(&rs, &sf, &mcp, &arch);
        let action = s.enter(&rows);
        assert_eq!(
            action,
            PaletteAction::RunDynamic(ProviderKind::McpServers, "m1".to_string())
        );
    }

    #[test]
    fn enter_empty_list_is_none() {
        let mut s = CommandPaletteState::new();
        s.set_query("no-such-zzz-row");
        let rows = s.visible_rows(&[], &[], &[], &[]);
        assert!(rows.is_empty());
        assert_eq!(s.enter(&rows), PaletteAction::None);
    }

    #[test]
    fn back_pops_sub_page_then_closes() {
        let mut s = CommandPaletteState::new();
        s.open();
        s.page = Page::ColorMode;
        s.query = "x".into();
        s.focused_idx = 2;
        // First back: sub-page -> root, palette stays open.
        let closed = s.back();
        assert!(!closed);
        assert_eq!(s.page, Page::Root);
        assert_eq!(s.query, "");
        assert_eq!(s.focused_idx, 0);
        assert!(s.open);
        // Second back: root -> closed.
        let closed = s.back();
        assert!(closed);
        assert!(!s.open);
    }

    #[test]
    fn keybind_hint_surfaces_from_catalog() {
        let s = CommandPaletteState::new();
        let rows = s.visible_rows(&[], &[], &[], &[]);
        let settings = rows
            .iter()
            .find(|r| matches!(r, SelRow::Catalog(e) if e.id == "goto.settings"))
            .unwrap();
        assert_eq!(settings.keybind(), Some("Cmd+,"));
        // Dynamic rows never carry a keybind.
        assert_eq!(
            SelRow::Dyn {
                kind: ProviderKind::McpServers,
                entry: DynEntry::new("m", "m")
            }
            .keybind(),
            None
        );
    }

    #[test]
    fn render_is_none_when_closed_some_when_open() {
        let mut s = CommandPaletteState::new();
        assert!(command_palette(
            &s,
            PaletteSources {
                recent_sessions: &[],
                settings_fields: &[],
                mcp_servers: &[],
                archived_chats: &[],
            },
            test_row_listener,
            test_close(),
            test_query()
        )
        .is_none());
        s.open();
        assert!(command_palette(
            &s,
            PaletteSources {
                recent_sessions: &[],
                settings_fields: &[],
                mcp_servers: &[],
                archived_chats: &[],
            },
            test_row_listener,
            test_close(),
            test_query()
        )
        .is_some());
    }

    #[test]
    fn query_key_printable_appends() {
        assert_eq!(query_key("a", false), QueryKey::Append("a".to_string()));
        assert_eq!(query_key("z", false), QueryKey::Append("z".to_string()));
        assert_eq!(query_key("space", false), QueryKey::Append(" ".to_string()));
    }

    #[test]
    fn query_key_backspace_and_ignores() {
        assert_eq!(query_key("backspace", false), QueryKey::Backspace);
        assert_eq!(query_key("delete", false), QueryKey::Backspace);
        // Arrows / named keys are not text edits.
        assert_eq!(query_key("up", false), QueryKey::Ignore);
        assert_eq!(query_key("enter", false), QueryKey::Ignore);
        // A chord (⌘V etc.) is never a query edit.
        assert_eq!(query_key("a", true), QueryKey::Ignore);
    }

    #[test]
    fn push_and_backspace_edit_query() {
        let mut s = CommandPaletteState::new();
        s.open();
        s.push_query("s");
        s.push_query("e");
        assert_eq!(s.query, "se");
        s.backspace();
        assert_eq!(s.query, "s");
        // Backspace on empty is a no-op, never a panic.
        s.backspace();
        s.backspace();
        assert_eq!(s.query, "");
    }

    #[test]
    fn catalog_nav_maps_every_static_id() {
        // Every real navigation surface resolves; nothing panics.
        assert_eq!(
            catalog_nav("goto.new_conversation"),
            PaletteNav::NewConversation
        );
        assert_eq!(catalog_nav("goto.feed"), PaletteNav::OpenFeed);
        assert_eq!(
            catalog_nav("goto.command_center"),
            PaletteNav::GoCommandCenter
        );
        assert_eq!(catalog_nav("cc.sessions"), PaletteNav::GoCommandCenter);
        assert_eq!(catalog_nav("cc.system"), PaletteNav::GoCommandCenter);
        assert_eq!(catalog_nav("cc.usage"), PaletteNav::GoCommandCenter);
        assert_eq!(catalog_nav("goto.skills"), PaletteNav::GoSkills);
        assert_eq!(catalog_nav("goto.messaging"), PaletteNav::GoMessaging);
        assert_eq!(catalog_nav("goto.settings"), PaletteNav::OpenSettings);
        assert_eq!(catalog_nav("settings.model"), PaletteNav::OpenSettings);
        assert_eq!(catalog_nav("settings.messaging"), PaletteNav::OpenSettings);
        // Appearance routes to its Settings page.
        assert_eq!(catalog_nav("appearance.theme"), PaletteNav::OpenAppearance);
        assert_eq!(
            catalog_nav("appearance.color_mode"),
            PaletteNav::OpenAppearance
        );
        assert_eq!(catalog_nav("nope.unknown"), PaletteNav::Noop);
    }

    #[test]
    fn dynamic_nav_routes_each_provider_kind_to_its_window_command() {
        // Each provider kind opens a distinct surface, and the opaque id flows
        // through unchanged to the two kinds that carry one (so the window opens
        // exactly the picked item, not the wrong one).
        assert_eq!(
            dynamic_nav(ProviderKind::RecentSessions, "sess-42".to_string()),
            DynNav::OpenSession("sess-42".to_string())
        );
        assert_eq!(
            dynamic_nav(ProviderKind::ArchivedChats, "conv-9".to_string()),
            DynNav::OpenArchived("conv-9".to_string())
        );
        // SettingsFields ships no live rows yet but routes to Settings when wired.
        assert_eq!(
            dynamic_nav(ProviderKind::SettingsFields, "x".to_string()),
            DynNav::OpenSettings
        );
        // McpServers is sourceless → clean close.
        assert_eq!(
            dynamic_nav(ProviderKind::McpServers, "m".to_string()),
            DynNav::Noop
        );
    }

    #[test]
    fn every_catalog_row_navigates_or_drills() {
        // No static catalog row is dead: it either drills into a sub-page
        // (color mode) or maps to a concrete PaletteNav (Noop counts — it is a
        // deliberate, documented close-with-no-effect for Appearance rows).
        for e in crate::command_catalog::CATALOG {
            if e.id == COLOR_MODE_ID {
                continue; // opens the color-mode sub-page instead of a nav.
            }
            // catalog_nav is total; this asserts it never falls through by
            // accident for a real catalog id we forgot to list.
            let nav = catalog_nav(e.id);
            let is_appearance = matches!(e.group, CatalogGroup::Appearance);
            if is_appearance {
                assert_eq!(
                    nav,
                    PaletteNav::OpenAppearance,
                    "appearance row {} opens its Settings page",
                    e.id
                );
            } else {
                assert_ne!(
                    nav,
                    PaletteNav::Noop,
                    "non-appearance row {} must navigate",
                    e.id
                );
            }
        }
    }

    /// What the rendered palette reported back to the window.
    #[derive(Default)]
    struct Reports {
        chosen: RefCell<Vec<usize>>,
        closes: Cell<u32>,
        queries: RefCell<Vec<String>>,
    }

    /// Render the palette over `state` in a tall window (every row is on
    /// screen, so each can be hit). The row, close and query listeners report
    /// into `reports`; a query edit also lands in `state`, as the window does.
    fn open_palette(
        cx: &mut TestAppContext,
        state: Rc<RefCell<CommandPaletteState>>,
        reports: Rc<Reports>,
    ) -> &mut VisualTestContext {
        let (_handle, wcx) =
            render_part_at(cx, gpui::size(gpui::px(900.), gpui::px(2400.)), move || {
                let s = state.borrow().clone();
                let (rs, sf, mcp, arch) = dyn_rows();
                let (state_q, reports_r, reports_c, reports_q) = (
                    state.clone(),
                    reports.clone(),
                    reports.clone(),
                    reports.clone(),
                );
                div().size_full().children(command_palette(
                    &s,
                    PaletteSources {
                        recent_sessions: &rs,
                        settings_fields: &sf,
                        mcp_servers: &mcp,
                        archived_chats: &arch,
                    },
                    move |i: usize| -> ClickFn {
                        let reports = reports_r.clone();
                        Box::new(
                            move |_e: &gpui::ClickEvent,
                                  _w: &mut gpui::Window,
                                  _c: &mut gpui::App| {
                                reports.chosen.borrow_mut().push(i);
                            },
                        )
                    },
                    Box::new(move |_w: &mut gpui::Window, _c: &mut gpui::App| {
                        reports_c.closes.set(reports_c.closes.get() + 1);
                    }),
                    Box::new(
                        move |q: String, _w: &mut gpui::Window, _c: &mut gpui::App| {
                            state_q.borrow_mut().set_query(&q);
                            reports_q.queries.borrow_mut().push(q);
                        },
                    ),
                ))
            });
        wcx
    }

    /// A palette opened at the root with `query` typed in.
    fn open_state(query: &str) -> Rc<RefCell<CommandPaletteState>> {
        let mut s = CommandPaletteState::new();
        s.open();
        s.set_query(query);
        Rc::new(RefCell::new(s))
    }

    fn row_selector(i: usize) -> String {
        format!("ui-palette-row-{PALETTE_ID}-row-{i}")
    }

    fn row_bounds(wcx: &mut VisualTestContext, i: usize) -> gpui::Bounds<gpui::Pixels> {
        let key: &'static str = Box::leak(row_selector(i).into_boxed_str());
        wcx.debug_bounds(key)
            .unwrap_or_else(|| panic!("row {i} recorded no bounds"))
    }

    /// A real left click at the centre of rendered row `i`.
    fn click_row(wcx: &mut VisualTestContext, i: usize) {
        let at = row_bounds(wcx, i).center();
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

    /// Simulate typed keys (the window's focused palette) and let them land.
    fn type_keys(wcx: &mut VisualTestContext, keys: &str) {
        wcx.simulate_keystrokes(keys);
        wcx.run_until_parked();
    }

    /// The rendered rows are the reducer's rows, in the reducer's order, one
    /// tooltip (the full label) per row; and a group boundary is a taller step
    /// between two rows than a step inside a group (its section title sits
    /// between them).
    #[gpui::test]
    fn rendered_rows_follow_reducer_order_and_group_boundaries(cx: &mut TestAppContext) {
        let state = open_state("");
        let wcx = open_palette(cx, state.clone(), Rc::new(Reports::default()));
        wcx.run_until_parked();
        let (rs, sf, mcp, arch) = dyn_rows();
        let expected = state.borrow().visible_rows(&rs, &sf, &mcp, &arch);
        let n = expected.len();
        assert!(n > 4, "the root lists the catalog and the providers");
        for (i, row) in expected.iter().enumerate() {
            assert_eq!(
                tooltip_at(wcx, &row_selector(i)).as_deref(),
                Some(row.label()),
                "rendered row {i} is the reducer's row {i}"
            );
        }

        let sections: Vec<&str> = expected.iter().map(row_section).collect();
        let tops: Vec<f32> = (0..n)
            .map(|i| f32::from(row_bounds(wcx, i).origin.y))
            .collect();
        let within = (0..n - 1)
            .find(|&i| sections[i] == sections[i + 1])
            .expect("a group holds more than one row");
        let step = tops[within + 1] - tops[within];
        assert!(step > 0.0, "rows stack downwards");
        for i in 0..n - 1 {
            let gap = tops[i + 1] - tops[i];
            if sections[i] == sections[i + 1] {
                assert!(
                    (gap - step).abs() < 0.5,
                    "rows inside one group sit one step apart: row {i}"
                );
            } else {
                assert!(
                    gap > step + 1.0,
                    "a new group opens a section title: row {i}"
                );
            }
        }
        assert!(
            sections.windows(2).any(|w| w[0] != w[1]),
            "the rows span more than one group"
        );
    }

    /// The focused row is the one the palette highlights: its keybind hint
    /// paints brighter than an unfocused keybind row's.
    #[gpui::test]
    fn rendered_selection_highlights_the_focused_row(cx: &mut TestAppContext) {
        let state = open_state("");
        let (rs, sf, mcp, arch) = dyn_rows();
        let rows = state.borrow().visible_rows(&rs, &sf, &mcp, &arch);
        let sel = rows
            .iter()
            .position(|r| matches!(r, SelRow::Catalog(e) if e.id == "goto.settings"))
            .expect("the settings row is in the root catalog");
        let other = rows
            .iter()
            .enumerate()
            .position(|(i, r)| i != sel && r.keybind().is_some())
            .expect("another row carries a keybind");
        state.borrow_mut().focused_idx = sel;

        let wcx = open_palette(cx, state.clone(), Rc::new(Reports::default()));
        wcx.run_until_parked();
        let shortcut = |i: usize| {
            child_id(
                &child_id(&ElementId::from(PALETTE_ID), &format!("row-{i}")),
                "shortcut",
            )
        };
        let p = crate::theme::palette();
        assert!(
            same_color(painted(&shortcut(sel)).unwrap(), p.text_2),
            "the focused row's hint lifts"
        );
        assert!(
            same_color(painted(&shortcut(other)).unwrap(), p.text_dim),
            "an unfocused row's hint rests"
        );
    }

    /// Typed characters and Backspace report each new query through the query
    /// callback; the window stores it, and the rows follow it.
    #[gpui::test]
    fn rendered_query_edits_report_through_the_query_callback(cx: &mut TestAppContext) {
        let state = open_state("");
        let reports = Rc::new(Reports::default());
        let wcx = open_palette(cx, state.clone(), reports.clone());
        wcx.run_until_parked();

        type_keys(wcx, "g a m m a");
        type_keys(wcx, "backspace");
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();

        assert_eq!(
            reports.queries.borrow().as_slice(),
            ["g", "ga", "gam", "gamm", "gamma", "gamm"],
            "each typed character and Backspace reports the new query"
        );
        assert_eq!(state.borrow().query, "gamm", "the window stored the query");
        let (rs, sf, mcp, arch) = dyn_rows();
        let expected = state.borrow().visible_rows(&rs, &sf, &mcp, &arch);
        assert_eq!(expected.len(), 1, "only the MCP row matches 'gamm'");
        assert_eq!(
            tooltip_at(wcx, &row_selector(0)).as_deref(),
            Some("mcp server gamma"),
            "the rendered list follows the stored query"
        );
    }

    /// The color-mode drill-in keeps the palette open when chosen by mouse or
    /// by keyboard, and rows on the Color Mode page still close it first.
    #[gpui::test]
    fn drill_in_keeps_the_palette_open_and_color_mode_rows_close(cx: &mut TestAppContext) {
        let state = open_state("color mode");
        let reports = Rc::new(Reports::default());
        let wcx = open_palette(cx, state.clone(), reports.clone());
        wcx.run_until_parked();

        click_row(wcx, 0);
        assert_eq!(
            reports.chosen.borrow().as_slice(),
            [0usize],
            "the drill-in row runs its action on a click"
        );
        assert_eq!(
            reports.closes.get(),
            0,
            "a click on the drill-in keeps the palette open"
        );

        type_keys(wcx, "enter");
        assert_eq!(
            reports.chosen.borrow().as_slice(),
            [0usize, 0usize],
            "Enter on the drill-in runs its action"
        );
        assert_eq!(
            reports.closes.get(),
            0,
            "Enter on the drill-in keeps the palette open"
        );

        {
            let mut s = state.borrow_mut();
            s.page = Page::ColorMode;
            s.set_query("");
        }
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        click_row(wcx, 1);
        assert_eq!(
            reports.closes.get(),
            1,
            "a row on the Color Mode page closes the palette before its action"
        );
    }
}
