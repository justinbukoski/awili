//! Awili's consolidated navigation. Execution lanes remain internal to Work.
use gpui::{div, prelude::*, AnyElement, Context, FocusHandle};

use crate::{icons::Icon, theme, ui, work::Mode, AwiliApp};

#[derive(Clone, PartialEq)]
pub(crate) struct MessagingRevealKey {
    pub(crate) focus: FocusHandle,
    pub(crate) face: gpui::Bounds<gpui::Pixels>,
    pub(crate) inner: gpui::Bounds<gpui::Pixels>,
    pub(crate) body: gpui::Bounds<gpui::Pixels>,
}

pub(crate) struct Shell {
    pub(crate) tabs: crate::workshop::Workshop,
    pub(crate) archived_reader: Option<crate::workshop::ArchivedReader>,
    pub(crate) composer_details: bool,
    pub(crate) job_open: bool,
    pub(crate) inspector: crate::reply_inspector::InspectorState,
    pub(crate) job_tab: crate::job_panel::JobTab,
    pub(crate) job_details: bool,
    pub(crate) job_focus: FocusHandle,
    pub(crate) approval_focus: FocusHandle,
    pub(crate) error_focus: FocusHandle,
    pub(crate) blocking_reveal:
        std::rc::Rc<std::cell::RefCell<Option<crate::work::BlockingReveal>>>,
    pub(crate) job_tabs_scroll: gpui::ScrollHandle,
    pub(crate) composer_menus: crate::workshop_composer::ComposerMenus,
    pub(crate) conversations: bool,
    pub(crate) conversations_focus: FocusHandle,
    pub(crate) more: bool,
    pub(crate) more_selected: usize,
    pub(crate) workshop: Mode,
    pub(crate) backstage: Mode,
    pub(crate) rail_focus: FocusHandle,
    /// Focus of last resort: the window root. Off macOS it takes focus when nothing
    /// else has it, so key bindings scoped by context (`!Terminal`) still have a
    /// context path to match (see `app_lifecycle::ensure_root_focus`).
    pub(crate) root_focus: FocusHandle,
    pub(crate) navigation_focus: FocusHandle,
    pub(crate) rail_scroll: gpui::ScrollHandle,
    pub(crate) history_scroll: gpui::ScrollHandle,
    pub(crate) backstage_scroll: gpui::ScrollHandle,
    pub(crate) backstage_tabs_scroll: gpui::ScrollHandle,
    pub(crate) messaging_connectors_scroll: gpui::ScrollHandle,
    pub(crate) messaging_env_scroll: gpui::ScrollHandle,
    pub(crate) messaging_action_focus: std::collections::HashMap<String, FocusHandle>,
    pub(crate) messaging_action_bounds: std::collections::HashMap<
        String,
        std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    >,
    pub(crate) messaging_reveal_key: std::rc::Rc<std::cell::RefCell<Option<MessagingRevealKey>>>,
    pub(crate) skills_scroll: gpui::ScrollHandle,
    pub(crate) cron_scroll: gpui::ScrollHandle,
    pub(crate) sessions_scroll: gpui::ScrollHandle,
    pub(crate) menu_focus: FocusHandle,
    pub(crate) more_trigger_focus: FocusHandle,
    pub(crate) backstage_focus: FocusHandle,
}

