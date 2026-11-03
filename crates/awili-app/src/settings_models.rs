//! SXe: the Models & Fleet page and safe closing (design:
//! the settings design notes, O6/O7). Model services keep
//! an explicit Save because a half-typed service is not valid; the Save and
//! Throw-away buttons show only when a card differs from the file. The
//! default model is live. Closing Settings (Done, Esc, ⌘,) while services
//! are unsaved never loses them: the change bar asks first.
use super::*;

pub(crate) type ServiceActionRevealKey = std::rc::Rc<
    std::cell::Cell<
        Option<(
            settings_nav::Action,
            gpui::Bounds<gpui::Pixels>,
            gpui::Size<gpui::Pixels>,
        )>,
    >,
>;

impl AwiliApp {
    /// Close Settings, unless model services are unsaved: then ask.
    pub(crate) fn request_close_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings.services_dirty() {
            self.settings.confirm_close = true;
            // SXe-FIX1: the question shows only a refusal of its own Save.
            self.settings.error = None;
            cx.notify();
        } else {
            self.close_settings(cx);
        }
    }

    pub(crate) fn apply_default_model_choice(
        &mut self,
        choice: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if self.settings.apply_default_model(choice) {
            cx.emit(ConfigReloaded);
        }
        cx.notify();
    }

    /// The intro above Model services, in the first Models & Fleet scroll
    /// row so it cannot starve the provider and local-discovery viewport.
    pub(crate) fn settings_models_intro(&self) -> gpui::AnyElement {
        crate::ui::text::body(
            "Only needed for models Awili calls directly, such as local Ollama or a fleet server. Chat, Work and Code use their own assistants (see those pages).",
        )
        .reading()
        .wrap()
        .into_any_element()
    }

    /// The form's service notice and Save / Throw away actions, appended
    /// after the rows in its bounded scroll viewport. Actions show only
    /// while a model-service card differs from the file.
    pub(crate) fn settings_services_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let dirty = self.settings.services_dirty();
        let mut content: Vec<gpui::AnyElement> = Vec::new();
        if let Some(notice) = self.settings.restart_notice.clone() {
            content.push(
                crate::ui::Banner::new(
                    ("settings-restart-notice", 0usize),
                    theme::Tone::Attention,
                    notice,
                )
                .into_any_element(),
            );
        }
        if dirty {
            content.push(
                crate::ui::text::label("Model services have unsaved changes.")
                    .tone(theme::Tone::Attention)
                    .wrap()
                    .into_any_element(),
            );
        }
        if content.is_empty() && !dirty {
            return crate::ui::Divider::new().into_any_element();
        }

        let mut actions: Vec<gpui::AnyElement> = Vec::new();
        let stack_actions = theme::ui_scale() >= 2.0;
        if dirty {
            let focused_discard = self.settings_nav_focused
                && !self.settings_in_sidebar
                && self.settings.page == settings_catalog::Page::Models
                && self.settings.tab == settings::Tab::Form
                && self.settings_nav.selected() == settings_nav::Action::Discard;
            let discard_bounds = self.settings_service_action_bounds[1].clone();
            let mut discard =
                crate::ui::Button::new(("sx-discard-services", 0usize), "Throw away changes")
                    .selected(false)
                    .reserve_selected_check(true)
                    .focused(focused_discard)
                    .pointer_only()
                    .allow_double_click()
                    .bounds_handle(discard_bounds)
                    .on_click(cx.listener(|t, _ev, _window, cx| {
                        t.settings.revert_services();
                        cx.notify();
                    }));
            if stack_actions {
                discard = discard.full_width();
            }
            actions.push(
                div()
                    .children(focused_discard.then(|| {
                        div()
                            .debug_selector(|| "sx-keyboard-focus".to_string())
                            .absolute()
                            .size(px(theme::SETTINGS_FOCUS_MARKER_PX))
                    }))
                    .debug_selector(|| "sx-discard-services-0".to_string())
                    .flex()
                    .when(stack_actions, |d| d.w_full().flex_col().items_stretch())
                    .when(!stack_actions, |d| d.flex_none())
                    .child(discard)
                    .into_any_element(),
            );
            let focused_save = self.settings_nav_focused
                && !self.settings_in_sidebar
                && self.settings.page == settings_catalog::Page::Models
                && self.settings.tab == settings::Tab::Form
                && self.settings_nav.selected() == settings_nav::Action::Save;
            let save_bounds = self.settings_service_action_bounds[0].clone();
            let mut save =
                crate::ui::Button::new(("sx-save-services", 0usize), "Save model services")
                    .selected(true)
                    .reserve_selected_check(true)
                    .focused(focused_save)
                    .pointer_only()
                    .allow_double_click()
                    .bounds_handle(save_bounds)
                    .on_click(cx.listener(|t, _ev, _window, cx| {
                        t.save_draft_settings(cx);
                    }));
            if stack_actions {
                save = save.full_width();
            }
            actions.push(
                div()
                    .children(focused_save.then(|| {
                        div()
                            .debug_selector(|| "sx-keyboard-focus".to_string())
                            .absolute()
                            .size(px(theme::SETTINGS_FOCUS_MARKER_PX))
                    }))
                    .debug_selector(|| "sx-save-services-0".to_string())
                    .flex()
                    .when(stack_actions, |d| d.w_full().flex_col().items_stretch())
                    .when(!stack_actions, |d| d.flex_none())
                    .child(save)
                    .into_any_element(),
            );
        }
        let action_row = if stack_actions {
            div()
                .flex()
                .flex_col()
                .items_stretch()
                .w_full()
                .gap(theme::z(theme::SP_8))
                .children(actions)
        } else {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .w_full()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .children(actions)
        };
        let action = self.settings_nav.selected();
        let action_bounds = match action {
            settings_nav::Action::Save => Some(self.settings_service_action_bounds[0].clone()),
            settings_nav::Action::Discard => Some(self.settings_service_action_bounds[1].clone()),
            _ => None,
        };
        let reveal_owner = cx.entity().downgrade();
        let reveal_key = self.settings_service_reveal_key.clone();
        let reveal_focus = self.settings_nav_focus.clone();
        let reveal_scroll = self.settings_nav_scroll.clone();
        let selected_action = self.settings_nav.selected();
        let card = crate::ui::Card::new("settings-services-footer")
            .children(content)
            // `Card` keeps its own actions slot on one line. Wrap the natural
            // button faces at ordinary scale; at large text scale, stack
            // full-width Buttons so each target and its tooltip remain usable.
            .actions([action_row]);
        div()
            .on_children_prepainted(move |_, window, cx| {
                let Some(face_handle) = action_bounds.as_ref() else {
                    reveal_key.set(None);
                    return;
                };
                if !matches!(
                    selected_action,
                    settings_nav::Action::Save | settings_nav::Action::Discard
                ) || !reveal_focus.is_focused(window)
                {
                    reveal_key.set(None);
                    return;
                }
                let Some(face) = face_handle.get() else {
                    reveal_key.set(None);
                    return;
                };
                let offset = reveal_scroll.offset();
                let normalized_face = gpui::Bounds::new(
                    gpui::point(face.origin.x, face.origin.y - offset.y),
                    face.size,
                );
                let viewport = reveal_scroll.bounds();
                let key = (selected_action, normalized_face, viewport.size);
                if reveal_key.get() == Some(key) {
                    return;
                }
                reveal_key.set(Some(key));

                let reveal_key = reveal_key.clone();
                let face_handle = face_handle.clone();
                let reveal_scroll = reveal_scroll.clone();
                let reveal_focus = reveal_focus.clone();
                let reveal_owner = reveal_owner.clone();
                window.defer(cx, move |window, cx| {
                    if reveal_key.get() != Some(key)
                        || !reveal_focus.is_focused(window)
                        || !reveal_owner
                            .read_with(cx, |app, _| {
                                app.settings.open
                                    && app.settings.tab == settings::Tab::Form
                                    && app.settings.page == settings_catalog::Page::Models
                                    && app.settings.services_dirty()
                                    && app.settings_nav.selected() == selected_action
                            })
                            .unwrap_or(false)
                    {
                        return;
                    }
                    let Some(face) = face_handle.get() else {
                        return;
                    };
                    let viewport = reveal_scroll.bounds();
                    let mut offset = reveal_scroll.offset();
                    if face.top() < viewport.top() {
                        offset.y += viewport.top() - face.top();
                    } else if face.bottom() > viewport.bottom() {
                        offset.y += viewport.bottom() - face.bottom();
                    }
                    offset.y = offset
                        .y
                        .clamp(-reveal_scroll.max_offset().y, gpui::Pixels::ZERO);
                    if offset != reveal_scroll.offset() {
                        reveal_scroll.set_offset(offset);
                        window.refresh();
                    }
                });
            })
            .child(card)
            .into_any_element()
    }

    /// The default model as big buttons: "First available" plus one per
    /// model of each enabled SAVED service. One click saves it.
    pub(crate) fn settings_default_model_rows(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let saved = self
            .settings
            .doc
            .as_ref()
            .map(settings::Draft::from_doc)
            .map(|d| (d.model_options(), d.default_provider, d.default_model))
            .unwrap_or_default();
        let (options, provider, model) = saved;
        let auto_on = provider.is_none() && model.is_none();
        let mut choices: Vec<gpui::AnyElement> =
            vec![
                crate::ui::Button::new(("sx-model-auto", 0usize), "First available")
                    .quiet()
                    .selected(auto_on)
                    .reserve_selected_check(true)
                    .pointer_only()
                    .allow_double_click()
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.apply_default_model_choice(None, cx);
                    }))
                    .into_any_element(),
            ];
        for (i, (p, m)) in options.into_iter().enumerate() {
            let on =
                provider.as_deref() == Some(p.as_str()) && model.as_deref() == Some(m.as_str());
            let label = format!("{p} · {m}");
            choices.push(
                crate::ui::Button::new(("sx-model", i), label)
                    .quiet()
                    .selected(on)
                    .reserve_selected_check(true)
                    .pointer_only()
                    .allow_double_click()
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.apply_default_model_choice(Some((p.clone(), m.clone())), cx);
                    }))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_16))
            .py(theme::z(theme::SP_4))
            .children(choices)
            .into_any_element()
    }

    /// The change bar while closing waits on unsaved model services.
    pub(crate) fn settings_confirm_close_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut children: Vec<gpui::AnyElement> = Vec::new();
        if let Some(error) = self.settings.error.clone() {
            children.push(
                crate::ui::Banner::new(
                    "sx-confirm-error",
                    theme::Tone::Attention,
                    format!("Not saved: {error}"),
                )
                .selector("sx-confirm-error")
                .quiet_dot()
                .into_any_element(),
            );
        }
        let actions: Vec<gpui::AnyElement> = vec![
            self.sx_button(
                ("sx-confirm-keep", 0),
                "Keep editing",
                false,
                cx,
                |t, cx| {
                    t.settings.confirm_close = false;
                    cx.notify();
                },
            ),
            self.sx_button(
                ("sx-confirm-discard", 0),
                "Throw away and close",
                false,
                cx,
                |t, cx| {
                    t.settings.revert_services();
                    t.close_settings(cx);
                },
            ),
            self.sx_button(
                ("sx-confirm-save", 0),
                "Save and close",
                true,
                cx,
                |t, cx| {
                    if t.save_draft_settings(cx) {
                        t.close_settings(cx);
                    }
                },
            ),
        ];
        crate::ui::Card::new("settings-close-confirm")
            .selector("sx-confirm-close")
            .tone(theme::Tone::Attention)
            .label("Unsaved model-service changes")
            .body(
                "Choose whether to keep editing, throw away the draft, or save and close Settings.",
            )
            .children(children)
            .actions(actions)
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "settings_models_tests.rs"]
mod tests;
