//! SXc: the Settings pages (design: the settings design notes).
//! A sidebar of task pages and, on each page, the catalog's settings as
//! big rows: a plain label, one line of help, and click-only controls. A
//! click saves at once (`settings_live`); a changed row gets an accent bar
//! and a Reset button; the bar under the page names the last change and
//! offers Undo. Models & Fleet still shows the provider form with its own
//! Save until SXe moves it. SXd adds search, typed values and Text size
//! (`settings_search`).
use super::*;
use crate::settings_catalog::{self as cat, Control, Edit, Item, Page};
use crate::ui::{text, Card, Divider, Row};

/// The smallest click target in Settings (Apple's 44 pt guideline).
#[cfg(test)]
pub(crate) const HIT: f32 = 44.0;

/// SXe-FIX1: the `pending_choice` value of an armed Reset on a guarded row
/// (never a real setting value).
pub(crate) const RESET_ARMED: &str = "(reset)";

/// The catalog index of `item` (its row's element ids and test selectors).
/// RC2-UX (R2-24: "Takes effect next time Awili starts" was unclear): what
/// to do for a setting that needs a restart.
pub(crate) const RESTART_NOTE: &str = "Applies after you quit Awili and open it again";

/// RC2-UX (R2-21): the mark a row shows right after a change lands on it.
pub(crate) const SAVED_HERE: &str = "\u{2713} Saved";
/// RC2-UX (R2-21): the mark a row shows right after its Reset.
pub(crate) const RESET_HERE: &str = "\u{2713} Back to default";
/// RC2-UX (R2-23): what a test notification press says in Awili itself.
///
/// RC2-FIXB4 (RC2-UX review P2: it said "sent" while macOS was still being
/// asked): the words follow what the press (or macOS's later answer) did.
pub(crate) fn test_notice(outcome: crate::notify_hooks::TestOutcome) -> &'static str {
    use crate::notify_hooks::TestOutcome;
    match outcome {
        TestOutcome::Shown => {
            "Test notification sent \u{2014} it shows at the top right of your screen."
        }
        TestOutcome::ShownByScriptEditor => {
            "Test notification sent through Script Editor \u{2014} it shows at the top right of \
             your screen."
        }
        TestOutcome::Asking => {
            "macOS is asking whether Awili may show notifications. Click Options, then Allow \
             \u{2014} the test shows once you do."
        }
        TestOutcome::Refused => {
            "macOS is not showing Awili's notifications, so the test cannot appear. Open System \
             Settings below to allow them."
        }
        TestOutcome::NotShown => {
            "macOS cannot show Awili's notifications here, so the test was not shown."
        }
    }
}

fn ix_of(item: &Item) -> usize {
    cat::ITEMS.iter().position(|i| i.id == item.id).unwrap_or(0)
}

