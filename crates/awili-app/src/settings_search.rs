//! SXd: Settings search, typed values and Text size (design:
//! the settings design notes). The search box sits on
//! top of the Settings sidebar and filters live; Enter opens the first hit's
//! page, Escape clears the query before it closes Settings. Text and key
//! settings get a big Change button that points the existing inline editor
//! (`settings_field`) at the item; Return saves through live apply.
use super::*;
use crate::settings_catalog::{self as cat, Control, Edit, Item, Page};

impl AwiliApp {
    /// The trimmed search query ("" when not searching).
    pub(crate) fn settings_query(&self, _cx: &App) -> String {
        self.settings_search_query.trim().to_string()
    }

    pub(crate) fn clear_settings_search(&mut self, cx: &mut Context<Self>) {
        self.settings_search_query.clear();
        cx.notify();
    }

    /// Enter in the search box: open the first hit's page, clear the query.
    pub(crate) fn open_first_settings_hit(&mut self, cx: &mut Context<Self>) {
        let query = self.settings_query(cx);
        if let Some(hit) = cat::search(&query).first() {
            let page = cat::home_page(hit);
            self.clear_settings_search(cx);
            self.select_settings_page(page, cx);
        }
    }

    /// Point the inline editor at a text or key setting and focus it. A key
    /// starts empty: its value is never put on screen.
    pub(crate) fn begin_item_edit(
        &mut self,
        item: &'static Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match (item.control, self.settings.doc.as_ref()) {
            (Control::Text, Some(doc)) => cat::current(doc, item)
                .as_str()
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        };
        self.settings.editing = None;
        self.settings.field = None;
        self.settings.editing_item = Some(item.id);
        self.settings.error = None;
        self.settings_field
            .update(cx, |c, cx| c.set_text(&text, cx));
        let h = self.settings_field.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Return in the inline editor while it edits a catalog item.
    pub(crate) fn commit_item_edit(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.settings.editing_item.take() else {
            return;
        };
        // Return can reach the buffer as a newline before Submit; a key or
        // an address never holds one (a key's spaces are kept).
        let text = self
            .settings_field
            .read(cx)
            .text()
            .trim_matches(|c| c == '\n' || c == '\r')
            .to_string();
        // A key never stays in the editor after it was used.
        self.settings_field.update(cx, |c, cx| c.set_text("", cx));
        self.apply_settings_item(id, Edit::Text(text), cx);
        // SXd-FIX1: the editor leaves the screen; the page takes focus back
        // so the next Escape still reaches Settings.
        self.settings_nav_focus_pending = true;
    }

    /// The search box (top of the sidebar).
    pub(crate) fn settings_search_box(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let search_owner = cx.entity().downgrade();
        ui::SearchField::new("settings-search", "Search settings")
            .selector("sx-search")
            .focus_handle(&self.settings_search_focus)
            .query(self.settings_search_query.clone())
            .on_query(move |query, _window, cx| {
                let _ = search_owner.update(cx, |this, cx| {
                    this.settings_search_query = query;
                    cx.notify();
                });
            })
            .into_any_element()
    }

    /// How many hits of `query` a page lists (the sidebar count).
    pub(crate) fn settings_hits_on(&self, page: Page, query: &str) -> usize {
        cat::search(query)
            .iter()
            .filter(|i| cat::home_page(i) == page || (page == Page::General && i.common))
            .count()
    }