impl Shell {
    pub(crate) fn new(cx: &mut Context<AwiliApp>) -> Self {
        Self {
            tabs: crate::workshop::Workshop::new(cx),
            archived_reader: None,
            composer_details: false,
            job_open: false,
            inspector: crate::reply_inspector::InspectorState::new(cx),
            job_tab: crate::job_panel::JobTab::History,
            job_details: false,
            job_focus: cx.focus_handle(),
            approval_focus: cx.focus_handle(),
            error_focus: cx.focus_handle(),
            blocking_reveal: Default::default(),
            job_tabs_scroll: gpui::ScrollHandle::new(),
            composer_menus: crate::workshop_composer::ComposerMenus::new(cx),
            conversations: false,
            conversations_focus: cx.focus_handle(),
            more: false,
            more_selected: 0,
            workshop: Mode::Work,
            backstage: Mode::Skills,
            rail_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            navigation_focus: cx.focus_handle(),
            rail_scroll: gpui::ScrollHandle::new(),
            history_scroll: gpui::ScrollHandle::new(),
            backstage_scroll: gpui::ScrollHandle::new(),
            backstage_tabs_scroll: gpui::ScrollHandle::new(),
            messaging_connectors_scroll: gpui::ScrollHandle::new(),
            messaging_env_scroll: gpui::ScrollHandle::new(),
            messaging_action_focus: std::collections::HashMap::new(),
            messaging_action_bounds: std::collections::HashMap::new(),
            messaging_reveal_key: std::rc::Rc::default(),
            skills_scroll: gpui::ScrollHandle::new(),
            cron_scroll: gpui::ScrollHandle::new(),
            sessions_scroll: gpui::ScrollHandle::new(),
            menu_focus: cx.focus_handle(),
            more_trigger_focus: cx.focus_handle(),
            backstage_focus: cx.focus_handle(),
        }
    }
}

/// Old utility routes select a section of Backstage. Their identities remain
/// stable for saved commands, toast ownership and section keyboard handling.
pub(crate) const BACKSTAGE: [(Mode, &str); 5] = [
    (Mode::Skills, "Skills"),
    (Mode::Cron, "Automations"),
    (Mode::Sessions, "Sessions"),
    (Mode::Messaging, "Connections"),
    (Mode::CommandCenter, "Health"),
];

pub(crate) fn backstage_index(mode: Mode) -> Option<usize> {
    BACKSTAGE.iter().position(|(route, _)| *route == mode)
}

pub(crate) fn conversation_mode(mode: Mode) -> bool {
    workshop_mode(mode) || mode == Mode::Council
}

pub(crate) fn workshop_mode(mode: Mode) -> bool {
    matches!(mode, Mode::Work | Mode::Code)
}

/// Inner lists consume a wheel only when they can move. Hidden overflow keeps
/// GPUI's default wheel listener from applying the delta a second time; the
/// tracked handle supplies offset and layout clamping; hidden overflow clips.
/// At an inner limit the event reaches the surrounding Backstage section instead.
pub(crate) fn scroll_backstage_list(
    handle: &gpui::ScrollHandle,
    event: &gpui::ScrollWheelEvent,
    window: &gpui::Window,
) -> bool {
    let delta = event.delta.pixel_delta(window.line_height());
    let delta_y = if delta.y == gpui::Pixels::ZERO {
        delta.x
    } else {
        delta.y
    };
    let before = handle.offset();
    let next_y = (before.y + delta_y).clamp(-handle.max_offset().y, gpui::Pixels::ZERO);
    if next_y == before.y {
        return false;
    }
    handle.set_offset(gpui::point(before.x, next_y));
    true
}

impl AwiliApp {
    pub(crate) fn toggle_conversations(&mut self, cx: &mut Context<Self>) {
        let from_workshop = workshop_mode(self.mode);
        let showing = conversation_mode(self.mode) && self.shell.conversations;
        if self.mode != Mode::Council && self.shell.archived_reader.is_none() {
            self.open_workshop(cx);
        }
        if workshop_mode(self.mode) && self.shell.archived_reader.is_none() {
            // The right-hand Conversations panel: toggle it from the Workshop,
            // show it when arriving from another page.
            if from_workshop {
                self.toggle_workshop_history(cx);
            } else {
                self.set_workshop_history_open(true, cx);
            }
            return;
        }
        self.shell.conversations = !showing;
        self.sidebar.collapsed = false;
        cx.notify();
    }
    pub(crate) fn focus_shell_page(&mut self, window: &mut gpui::Window, cx: &mut gpui::App) {
        if conversation_mode(self.mode) {
            self.focus_composer(window, cx);
        } else {
            window.focus(&self.shell.rail_focus, cx);
        }
    }