/// The unseen twin of an Sxi button: the same shared Button face (its
/// reserved check included), invisible, so a slot keeps the room its button
/// will need.
fn sx_placeholder(
    selector: String,
    id: (&'static str, usize),
    label: &'static str,
) -> gpui::AnyElement {
    div()
        .debug_selector(move || selector.clone())
        .invisible()
        .child(crate::ui::Button::new(id, label).reserve_selected_check(true))
        .into_any_element()
}

/// The Settings-owned button wrapper: its painted face is the KIT Button,
/// while focus order and focus indication remain owned by `settings_focus`.
#[derive(Clone, Copy)]
struct ButtonFaceLayout {
    selector: Option<&'static str>,
    full_width: bool,
}

fn sx_button_face(
    id: (&'static str, usize),
    label: SharedString,
    on: bool,
    focused: bool,
    disabled_reason: Option<SharedString>,
    button_selector: Option<&'static str>,
    action: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    sx_button_face_sized(
        id,
        label,
        on,
        focused,
        disabled_reason,
        ButtonFaceLayout {
            selector: button_selector,
            full_width: false,
        },
        action,
    )
}

/// A Settings action that fills a constrained wrapping slot. This is limited
/// to the notification actions; ordinary Settings controls keep their natural
/// face width.
fn sx_button_face_full_width(
    id: (&'static str, usize),
    label: SharedString,
    on: bool,
    focused: bool,
    disabled_reason: Option<SharedString>,
    button_selector: Option<&'static str>,
    action: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    sx_button_face_sized(
        id,
        label,
        on,
        focused,
        disabled_reason,
        ButtonFaceLayout {
            selector: button_selector,
            full_width: true,
        },
        action,
    )
}

fn sx_button_face_sized(
    id: (&'static str, usize),
    label: SharedString,
    on: bool,
    focused: bool,
    disabled_reason: Option<SharedString>,
    layout: ButtonFaceLayout,
    action: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    let mut button = crate::ui::Button::new(id, label)
        .selected(on)
        .reserve_selected_check(true)
        .focused(focused)
        .pointer_only()
        .allow_double_click()
        .disabled_if(disabled_reason)
        .on_click(action);
    if layout.full_width {
        button = button.full_width();
    }
    if let Some(selector) = layout.selector {
        button = button.selector(selector);
    }
    let mut wrapper = div()
        .children(focused.then(|| {
            div()
                .debug_selector(|| "sx-keyboard-focus".to_string())
                .absolute()
                .size(px(theme::SETTINGS_FOCUS_MARKER_PX))
        }))
        .flex()
        // The Settings focus state is the only keyboard stop for this face.
        // A parent row may pass a reason to disable a custom Reset as well.
        .child(button);
    if layout.full_width {
        // A column stretches its child across the available width. The KIT
        // Button's full-width face then fills that constrained root instead
        // of retaining the label's intrinsic width.
        wrapper = wrapper.w_full().flex_col().items_stretch();
    } else {
        wrapper = wrapper.flex_none();
    }
    if layout.selector.is_some() {
        wrapper.into_any_element()
    } else {
        wrapper
            .debug_selector(move || format!("{}-{}", id.0, id.1))
            .into_any_element()
    }
}

impl AwiliApp {
    /// Show a page (leaves "Every setting" / "Settings file" if open).
    pub(crate) fn select_settings_page(&mut self, page: Page, cx: &mut Context<Self>) {
        // RC2-R64: the Notifications page reports what macOS allows now.
        if page == Page::Notifications {
            self.notifier.query_permission();
        }
        // S6: the Allowed actions list is read live each time it opens.
        if page == Page::Allowed {
            self.reload_allowed_actions();
        }
        self.settings.set_tab(settings::Tab::Form);
        self.settings.page = page;
        self.settings.pending_choice = None;
        self.settings.saved_here = None;
        // SXi: a new page starts with the keyboard in the sidebar.
        self.settings_stop = None;
        self.settings_in_sidebar = true;
        cx.notify();
    }

    /// One press on a choice button, by click or by key. A guarded choice
    /// (SXe leaving the fleet route, SXh turning on Auto) arms first.
    pub(crate) fn press_settings_choice(
        &mut self,
        item: &'static Item,
        stored: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.settings.doc.as_ref() else {
            return;
        };
        let now = cat::current(doc, item);
        let on = cat::choice_selected(item, now.as_str(), stored);
        // A family choice can be selected by a pinned model (for example,
        // Claude Haiku). Clicking that already-selected family must preserve
        // the stored model instead of replacing it with the family default.
        if item.id == "work.default_executor" && on && now.as_str() != Some(stored) {
            return;
        }
        let guarded = !on
            && ((item.confirm_leave.is_some() && now.as_str() == item.confirm_leave)
                || item.confirm_to == Some(stored));
        if guarded && self.settings.pending_choice != Some((item.id, stored)) {
            self.settings.pending_choice = Some((item.id, stored));
            cx.notify();
        } else {
            self.apply_settings_item(item.id, Edit::Text(stored.to_string()), cx)
        }
    }

    /// One press on a row's Reset, by click or by key (SXe-FIX1: off a
    /// guarded value it arms first).
    pub(crate) fn press_settings_reset(&mut self, item: &'static Item, cx: &mut Context<Self>) {
        let Some(doc) = self.settings.doc.as_ref() else {
            return;
        };
        let now = cat::current(doc, item);
        let guarded = item.confirm_leave.is_some() && now.as_str() == item.confirm_leave;
        if guarded && self.settings.pending_choice != Some((item.id, RESET_ARMED)) {
            self.settings.pending_choice = Some((item.id, RESET_ARMED));
            cx.notify();
        } else {
            self.reset_settings_item(item.id, cx)
        }
    }

    /// Apply one edit to one setting and save at once (`settings_live`).
    pub(crate) fn apply_settings_item(&mut self, id: &str, edit: Edit, cx: &mut Context<Self>) {
        // SXe-FIX1: any change disarms a pending second-click choice.
        self.settings.pending_choice = None;
        if self.settings.apply_item(id, edit) {
            // RC2-UX (R2-21): say it next to the control.
            self.settings.saved_here = cat::find(id).map(|i| (i.id, SAVED_HERE));
            self.after_settings_change(id);
            if id == "appearance.color_mode" {
                self.sync_saved_appearance(cx);
            } else {
                cx.emit(ConfigReloaded);
            }
        }
        cx.notify();
    }

    /// Put one setting back to its built-in default (or remove it).
    pub(crate) fn reset_settings_item(&mut self, id: &str, cx: &mut Context<Self>) {
        // SXe-FIX1: a Reset disarms a pending second click too.
        self.settings.pending_choice = None;
        if self.settings.reset_item(id) {
            self.settings.saved_here = cat::find(id).map(|i| (i.id, RESET_HERE));
            // SXf-FIX1: a reset Work helper is the current executor (and the
            // remembered one) too.
            self.after_settings_change(id);
            if id == "appearance.color_mode" {
                self.sync_saved_appearance(cx);
            } else {
                cx.emit(ConfigReloaded);
            }
        }
        cx.notify();
    }

    /// Walk back the last change (`settings_live` Undo).
    pub(crate) fn undo_settings_change(&mut self, cx: &mut Context<Self>) {
        // SXe-FIX1: an Undo back onto a guarded value must ask again.
        self.settings.pending_choice = None;
        self.settings.saved_here = None;
        let helper = self.saved_work_helper();
        let before = self.saved_config();
        if self.settings.undo_last() {
            // SXf-FIX1: an Undo that changes the Work helper switches Work
            // (and the remembered choice) back too.
            if self.saved_work_helper() != helper {
                self.after_settings_change("work.default_executor");
            }
            if Self::appearance_only_change(before, self.saved_config()) {
                self.sync_saved_appearance(cx);
            } else {
                cx.emit(ConfigReloaded);
            }
        }
        cx.notify();
    }

    /// A big labelled button. `on` marks the chosen one of a group.
    pub(crate) fn sx_button(
        &self,
        id: (&'static str, usize),
        label: impl Into<SharedString>,
        on: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        self.sx_button_w(id, label, on, cx, move |t, _w, cx| action(t, cx))
    }

    /// A full-width Settings Button whose action may use the current window.
    /// This shares the Settings-owned focus state and callback path with sx_button_w.
    pub(crate) fn sx_button_full_width_w(
        &self,
        id: (&'static str, usize),
        label: impl Into<SharedString>,
        on: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label: SharedString = label.into();
        let focused = self.settings_nav_focused
            && !self.settings_in_sidebar
            && if self.settings.page == settings_catalog::Page::Models
                && self.settings.tab == settings::Tab::Form
            {
                Self::settings_nav_action_for_id(id) == Some(self.settings_nav.selected())
            } else {
                self.settings_stop.map(|s| s.button()) == Some(id)
            };
        let listener = cx.listener(move |this, _ev, window, cx| action(this, window, cx));
        sx_button_face_full_width(id, label, on, focused, None, None, listener)
    }

    /// [`Self::sx_button`] whose action also gets the window (focus, zoom).
    /// The chosen button carries its check; an unchosen one keeps the check's
    /// room unseen (SXd). The face is the shared [`crate::ui::Button`].
    pub(crate) fn sx_button_w(
        &self,
        id: (&'static str, usize),
        label: impl Into<SharedString>,
        on: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label: SharedString = label.into();
        // SXi: the keyboard's button is the kit's focused face, and a marker
        // inside its ring so a test can see where it is.
        let focused = self.settings_nav_focused
            && !self.settings_in_sidebar
            && if self.settings.page == settings_catalog::Page::Models
                && self.settings.tab == settings::Tab::Form
            {
                Self::settings_nav_action_for_id(id) == Some(self.settings_nav.selected())
            } else {
                self.settings_stop.map(|s| s.button()) == Some(id)
            };
        let listener = cx.listener(move |this, _ev, w, cx| action(this, w, cx));
        sx_button_face(id, label, on, focused, None, None, listener)
    }

    /// Notification actions fill their wrapping Card slot so the two real
    /// KIT button faces remain within the available card width.
    fn sx_notification_button_w(
        &self,
        id: (&'static str, usize),
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label: SharedString = label.into();
        let focused = self.settings_nav_focused
            && !self.settings_in_sidebar
            && self.settings_stop.map(|s| s.button()) == Some(id);
        let listener = cx.listener(move |this, _ev, w, cx| action(this, w, cx));
        sx_button_face_full_width(id, label, false, focused, None, None, listener)
    }

    /// The Settings-owned Reset face, deferred so SettingRow can supply its
    /// effective disabled reason without taking ownership of the callback.
    fn sx_reset_button_w(
        &self,
        id: (&'static str, usize),
        label: impl Into<SharedString>,
        on: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> crate::ui::setting_row::ResetSlotBuilder {
        let label: SharedString = label.into();
        let focused = self.settings_nav_focused
            && !self.settings_in_sidebar
            && self.settings_stop.map(|s| s.button()) == Some(id);
        let listener = cx.listener(move |this, _ev, w, cx| action(this, w, cx));
        Box::new(move |reason| sx_button_face(id, label, on, focused, reason, None, listener))
    }

    /// The sidebar: one big row per page.
    pub(crate) fn settings_page_nav(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let current = self.settings.page;
        let query = self.settings_query(cx);
        let mut nav = vec![self.settings_search_box(cx)];
        nav.extend(Page::ALL.iter().enumerate().map(|(ix, &page)| {
            let active = page == current && query.is_empty();
            let hits = (!query.is_empty()).then(|| self.settings_hits_on(page, &query));
            let count = hits.map(|n| {
                div()
                    .debug_selector(move || format!("sx-nav-count-{ix}"))
                    .child(
                        text::label(SharedString::from(n.to_string())).text_color_override(
                            if n > 0 {
                                theme::p().accent_text
                            } else {
                                theme::p().faint
                            },
                        ),
                    )
            });
            // SXi: the sidebar's keyboard ring (the open page) is the KIT
            // focus ring, driven by the Settings focus state.
            let focused = active && self.settings_in_sidebar && self.settings_nav_focused;
            let mut row = Row::new(("sx-nav", ix))
                .title(page.title())
                .selected(active)
                .focused(focused)
                // Settings owns the sidebar's Tab order and arrow keys.
                .pointer_only();
            if active {
                row = row.strong();
            }
            if let Some(count) = count {
                row = row.trailing(count);
            }
            let on_click = cx.listener(move |this, _ev, _w, cx| {
                this.clear_settings_search(cx);
                this.select_settings_page(page, cx)
            });
            // The page's test selector sits on this structural wrapper, so
            // it keeps the row's full hit area.
            div()
                .debug_selector(move || format!("sx-nav-{ix}"))
                .mx(theme::z(theme::SP_8))
                .child(row.on_click(on_click))
                .into_any_element()
        }));
        nav
    }

    /// The page: its blurb, then each group's heading and rows.
    pub(crate) fn settings_page_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let query = self.settings_query(cx);
        if !query.is_empty() {
            return self.settings_search_results(&query, cx);
        }
        let page = self.settings.page;
        let mut rows: Vec<gpui::AnyElement> = vec![div()
            .px(theme::z(theme::SP_16))
            .pt(theme::z(theme::SP_12))
            .child(text::ui_text(page.blurb()).reading().wrap())
            .into_any_element()];
        if page == Page::General {
            rows.push(self.settings_text_size_row(cx));
        }
        // S6: this page is a live list, not catalog rows.
        if page == Page::Allowed {
            rows.extend(self.allowed_actions_rows(cx));
            *self.settings_page_rows.borrow_mut() = vec![None; rows.len()];
            return div()
                .id("sx-page-scroll")
                .debug_selector(|| "sx-page".to_string())
                .track_scroll(&self.settings_page_scroll)
                .track_focus(&self.settings_nav_focus)
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .pb(theme::z(theme::SP_16))
                .children(rows)
                .into_any_element();
        }
        // Advanced: the two whole-file views come first, above the fold
        // (and even when the file cannot be read, so it can be repaired).
        if page == Page::Advanced {
            let path: SharedString =
                format!("Settings file: {}", self.settings.path().display()).into();
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(theme::z(theme::SP_8))
                    .px(theme::z(theme::SP_16))
                    .pt(theme::z(theme::SP_16))
                    .child(self.sx_button(
                        ("sx-every", 0),
                        "Show every setting",
                        false,
                        cx,
                        |t, cx| t.set_settings_tab(settings::Tab::Fields, cx),
                    ))
                    .child(self.sx_button(
                        ("sx-file", 0),
                        "Edit the settings file",
                        false,
                        cx,
                        |t, cx| t.set_settings_tab(settings::Tab::Raw, cx),
                    ))
                    .into_any_element(),
            );
            rows.push(
                div()
                    .px(theme::z(theme::SP_16))
                    .py(theme::z(theme::SP_8))
                    .child(text::caption(path))
                    .into_any_element(),
            );
        }
        let Some(doc) = self.settings.doc.clone() else {
            rows.push(
                div()
                    .p(theme::z(theme::SP_16))
                    .child(
                        text::ui_text(
                            "The settings file could not be read. Open Advanced, then Settings file, to fix it.",
                        )
                        .text_color_override(theme::p().chip_warn)
                        .wrap(),
                    )
                    .into_any_element(),
            );
            return div().flex().flex_col().children(rows).into_any_element();
        };
        // SXg: the Notifications page starts with macOS's answer, a test
        // button, and what to do about the permission.
        if page == Page::Notifications {
            rows.push(self.settings_notify_status(cx));
        }
        let items = cat::items_on(page, false);
        let mut group = "";
        // SXi: which row each catalog item is, for scroll-into-view.
        let mut item_rows: Vec<(usize, &'static str)> = Vec::new();
        // RC2-R511 (usability RC2 round 4, Minor: the Voice page said "Applies
        // after you quit Awili and open it again" four times): a page whose
        // rows all need a restart says it once, at its top.
        let page_noted = items.len() > 1 && items.iter().all(|i| i.restart);
        if page_noted {
            rows.push(
                div()
                    .debug_selector(|| "rc2r511-restart-note".to_string())
                    .px(theme::z(theme::SP_16))
                    .pt(theme::z(theme::SP_8))
                    .child(
                        text::label(format!(
                            "Every change here: {}.",
                            RESTART_NOTE.to_lowercase()
                        ))
                        .text_color_override(theme::p().chip_warn),
                    )
                    .into_any_element(),
            );
        }
        for item in items.iter().copied() {
            if item.group != group {
                group = item.group;
                rows.push(
                    div()
                        .px(theme::z(theme::SP_16))
                        .pt(theme::z(theme::SP_16))
                        .pb(theme::z(theme::SP_4))
                        .child(
                            text::label(group)
                                .strong()
                                .text_color_override(theme::p().accent_text),
                        )
                        .into_any_element(),
                );
            }
            item_rows.push((rows.len(), item.id));
            rows.push(self.settings_item_row_noted(&doc, item, !page_noted, cx));
        }
        // Helper setup follows the defaults so task choices stay above the
        // install and sign-in checklist.
        if page == Page::Workshop {
            rows.push(
                div()
                    .px(theme::z(theme::SP_16))
                    .pt(theme::z(theme::SP_20))
                    .pb(theme::z(theme::SP_4))
                    .child(
                        text::label("Helper setup")
                            .strong()
                            .text_color_override(theme::p().accent_text),
                    )
                    .into_any_element(),
            );
            rows.extend(self.settings_code_rows(cx));
        }
        if items.is_empty() && page != Page::Advanced {
            rows.push(
                div()
                    .p(theme::z(theme::SP_16))
                    .child(text::ui_text("Nothing to set here yet.").reading())
                    .into_any_element(),
            );
        }
        let mut map = vec![None; rows.len()];
        for (row, id) in item_rows {
            map[row] = Some(id);
        }
        *self.settings_page_rows.borrow_mut() = map;
        div()
            .id("sx-page-scroll")
            .debug_selector(|| "sx-page".to_string())
            .track_scroll(&self.settings_page_scroll)
            .track_focus(&self.settings_nav_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(theme::z(theme::SP_16))
            .children(rows)
            .into_any_element()
    }

    /// One setting: label and help on the left, its control on the right,
    /// and Reset when it differs from the default.
    pub(crate) fn settings_item_row(
        &self,
        doc: &awili_config::ConfigDoc,
        item: &'static Item,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.settings_item_row_noted(doc, item, true, cx)
    }

    /// RC2-R511: [`Self::settings_item_row`]; `restart_note` false when its
    /// group already says it once.
    pub(crate) fn settings_item_row_noted(
        &self,
        doc: &awili_config::ConfigDoc,
        item: &'static Item,
        restart_note: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ix = ix_of(item);
        let now = cat::current(doc, item);
        let changed = cat::is_modified(doc, item);
        let id = item.id;
        let control: gpui::AnyElement = match item.control {
            Control::Toggle => {
                let on = now.as_bool() == Some(true);
                let mut choices = div().flex().gap(theme::z(theme::SP_8));
                if theme::ui_scale() >= 2.0 {
                    // At large text zoom the Settings content column can be
                    // narrower than two natural Button faces. Keep each
                    // target at its normal size and stack the pair.
                    choices = choices.flex_col().items_end();
                } else {
                    choices = choices.flex_row();
                }
                choices
                    .child(self.sx_button(("sx-on", ix), "On", on, cx, move |t, cx| {
                        t.apply_settings_item(id, Edit::Bool(true), cx)
                    }))
                    .child(
                        self.sx_button(("sx-off", ix), "Off", !on, cx, move |t, cx| {
                            t.apply_settings_item(id, Edit::Bool(false), cx)
                        }),
                    )
                    .into_any_element()
            }
            Control::Choice(options) => {
                let mut row = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_end()
                    .gap(theme::z(theme::SP_8))
                    .max_w(theme::z(theme::PANEL_W));
                for (k, &(stored, label)) in options.iter().enumerate() {
                    // SXh: an absent key shows the choice it means.
                    let on = cat::choice_selected(item, now.as_str(), stored);
                    // SXe: leaving a guarded value (the household fleet route)
                    // takes a second click on the same button. SXh: so does
                    // choosing a guarded target ("Go ahead without asking").
                    // (SXi: the press itself is `press_settings_choice`.)
                    let to_guarded = item.confirm_to == Some(stored);
                    let armed = self.settings.pending_choice == Some((id, stored));
                    let text = match (armed, to_guarded) {
                        (true, true) => "Click again to confirm",
                        (true, false) => "Click again to switch",
                        (false, _) => label,
                    };
                    row = row.child(self.sx_button(
                        ("sx-choice", ix * 16 + k),
                        text,
                        on,
                        cx,
                        move |t, cx| t.press_settings_choice(item, stored, cx),
                    ));
                }
                row.into_any_element()
            }
            Control::Stepper { .. } | Control::Level { .. } => {
                let (down, up) = (cat::step(item, &now, false), cat::step(item, &now, true));
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        self.sx_button(("sx-dec", ix), "−", false, cx, move |t, cx| {
                            if let Some(e) = down.clone() {
                                t.apply_settings_item(id, e, cx)
                            }
                        }),
                    )
                    .child(
                        div()
                            .min_w(theme::z(theme::STEPPER_VALUE_W))
                            .flex()
                            .justify_center()
                            .child(text::ui_text(cat::display(item, &now))),
                    )
                    .child(
                        self.sx_button(("sx-inc", ix), "+", false, cx, move |t, cx| {
                            if let Some(e) = up.clone() {
                                t.apply_settings_item(id, e, cx)
                            }
                        }),
                    )
                    .into_any_element()
            }
            Control::Text | Control::Secret => {
                self.settings_text_control(item, ix, cat::display(item, &now), cx)
            }
        };
        // SXe-FIX1: Reset off a guarded value (the household fleet route)
        // takes a second click too.
        // (SXi: the press itself is `press_settings_reset`.)
        let reset_text = if self.settings.pending_choice == Some((id, RESET_ARMED)) {
            "Click again to reset"
        } else {
            "Reset"
        };
        // RC2-UX (R2-21): the Reset slot is always there (an unseen twin of the
        // same size while the row is at its default), so nothing moves under
        // the pointer after a change.
        let reset = changed.then(|| {
            self.sx_reset_button_w(
                ("sx-reset", ix),
                reset_text,
                false,
                cx,
                move |t, _window, cx| t.press_settings_reset(item, cx),
            )
        });
        let reset_twin = sx_placeholder(
            format!("sx-reset-slot-{ix}"),
            ("sx-reset-slot", ix),
            "Reset",
        );
        // RC2-UX (R2-21): "✓ Saved" sits on the label's line, so the row does
        // not grow. It is the parent's own mark: it holds until the page
        // changes, an Undo or the next edit, not for a flash.
        let saved_here = self
            .settings
            .saved_here
            .filter(|(at, _)| *at == id)
            .map(|(_, mark)| {
                div()
                    .debug_selector(move || format!("sx-saved-here-{id}"))
                    .child(text::label(mark).text_color_override(theme::p().accent_text))
                    .into_any_element()
            });
        // SXg: under a switch that is Off, the row is dimmed and says so; it
        // stays clickable (a visual dim, never a disabled row).
        let dimmed = item
            .depends_on
            .and_then(cat::find)
            .is_some_and(|parent| cat::current(doc, parent).as_bool() == Some(false));
        let mut row = crate::ui::SettingRow::new(id, item.label)
            .help(item.help)
            .visually_dimmed(dimmed)
            .label_mark(saved_here)
            .reset_slot(reset, reset_twin)
            .control(move |_reason| control);
        if dimmed {
            row = row.note(
                div().debug_selector(move || format!("sx-dim-{id}")).child(
                    text::label("Turn Notifications on to choose these.")
                        .text_color_override(theme::p().chip_warn),
                ),
            );
        }
        if item.restart && restart_note {
            row = row.note(
                div()
                    .debug_selector(move || format!("sx-restart-{id}"))
                    .child(text::label(RESTART_NOTE).text_color_override(theme::p().chip_warn)),
            );
        }
        // The row's own edge: its persistent mark and Reset carry the change.
        div()
            .debug_selector(move || format!("sx-row-{id}"))
            .mx(theme::z(theme::SP_12))
            .child(row)
            .into_any_element()
    }

    /// SXg: the top of the Notifications page: macOS's answer in words, a
    /// test button, and plain guidance for the permission prompt.
    pub(crate) fn settings_notify_status(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let permission = self.notifier.core.permission().clone();
        let guidance = match permission {
            notifications::Permission::Unknown | notifications::Permission::Requesting => Some(
                "macOS will show a small box in the top-right corner. Click Options, then Allow.",
            ),
            notifications::Permission::Denied => {
                Some("Notifications are turned off for Awili in macOS.")
            }
            _ => None,
        };
        let open_settings = (permission == notifications::Permission::Denied).then(|| {
            self.sx_notification_button_w(
                ("sx-notify-sys", 0),
                "Open System Settings",
                cx,
                |t, _window, cx| {
                    t.notify_open_system_settings();
                    cx.notify();
                },
            )
        });
        let mut lines: Vec<gpui::AnyElement> = vec![text::ui_text(SharedString::from(format!(
            "macOS: {}",
            self.notifier.status_line()
        )))
        .wrap()
        .into_any_element()];
        if let Some(g) = guidance {
            lines.push(
                div()
                    .debug_selector(|| "sx-notify-guide".to_string())
                    .child(text::ui_text(g).reading().wrap())
                    .into_any_element(),
            );
        }
        let mut actions: Vec<gpui::AnyElement> = vec![self.sx_notification_button_w(
            ("sx-notify-test", 0),
            "Send a test notification",
            cx,
            |t, _window, cx| {
                // RC2-UX (R2-23): say so in Awili too — what
                // the press did (RC2-FIXB4).
                let outcome = t.notify_send_test();
                t.show_info_toast(test_notice(outcome), cx);
                cx.notify();
            },
        )];
        actions.extend(open_settings);
        let action_row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .w_full()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .children(actions);
        div()
            .mx(theme::z(theme::SP_16))
            .mt(theme::z(theme::SP_12))
            .child(
                Card::new("sx-notify-card")
                    .selector("sx-notify-status")
                    .children(lines)
                    // Card's action slot is a single horizontal row. Keep its
                    // actions in one wrapping child so both remain reachable
                    // at narrow widths and larger interface scales.
                    .actions([action_row]),
            )
            .into_any_element()
    }

    /// Under the page: the error, or the last change with a big Undo.
    pub(crate) fn open_settings_error_details(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.error.is_none() {
            return;
        }
        self.settings_error_scroll
            .set_offset(point(gpui::Pixels::ZERO, gpui::Pixels::ZERO));
        self.settings_error_scroll_signature =
            self.settings.error.as_deref().map(settings_error_signature);
        let fallback_focus = if self.settings.tab == settings::Tab::Form {
            self.settings_nav_focus.clone()
        } else {
            self.settings_active_tab_focus.clone()
        };
        self.settings_error_parent_focus = window.focused(cx).or_else(|| Some(fallback_focus));
        if self.settings.tab == settings::Tab::Form {
            if self.settings.page == settings_catalog::Page::Models {
                self.settings_nav.select(settings_nav::Action::ErrorDetails);
                self.settings_nav_focus_pending = false;
            } else {
                self.settings_in_sidebar = false;
                self.settings_stop = Some(settings_focus::FocusStop::ErrorDetails);
            }
        }
        self.settings_error_details_open = true;
        cx.notify();
    }

    pub(crate) fn close_settings_error_details(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_error_details_open = false;
        self.settings_error_scroll_signature = None;
        if self.settings.open {
            if let Some(parent) = self.settings_error_parent_focus.take() {
                window.focus(&parent, cx);
            } else if self.settings.tab == settings::Tab::Form {
                window.focus(&self.settings_nav_focus, cx);
            }
        } else {
            self.settings_error_parent_focus = None;
        }
        cx.notify();
    }

    pub(crate) fn settings_error_details_sheet(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.settings_error_details_open {
            return None;
        }
        let error = self.settings.error.clone()?;
        let scroll = self.settings_error_scroll.clone();
        let body_focus = self.settings_error_body_focus.clone();
        // An explicit Div focus handle bypasses the Div's own tab-index
        // transfer in this GPUI fork, so configure the shared handle itself.
        let body_tab_focus = body_focus.clone().tab_stop(true).tab_index(0);
        let body = div()
            .id("settings-error-details-body")
            .debug_selector(|| "settings-error-details-body".to_string())
            .tab_index(0)
            .track_focus(&body_tab_focus)
            .capture_key_down(move |ev, window, cx| {
                if let Some(key) =
                    crate::transcript_keys::classify(&ev.keystroke, true, cfg!(target_os = "macos"))
                {
                    let key = match key {
                        crate::transcript_keys::ScrollKey::PageUp
                        | crate::transcript_keys::ScrollKey::PageDown => key,
                        _ => return,
                    };
                    let offset = scroll.offset();
                    let max = scroll.max_offset();
                    let viewport = scroll.bounds().size.height;
                    let y = crate::transcript_keys::target_offset_y(
                        key,
                        f32::from(offset.y),
                        f32::from(max.y),
                        f32::from(viewport),
                    );
                    scroll.set_offset(point(offset.x, px(y)));
                    window.prevent_default();
                    cx.stop_propagation();
                }
            })
            .children(error.split('\n').enumerate().map(|(index, line)| {
                crate::ui::text::read(line.to_string())
                    .wrap()
                    .id(("settings-full-error-line", index))
            }));
        let sheet_focus = self.settings_error_sheet_focus.clone();
        let close =
            cx.listener(|this, _ev, window, cx| this.close_settings_error_details(window, cx));
        Some(
            crate::ui::Panel::new("settings-error-details-sheet")
                // Settings errors stay modal at every window width.
                .sheet_when(true)
                .selector("settings-error-details-sheet")
                .focus_handle(&sheet_focus)
                .header("Settings error", Some("Complete error message".into()))
                .scroll_handle(self.settings_error_scroll.clone())
                .body(body)
                .on_close(close)
                .into_any_element(),
        )
    }

    pub(crate) fn settings_change_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.settings.confirm_close {
            return Some(self.settings_confirm_close_bar(cx));
        }
        let (message, color) = match (&self.settings.error, &self.settings.status) {
            (Some(err), _) => (err.clone(), theme::p().chip_warn),
            (None, Some(status)) => (format!("{status} ✓"), theme::p().accent_text),
            (None, None) => match self.settings.last_change() {
                Some(label) => (format!("Last change: {label}"), theme::p().muted),
                None => (
                    "Changes save as soon as you click.".into(),
                    theme::p().faint,
                ),
            },
        };
        let settings_owns_form_keys = self.settings.tab == settings::Tab::Form
            && (self.settings.page == settings_catalog::Page::Models
                || self.settings_query(cx).is_empty());
        let error_details = self.settings.error.as_ref().map(|_| {
            let id = ("sx-error-details", 0);
            if settings_owns_form_keys {
                if theme::ui_scale() >= 2.0 {
                    self.sx_button_full_width_w(
                        id,
                        "Read full error",
                        false,
                        cx,
                        |this, window, cx| this.open_settings_error_details(window, cx),
                    )
                } else {
                    self.sx_button_w(id, "Read full error", false, cx, |this, window, cx| {
                        this.open_settings_error_details(window, cx)
                    })
                }
            } else if theme::ui_scale() >= 2.0 {
                let button = crate::ui::Button::new(id, "Read full error")
                    .full_width()
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.open_settings_error_details(window, cx)
                    }));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_stretch()
                    .child(button)
                    .into_any_element()
            } else {
                crate::ui::Button::new(id, "Read full error")
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.open_settings_error_details(window, cx)
                    }))
                    .into_any_element()
            }
        });
        // The Undo slot is always there on the pages (disabled when there is
        // nothing to undo), so nothing moves after a change (SXd).
        let undo = (self.settings.tab == settings::Tab::Form).then(|| {
            if self.settings.last_change().is_some() {
                self.sx_button(("sx-undo", 0), "Undo", false, cx, |t, cx| {
                    t.undo_settings_change(cx)
                })
            } else {
                // RC2-UX: the same room as the Undo button (its unseen check).
                sx_placeholder("sx-undo-off".to_string(), ("sx-undo-off", 0), "Undo")
            }
        });
        Some(
            div()
                .debug_selector(|| "sx-debug-footer".to_string())
                .flex_none()
                .flex()
                .flex_col()
                .child(Divider::new())
                .child(
                    div()
                        .debug_selector(|| "sx-debug-footer-row".to_string())
                        .flex()
                        .flex_row()
                        // At large text scales the Undo face can consume
                        // nearly the whole content column. Keep the status
                        // readable by wrapping Undo onto its own line rather
                        // than shrinking the message to a few pixels.
                        .flex_wrap()
                        .items_center()
                        .gap(theme::z(theme::SP_8))
                        .px(theme::z(theme::SP_12))
                        // Keep the footer compact so the page retains room
                        // for its scroll viewport beside the modal header.
                        .py(theme::z(theme::SP_4))
                        .child(
                            div()
                                .debug_selector(|| "sx-status".to_string())
                                .flex_1()
                                .min_w(theme::z(theme::SETTINGS_STATUS_MIN_W))
                                .child(
                                    text::ui_text(SharedString::from(message))
                                        // The KIT's single-line text slot
                                        // keeps long status and error copy
                                        // from consuming the page viewport;
                                        // the id provides its full-text tooltip.
                                        .id("settings-change-status")
                                        .text_color_override(color),
                                ),
                        )
                        .children(undo)
                        .children(error_details),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
#[path = "settings_pages_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "settings_error_details_tests.rs"]
mod error_details_tests;

#[cfg(test)]
#[path = "settings_notify_tests.rs"]
mod notify_tests;

#[cfg(test)]
#[path = "settings_approval_tests.rs"]
mod approval_tests;

#[cfg(test)]
#[path = "settings_keys_tests.rs"]
mod keys_tests;