    /// The results page shown while the query is not empty.
    pub(crate) fn settings_search_results(
        &self,
        query: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let hits = cat::search(query);
        let mut rows: Vec<gpui::AnyElement> = vec![div()
            .px(theme::z(theme::SP_16))
            .pt(theme::z(theme::SP_12))
            .child(ui::text::ui_text(format!("Results for “{query}”")).strong())
            .into_any_element()];
        if hits.is_empty() {
            rows.push(
                ui::EmptyState::new(
                    "sx-no-hits",
                    format!("No setting matches “{query}”"),
                    "Try other words, or open Advanced, then Show every setting.",
                )
                .selector("sx-no-hits")
                .top(theme::z(theme::SP_16))
                .into_any_element(),
            );
        }
        if let Some(doc) = self.settings.doc.clone() {
            for item in hits {
                let tag = match item.advanced {
                    true => format!("Advanced · {}", item.page.title()),
                    false => item.page.title().to_string(),
                };
                rows.push(
                    div()
                        .px(theme::z(theme::SP_20))
                        .pt(theme::z(theme::SP_12))
                        .child(
                            ui::text::label(tag)
                                .strong()
                                .text_color_override(theme::p().accent_text),
                        )
                        .into_any_element(),
                );
                rows.push(self.settings_item_row(&doc, item, cx));
            }
        }
        div()
            .id("sx-results-scroll")
            .track_focus(&self.settings_nav_focus)
            .flex_1()
            .min_h(gpui::Pixels::ZERO)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(theme::z(theme::SP_16))
            .children(rows)
            .into_any_element()
    }

    /// General's first row: the interface zoom (kept in `ui_scale`, not in
    /// config.toml, so it is not a catalog item).
    /// RC2-UX (R2-21, round 2 Minor 11: a text size change left "Last change:
    /// Chat assistant" on the line): Settings' text size buttons change the
    /// size and the line says so.
    pub(crate) fn settings_text_size(
        &mut self,
        scale: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_ui_scale(scale, window, cx);
        let pct = (theme::ui_scale() * 100.0).round() as i32;
        self.settings.error = None;
        self.settings.saved_here = None;
        self.settings.status = Some(format!("Text size: {pct}%"));
        cx.notify();
    }

    pub(crate) fn settings_text_size_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let pct = (theme::ui_scale() * 100.0).round() as i32;
        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .child(
                self.sx_button_w(("sx-zoom-out", 0), "−", false, cx, |t, w, cx| {
                    t.settings_text_size(zoom::step_out(theme::ui_scale()), w, cx)
                }),
            )
            .child(
                div()
                    .min_w(theme::z(theme::SETTINGS_ZOOM_VALUE_W))
                    .flex()
                    .justify_center()
                    .child(ui::text::ui_text(format!("{pct}%"))),
            )
            .child(
                self.sx_button_w(("sx-zoom-in", 0), "+", false, cx, |t, w, cx| {
                    t.settings_text_size(zoom::step_in(theme::ui_scale()), w, cx)
                }),
            )
            .child(self.sx_button_w(
                ("sx-zoom-reset", 0),
                "Actual size (100%)",
                false,
                cx,
                |t, w, cx| t.settings_text_size(zoom::ACTUAL, w, cx),
            ));
        ui::SettingRow::new("sx-row-text-size", "Text size")
            .selector("sx-row-text-size")
            .help("Makes all of Awili larger or smaller. View → Zoom does the same.")
            .control(move |_| controls)
            .into_any_element()
    }

    /// A text or key row's control: the value and a big Change button, or
    /// the inline editor while that item is being edited.
    pub(crate) fn settings_text_control(
        &self,
        item: &'static Item,
        ix: usize,
        shown: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.settings.editing_item == Some(item.id) {
            let hint = match item.control {
                Control::Secret => "Paste the new key, then press Return. Esc cancels.",
                _ => "Press Return to save. Esc cancels.",
            };
            let editor = self.settings_field.clone();
            return div()
                .w(theme::z(theme::SETTINGS_EDITOR_W))
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .child(
                    ui::TextField::new("sx-inline-editor", "")
                        .selector("sx-inline-editor")
                        .pointer_only()
                        .editor(move |_| editor),
                )
                .child(ui::text::label(hint).dim())
                .into_any_element();
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .child(
                div()
                    .debug_selector(move || format!("sx-value-{ix}"))
                    .max_w(theme::z(theme::SETTINGS_VALUE_MAX_W))
                    .overflow_hidden()
                    .flex_1()
                    .min_w(gpui::Pixels::ZERO)
                    .child(
                        ui::text::ui_text(shown)
                            .dim()
                            .id(format!("sx-value-text-{ix}")),
                    ),
            )
            .child(
                self.sx_button_w(("sx-edit", ix), "Change", false, cx, move |t, w, cx| {
                    t.begin_item_edit(item, w, cx)
                }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "settings_search_tests.rs"]
mod tests;