    pub(crate) fn consolidated_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let handle = cx.entity().downgrade();
        let mut rail = ui::NavRail::new("awili-navigation")
            .selector("awili-rail")
            .scroll_handle(self.shell.rail_scroll.clone())
            .focus_handle(&self.shell.rail_focus)
            .item_focus_handle(4, &self.shell.more_trigger_focus)
            .item(
                "awili-rail-Workshop",
                Icon::Chat,
                "Workshop",
                workshop_mode(self.mode),
            )
            .item(
                "awili-rail-Council",
                Icon::People,
                "Council",
                self.mode == Mode::Council,
            )
            .item(
                "awili-rail-Design",
                Icon::Palette,
                "Design",
                self.mode == Mode::Design,
            )
            .item(
                "awili-rail-Backstage",
                Icon::Servers,
                "Backstage",
                backstage_index(self.mode).is_some(),
            )
            .divider()
            .item(
                "awili-rail-More",
                Icon::MoreHorizontal,
                "More",
                self.shell.more,
            )
            .on_item_click(move |index, event, window, cx| {
                let _ = handle.update(cx, |this, cx| {
                    if this.onboarding.is_active() {
                        return;
                    }
                    let was_more = this.shell.more;
                    this.shell.more = false;
                    match index {
                        0 => {
                            if this.mode == Mode::Work && this.shell.archived_reader.is_none() {
                                this.toggle_job_panel(window, cx);
                                return;
                            }
                            this.open_workshop(cx);
                        }
                        1 => this.set_mode(Mode::Council, cx),
                        2 => this.set_mode(Mode::Design, cx),
                        3 => this.set_mode(this.shell.backstage, cx),
                        4 => {
                            this.ptt_key_down(cx);
                            this.shell.more = !was_more;
                            this.shell.more_selected = 0;
                            let focus = if this.shell.more {
                                &this.shell.menu_focus
                            } else {
                                &this.shell.more_trigger_focus
                            };
                            window.focus(focus, cx);
                            cx.notify();
                        }
                        _ => {}
                    }
                    if index < 4 {
                        // The newly active item tracks the shared handle.
                        // Keyboard activation stays on navigation; clicking a
                        // conversation page makes its composer ready to type.
                        if matches!(event, gpui::ClickEvent::Mouse(_)) {
                            this.focus_shell_page(window, cx);
                        } else {
                            window.focus(&this.shell.rail_focus, cx);
                        }
                    }
                    cx.notify();
                });
            });
        if self.onboarding.is_active() {
            rail = rail.disabled("Complete setup to navigate Awili");
        }
        if self.shell.more {
            // The legacy execution lanes remain reachable during M2/M3's
            // migration. They are not presented as additional rail pages.
            let move_handle = cx.entity().downgrade();
            let close_handle = cx.entity().downgrade();
            let menu = ui::Menu::new("awili-more-menu")
                .focus_handle(&self.shell.menu_focus)
                .trigger_focus(&self.shell.more_trigger_focus)
                .selected(self.shell.more_selected)
                .on_move(move |index, _window, cx| {
                    let _ = move_handle.update(cx, |this, cx| {
                        this.shell.more_selected = index;
                        cx.notify();
                    });
                })
                .on_close(move |window, cx| {
                    let _ = close_handle.update(cx, |this, cx| {
                        window.focus(&this.shell.more_trigger_focus, cx);
                        this.shell.more = false;
                        cx.notify();
                    });
                })
                .item(
                    ui::MenuItem::new("Settings")
                        .icon(Icon::Gear)
                        .shortcut(crate::shortcuts::localize("⌘,"))
                        .on_click(cx.listener(|this, _event, _window, cx| this.open_settings(cx))),
                )
                .item(
                    ui::MenuItem::new("Conversations")
                        .icon(Icon::Chat)
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.toggle_conversations(cx);
                        })),
                )
                .item(
                    ui::MenuItem::new("Keyboard shortcuts")
                        .icon(Icon::Menu)
                        .shortcut(crate::shortcuts::localize("⌘?"))
                        .on_click(cx.listener(|this, _event, window, cx| {
                            this.toggle_shortcuts(cx);
                            this.focus_shortcuts_overlay(window, cx);
                        })),
                );
            // Windows and Linux have no global menu bar: the commands the macOS
            // menu carries (File, Edit, View, Help) live in this menu instead.
            #[cfg(not(target_os = "macos"))]
            let menu = app_menu_items(menu);
            rail = rail.more(menu);
        }
        div()
            .id("awili-nav-host")
            .track_focus(&self.shell.navigation_focus)
            .absolute()
            .left(theme::z(theme::EDGE))
            .top(gpui::Pixels::ZERO)
            .bottom(gpui::Pixels::ZERO)
            .flex()
            .items_center()
            .child(rail)
            .into_any_element()
    }

    pub(crate) fn backstage_page(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = backstage_index(self.mode).unwrap_or_default();
        let compact = ui::is_compact(window);
        let tab_layout = (selected, window.viewport_size().width, theme::ui_scale());
        let last_layout = window.use_keyed_state("awili-backstage-tab-layout", cx, |_, _| None);
        let reveal = last_layout.update(cx, |last, _| {
            let changed = *last != Some(tab_layout);
            *last = Some(tab_layout);
            changed
        });
        if reveal {
            let scroll = self.shell.backstage_tabs_scroll.clone();
            scroll.scroll_to_item(selected);
            // GPUI's first reveal sees the previous viewport bounds. Repeat
            // after layout for resize/zoom and initial mount; skip a superseded
            // request. Wheel-only redraws retain manual strip scrolling.
            window.defer(cx, move |window, cx| {
                if *last_layout.read(cx) == Some(tab_layout) {
                    scroll.scroll_to_item(selected);
                    window.refresh();
                }
            });
        }
        let handle = cx.entity().downgrade();
        let tabs = ui::Tabs::new(
            "awili-backstage-sections",
            BACKSTAGE
                .iter()
                .map(|(_, label)| {
                    ui::TabItem::new(
                        gpui::SharedString::from(format!("backstage-{label}")),
                        *label,
                    )
                })
                .collect(),
            selected,
        )
        .scroll_handle(self.shell.backstage_tabs_scroll.clone())
        .focus_handle(&self.shell.backstage_focus)
        .on_select(move |index, _event, _window, cx| {
            let _ = handle.update(cx, |this, cx| {
                if let Some((route, _)) = BACKSTAGE.get(index) {
                    this.set_mode(*route, cx);
                }
            });
        });
        // Reuse the real section, including its in-flight drafts and actions.
        // No gallery samples or new network path enter this consolidation.
        let body = match self.mode {
            Mode::Skills => self.skills_panel(cx).into_any_element(),
            Mode::Cron => self.cron_panel(cx).into_any_element(),
            Mode::Sessions => self.sessions_panel(cx),
            Mode::Messaging => self.messaging_panel(window, cx).into_any_element(),
            _ => self.command_center_panel(cx).into_any_element(),
        };
        div()
            .debug_selector(|| "awili-backstage".to_string())
            .flex()
            .flex_col()
            .flex_1()
            .min_w(gpui::Pixels::ZERO)
            .min_h(gpui::Pixels::ZERO)
            .gap(theme::z(theme::SP_16))
            .px(theme::z(theme::SP_24))
            .py(theme::z(if compact { theme::SP_12 } else { theme::SP_24 }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap(theme::z(theme::SP_16))
                    .child(ui::text::title("Backstage").id("awili-backstage-title"))
                    .child(tabs)
                    .child(
                        ui::text::body(self.mode.rail_hint())
                            .id("awili-backstage-hint")
                            .wrap(),
                    ),
            )
            .child(
                // The section owns the remaining height. Inner lists consume
                // wheel movement before it; this boundary also makes oversized
                // section headers/notices reachable without shrinking ours.
                div()
                    .id("awili-backstage-body")
                    .debug_selector(|| "awili-backstage-body".to_string())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(theme::target())
                    .track_scroll(&self.shell.backstage_scroll)
                    .overflow_y_scroll()
                    .child(
                        div()
                            .debug_selector(|| "awili-backstage-section".to_string())
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .h_full()
                            .min_h(theme::z(theme::BACKSTAGE_SECTION_MIN_H))
                            .when(self.mode == Mode::Messaging, |section| {
                                section.h_auto().flex_grow(1.0)
                            })
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn design_page(&self) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .child(
                ui::EmptyState::new(
                    "awili-design",
                    "Design",
                    "A place for visual design work — coming in a later release.",
                )
                .glyph(Icon::Palette)
                .selector("awili-design-placeholder"),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_legacy_service_route_has_one_backstage_section() {
        assert_eq!(BACKSTAGE.len(), 5);
        for (index, (mode, _)) in BACKSTAGE.iter().enumerate() {
            assert_eq!(backstage_index(*mode), Some(index));
            assert!(!workshop_mode(*mode));
        }
        for mode in [Mode::Work, Mode::Code, Mode::Council] {
            assert_eq!(backstage_index(mode), None);
        }
    }
}

/// One command of the in-window app menu: the same action the macOS menu bar
/// item runs, dispatched through GPUI so every handler is shared.
#[cfg(not(target_os = "macos"))]
fn app_menu_item<A: gpui::Action + Clone>(
    label: impl Into<gpui::SharedString>,
    shortcut: Option<&'static str>,
    action: A,
) -> ui::MenuItem {
    let mut item = ui::MenuItem::new(label).on_click(move |_event, window, cx| {
        window.dispatch_action(Box::new(action.clone()), cx);
    });
    if let Some(shortcut) = shortcut {
        item = item.shortcut(crate::shortcuts::localize(shortcut));
    }
    item
}

/// Windows and Linux: append the menu bar's most-used commands (File, Edit, View
/// and Help equivalents) to the "More" menu (Settings, Conversations and Keyboard
/// shortcuts are above). The menu opens upward from the rail and is as tall as
/// the window at most, so it stays short: at 150 percent scaling on a 800 px tall
/// screen only about ten rows fit. Zoom, Toggle Needs You and the rest are one
/// Ctrl+K away (the command palette), and Zoom keeps its Ctrl+= / Ctrl+- / Ctrl+0
/// keys. Cut, Copy, Paste and Select All act on the focused text field, which this
/// menu is not, so they stay on their keys and on the right-click menus.
#[cfg(not(target_os = "macos"))]
fn app_menu_items(menu: ui::Menu) -> ui::Menu {
    use crate::native_menu as nm;
    menu.separator()
        // File
        .item(app_menu_item(
            "New conversation",
            Some("⌘N"),
            nm::NewConversation,
        ))
        .item(app_menu_item(
            "Save conversation as…",
            None,
            nm::SaveConversationAs,
        ))
        .separator()
        // Edit
        .item(app_menu_item("Find…", Some("⌘F"), nm::Find))
        // View
        .item(app_menu_item(
            "Command palette…",
            Some("⌘K"),
            nm::OpenCommandPalette,
        ))
        .separator()
        // Help
        .item(app_menu_item(
            format!("About Awili {}", env!("CARGO_PKG_VERSION")),
            None,
            nm::About,
        ))
        .separator()
        .item(app_menu_item("Quit Awili", Some("⌘Q"), nm::Quit))
}

#[cfg(all(test, not(target_os = "macos")))]
mod app_menu_tests {
    use super::*;

    /// Windows and Linux carry the File, Edit, View and Help commands in the
    /// in-window menu: the rows are there, in groups.
    #[test]
    fn the_in_window_app_menu_carries_the_menu_bar_commands() {
        let menu = app_menu_items(ui::Menu::new("test-app-menu"));
        // New, Save as, Find, Command palette, About, Quit (plus the rules).
        assert_eq!(menu.item_count(), 6);
    }
}
