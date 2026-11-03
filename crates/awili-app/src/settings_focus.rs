//! SXi: the Settings keyboard (design: the settings design notes).
//! Two regions, the sidebar and the page, plus the search box (⌘F or `/`).
//! In the sidebar, ↑/↓ open the previous/next page at once and Home/End go
//! to the first and last page. Tab moves into the page and walks every control in
//! reading order; Shift-Tab walks back, and both leave the page at its ends.
//! ←/→ move between the buttons of one row. Enter or Space presses the
//! focused button. The order comes from the catalog ([`order`]), never from
//! row indices. Models & Fleet keeps `settings_nav` for its provider form.
use super::*;
use crate::settings_catalog::{self as cat, Control, Edit, Item, Page};

/// One keyboard stop: a button on a catalog row, or the Undo button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusStop {
    Choice(&'static str, usize),
    On(&'static str),
    Off(&'static str),
    Dec(&'static str),
    Inc(&'static str),
    Edit(&'static str),
    Reset(&'static str),
    Undo,
    ErrorDetails,
    /// S6: Revoke on row `n` of the Allowed actions page.
    Revoke(usize),
}

fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap_or(0)
}

impl FocusStop {
    /// The catalog item this stop belongs to (`None` for Undo).
    pub(crate) fn item(self) -> Option<&'static str> {
        match self {
            FocusStop::Choice(id, _)
            | FocusStop::On(id)
            | FocusStop::Off(id)
            | FocusStop::Dec(id)
            | FocusStop::Inc(id)
            | FocusStop::Edit(id)
            | FocusStop::Reset(id) => Some(id),
            FocusStop::Undo | FocusStop::ErrorDetails | FocusStop::Revoke(_) => None,
        }
    }

    /// The button this stop is, as `sx_button` names it.
    pub(crate) fn button(self) -> (&'static str, usize) {
        match self {
            FocusStop::Choice(id, k) => ("sx-choice", ix(id) * 16 + k),
            FocusStop::On(id) => ("sx-on", ix(id)),
            FocusStop::Off(id) => ("sx-off", ix(id)),
            FocusStop::Dec(id) => ("sx-dec", ix(id)),
            FocusStop::Inc(id) => ("sx-inc", ix(id)),
            FocusStop::Edit(id) => ("sx-edit", ix(id)),
            FocusStop::Reset(id) => ("sx-reset", ix(id)),
            FocusStop::Undo => ("sx-undo", 0),
            FocusStop::ErrorDetails => ("sx-error-details", 0),
            FocusStop::Revoke(n) => ("sx-revoke", n),
        }
    }
}

/// Every stop of `page` in reading order: each row's buttons left to right,
/// then its Reset when the row is changed; Undo last when there is one.
pub(crate) fn order(page: Page, doc: &awili_config::ConfigDoc, undo: bool) -> Vec<FocusStop> {
    let mut stops = Vec::new();
    for item in cat::items_on(page, false) {
        let id = item.id;
        match item.control {
            Control::Toggle => stops.extend([FocusStop::On(id), FocusStop::Off(id)]),
            Control::Choice(options) => {
                stops.extend((0..options.len()).map(|k| FocusStop::Choice(id, k)))
            }
            Control::Stepper { .. } | Control::Level { .. } => {
                stops.extend([FocusStop::Dec(id), FocusStop::Inc(id)])
            }
            Control::Text | Control::Secret => stops.push(FocusStop::Edit(id)),
        }
        if cat::is_modified(doc, item) {
            stops.push(FocusStop::Reset(id));
        }
    }
    if undo {
        stops.push(FocusStop::Undo);
    }
    stops
}

/// What a key does in Settings (the pure part of [`AwiliApp::settings_key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyMove {
    Search,
    PrevPage,
    NextPage,
    FirstPage,
    LastPage,
    Next,
    Prev,
    Left,
    Right,
    Press,
}

/// The move for a keystroke, or `None` when Settings leaves the key alone.
pub(crate) fn key_move(ks: &gpui::Keystroke) -> Option<KeyMove> {
    let m = &ks.modifiers;
    if crate::composer_keys::main_modifier_only(m) && !m.alt && !m.shift && ks.key == "f" {
        return Some(KeyMove::Search);
    }
    if m.platform || m.control || m.alt || m.function {
        return None;
    }
    match (ks.key.as_str(), m.shift) {
        ("/", false) => Some(KeyMove::Search),
        ("tab", false) => Some(KeyMove::Next),
        ("tab", true) => Some(KeyMove::Prev),
        ("up", false) => Some(KeyMove::PrevPage),
        ("down", false) => Some(KeyMove::NextPage),
        ("home", false) => Some(KeyMove::FirstPage),
        ("end", false) => Some(KeyMove::LastPage),
        ("left", false) => Some(KeyMove::Left),
        ("right", false) => Some(KeyMove::Right),
        ("enter" | "space", false) => Some(KeyMove::Press),
        _ => None,
    }
}

impl AwiliApp {
    /// The stops of the page on screen.
    fn settings_stops(&self) -> Vec<FocusStop> {
        if self.settings.page == Page::Allowed {
            return (0..self.allowed_actions.grants.len())
                .map(FocusStop::Revoke)
                .collect();
        }
        let mut stops = self
            .settings
            .doc
            .as_ref()
            .map(|doc| {
                order(
                    self.settings.page,
                    doc,
                    self.settings.last_change().is_some(),
                )
            })
            .unwrap_or_default();
        if self.settings.error.is_some() {
            stops.push(FocusStop::ErrorDetails);
        }
        stops
    }

    /// Handle one key for the Settings pages. `true` when it was used.
    pub(crate) fn settings_key(
        &mut self,
        ks: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mv) = key_move(ks) else {
            return false;
        };
        if mv == KeyMove::Search {
            let h = self.settings_search_focus.clone();
            window.focus(&h, cx);
            cx.notify();
            return true;
        }
        let stops = self.settings_stops();
        let at = self
            .settings_stop
            .and_then(|s| stops.iter().position(|x| *x == s));
        let pages = Page::ALL;
        let page_at = pages
            .iter()
            .position(|p| *p == self.settings.page)
            .unwrap_or(0);
        if self.settings_in_sidebar {
            let go = match mv {
                KeyMove::PrevPage => Some(page_at.saturating_sub(1)),
                KeyMove::NextPage => Some((page_at + 1).min(pages.len() - 1)),
                KeyMove::FirstPage => Some(0),
                KeyMove::LastPage => Some(pages.len() - 1),
                _ => None,
            };
            if let Some(i) = go {
                self.select_settings_page(pages[i], cx);
                return true;
            }
            if matches!(mv, KeyMove::Next | KeyMove::Press) {
                self.settings_in_sidebar = false;
                if self.settings.page == Page::Models {
                    // Models & Fleet's form keeps its own keyboard model: Tab
                    // goes on to it; Enter only enters the page (it must not
                    // run the form's selected action).
                    cx.notify();
                    return mv == KeyMove::Press;
                }
                // Into the page, at its first stop.
                self.focus_settings_stop(stops.first().copied(), cx);
                return true;
            }
            if mv == KeyMove::Prev {
                // Out of the sidebar, up to the search box.
                let h = self.settings_search_focus.clone();
                window.focus(&h, cx);
                cx.notify();
                return true;
            }
            return false;
        }
        match mv {
            KeyMove::Next => match at {
                Some(i) if i + 1 < stops.len() => self.focus_settings_stop(Some(stops[i + 1]), cx),
                Some(_) => self.focus_settings_sidebar(cx),
                None => self.focus_settings_stop(stops.first().copied(), cx),
            },
            KeyMove::Prev => match at {
                Some(i) if i > 0 => self.focus_settings_stop(Some(stops[i - 1]), cx),
                _ => self.focus_settings_sidebar(cx),
            },
            KeyMove::Left | KeyMove::Right => {
                let Some(i) = at else { return true };
                let j = if mv == KeyMove::Left {
                    i.checked_sub(1)
                } else {
                    Some(i + 1)
                };
                if let Some(next) = j.and_then(|j| stops.get(j)) {
                    if next.item().is_some() && next.item() == stops[i].item() {
                        self.focus_settings_stop(Some(*next), cx);
                    }
                }
            }
            KeyMove::Press => {
                if let Some(stop) = self.settings_stop {
                    self.press_settings_stop(stop, window, cx);
                }
            }
            // Up/Down/Home/End scroll the page as usual.
            _ => return false,
        }
        true
    }

    fn focus_settings_sidebar(&mut self, cx: &mut Context<Self>) {
        self.settings_in_sidebar = true;
        self.settings_stop = None;
        cx.notify();
    }

    /// Focus `stop` and scroll its row into view.
    fn focus_settings_stop(&mut self, stop: Option<FocusStop>, cx: &mut Context<Self>) {
        self.settings_stop = stop;
        let row = stop.and_then(FocusStop::item).and_then(|id| {
            self.settings_page_rows
                .borrow()
                .iter()
                .position(|r| *r == Some(id))
        });
        if let Some(ix) = row {
            self.settings_page_scroll.scroll_to_item(ix);
        }
        cx.notify();
    }

    /// Press the button `stop` is, exactly as a click would.
    fn press_settings_stop(
        &mut self,
        stop: FocusStop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item: Option<&'static Item> = stop.item().and_then(cat::find);
        let now = match (item, self.settings.doc.as_ref()) {
            (Some(item), Some(doc)) => cat::current(doc, item),
            _ => serde_json::Value::Null,
        };
        match (stop, item) {
            (FocusStop::On(id), _) => self.apply_settings_item(id, Edit::Bool(true), cx),
            (FocusStop::Off(id), _) => self.apply_settings_item(id, Edit::Bool(false), cx),
            (FocusStop::Choice(_, k), Some(item)) => {
                if let Control::Choice(options) = item.control {
                    if let Some(&(stored, _)) = options.get(k) {
                        self.press_settings_choice(item, stored, cx);
                    }
                }
            }
            (FocusStop::Dec(id), Some(item)) | (FocusStop::Inc(id), Some(item)) => {
                let up = matches!(stop, FocusStop::Inc(_));
                if let Some(e) = cat::step(item, &now, up) {
                    self.apply_settings_item(id, e, cx);
                }
            }
            (FocusStop::Edit(_), Some(item)) => self.begin_item_edit(item, window, cx),
            (FocusStop::Reset(_), Some(item)) => self.press_settings_reset(item, cx),
            (FocusStop::Undo, _) => self.undo_settings_change(cx),
            (FocusStop::ErrorDetails, _) => self.open_settings_error_details(window, cx),
            (FocusStop::Revoke(n), _) => self.revoke_allowed_action(n, cx),
            _ => {}
        }
        // A press that changed the page (a Reset or Undo) may drop the stop.
        // RC2-UX (R2-21, round 2 Minor 12): the keyboard stays on that row
        // (its first button), not the page's first control.
        let stops = self.settings_stops();
        if !stops.contains(&stop) {
            self.settings_stop = stops
                .iter()
                .find(|s| s.item().is_some() && s.item() == stop.item())
                .or(stops.first())
                .copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sxi_order_follows_rows_left_to_right() {
        let doc = awili_config::ConfigDoc::parse("").unwrap();
        let ptt = cat::find("voice.ptt_key").unwrap();
        let Control::Choice(keys) = ptt.control else {
            panic!("a choice")
        };
        let mut want = vec![
            FocusStop::On("voice.auto_speak"),
            FocusStop::Off("voice.auto_speak"),
        ];
        want.extend((0..keys.len()).map(|k| FocusStop::Choice("voice.ptt_key", k)));
        want.extend([
            FocusStop::Dec("voice.vad_threshold"),
            FocusStop::Inc("voice.vad_threshold"),
            FocusStop::Dec("voice.vad_silence_windows"),
            FocusStop::Inc("voice.vad_silence_windows"),
        ]);
        assert_eq!(order(Page::Voice, &doc, false), want);
        let general = order(Page::General, &doc, false);
        assert_eq!(
            general.first(),
            Some(&FocusStop::On("memory.recall")),
            "General starts with its first row's first button"
        );
        assert_eq!(
            FocusStop::Choice("voice.ptt_key", 2).button(),
            ("sx-choice", ix("voice.ptt_key") * 16 + 2),
            "a stop is the very button a click would press"
        );
        assert_eq!(FocusStop::Undo.button(), ("sx-undo", 0));
        assert_eq!(
            order(Page::Voice, &doc, true).last(),
            Some(&FocusStop::Undo)
        );
    }

    #[test]
    fn sxi_reset_joins_the_order_only_when_changed() {
        let plain = awili_config::ConfigDoc::parse("").unwrap();
        assert!(!order(Page::Memory, &plain, false).contains(&FocusStop::Reset("memory.recall")));
        let changed = awili_config::ConfigDoc::parse(
            "[memory]\nbase_url = \"http://10.77.68.86:8841\"\nrecall = true\n",
        )
        .unwrap();
        let stops = order(Page::Memory, &changed, false);
        let off = stops
            .iter()
            .position(|s| *s == FocusStop::Off("memory.recall"))
            .unwrap();
        assert_eq!(
            stops.get(off + 1),
            Some(&FocusStop::Reset("memory.recall")),
            "Reset follows its row's buttons"
        );
        assert!(
            !stops.contains(&FocusStop::Reset("memory.write")),
            "an unchanged row has no Reset stop"
        );
    }

    #[test]
    fn sxi_keys_map_to_moves() {
        let k = |s: &str| key_move(&gpui::Keystroke::parse(s).unwrap());
        assert_eq!(k("tab"), Some(KeyMove::Next));
        assert_eq!(k("shift-tab"), Some(KeyMove::Prev));
        assert_eq!(k("secondary-f"), Some(KeyMove::Search));
        assert_eq!(k("/"), Some(KeyMove::Search));
        assert_eq!(k("space"), Some(KeyMove::Press));
        assert_eq!(k("enter"), Some(KeyMove::Press));
        assert_eq!(k("secondary-,"), None, "other chords pass through");
        assert_eq!(k("a"), None);
    }
}
