//! RC2-CNCL7: who sits on a council — the "Add or remove" sheet (Council
//! plan §3 decision 2, mockup step "pick models").
//!
//! Every model Awili can reach is listed, grouped by where it runs and what
//! it costs; a model that cannot answer right now shows why and cannot be
//! ticked; nothing else is hidden or limited (no cap on members). Saved
//! groups are one tap. Members change between messages; a new member reads
//! the whole council so far the next time it is asked.

use gpui::{div, prelude::*, AnyElement, Context, ScrollHandle, SharedString, Window};

use crate::council_view::{group_to_json, CouncilPage, SavedGroup};
use crate::council_view::{GROUPS_KEY, LAST_GROUP_KEY};
use crate::executor_labels::{council_pick_groups, CouncilPick, CouncilReach};
use crate::persistence::PersistCmd;
use crate::{theme, AwiliApp};

/// `debug_selector`s.
pub(crate) const ADD_REMOVE_SELECTOR: &str = "council-add-remove";
pub(crate) const PICK_DONE_SELECTOR: &str = "council-pick-done";
pub(crate) const PICK_CANCEL_SELECTOR: &str = "council-pick-cancel";
pub(crate) const SAVE_GROUP_SELECTOR: &str = "council-save-group";
/// Council picker face/desktop host width, in pre-zoom logical pixels.
const COUNCIL_PICKER_WIDTH: f32 = 520.0;

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_PICKER_SCROLL: std::cell::RefCell<Option<ScrollHandle>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn pick_selector(route: &str) -> String {
    format!("council-pick-{route}")
}

pub(crate) fn group_selector(i: usize) -> String {
    format!("council-group-{i}")
}

/// A saved group's name when the owner saves it: its members' names.
pub(crate) fn group_name(labels: &[String]) -> String {
    let joined = labels.join(", ");
    if joined.chars().count() <= 48 {
        joined
    } else {
        format!("{} and {} more", labels[0], labels.len() - 1)
    }
}

fn groups_from_json(raw: &str) -> Option<Vec<SavedGroup>> {
    serde_json::from_str(raw).ok()
}

impl AwiliApp {
    /// What the picker knows about what Awili can reach right now.
    pub(crate) fn council_reach(&self) -> CouncilReach<'_> {
        CouncilReach {
            engine: &self.work.engine,
            extras: &self.work.extra_engines,
            models: &self.work.models_cfg,
            agents: &self.work.agents,
            fleet: &self.council_fleet,
            local_down: matches!(
                self.work.local.health.reach(),
                crate::gateway_health::Reach::Down(_)
            ),
            choices: &self.choices,
            provider_health: &self.provider_health,
        }
    }

    /// Every pickable model, in groups.
    pub(crate) fn council_picks(&self) -> Vec<(&'static str, Vec<CouncilPick>)> {
        council_pick_groups(&self.council_reach())
    }

    fn council_pick(&self, route: &str) -> Option<CouncilPick> {
        self.council_picks()
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .find(|p| p.route == route)
    }

    /// The saved groups (read once from `app_state`).
    pub(crate) fn council_saved_groups(&mut self) -> Vec<SavedGroup> {
        if self.council_groups.is_none() {
            self.council_groups = Some(
                self.search_store
                    .as_ref()
                    .and_then(|s| s.get_app_state(GROUPS_KEY).ok().flatten())
                    .and_then(|raw| groups_from_json(&raw))
                    .unwrap_or_default(),
            );
        }
        self.council_groups.clone().unwrap_or_default()
    }

    /// "Add or remove": open the sheet with the present members ticked.
    /// Members change between messages, so it stays shut while the council
    /// is answering.
    pub(crate) fn council_open_picker(&mut self, cx: &mut Context<Self>) {
        let _ = self.council_saved_groups();
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        if page.council.is_running() {
            return;
        }
        page.picker = Some(
            page.council
                .active_members()
                .iter()
                .map(|m| m.route.clone())
                .collect(),
        );
        cx.notify();
    }

    /// Tick or untick one model. A model that cannot answer right now
    /// cannot be ticked (it can always be unticked).
    pub(crate) fn council_toggle_pick(&mut self, route: &str, cx: &mut Context<Self>) {
        let offline = self.council_pick(route).is_none_or(|p| p.offline.is_some());
        let conv = self.sidebar.active_id().to_string();
        let Some(picked) = self.councils.get_mut(&conv).and_then(|p| p.picker.as_mut()) else {
            return;
        };
        if let Some(i) = picked.iter().position(|r| r == route) {
            picked.remove(i);
        } else if !offline {
            picked.push(route.to_string());
        }
        cx.notify();
    }

    /// One tap on a saved group ticks exactly its members.
    pub(crate) fn council_use_group(&mut self, i: usize, cx: &mut Context<Self>) {
        let groups = self.council_saved_groups();
        let Some(g) = groups.get(i) else {
            return;
        };
        let conv = self.sidebar.active_id().to_string();
        if let Some(picked) = self.councils.get_mut(&conv).and_then(|p| p.picker.as_mut()) {
            *picked = g.members.iter().map(|(r, _)| r.clone()).collect();
        }
        cx.notify();
    }

    /// "+ Save this group": keep the ticked models as a group.
    pub(crate) fn council_save_group(&mut self, cx: &mut Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        let Some(picked) = self
            .councils
            .get(&conv)
            .and_then(|p| p.picker.clone())
            .filter(|p| !p.is_empty())
        else {
            return;
        };
        let members: Vec<(String, String)> = picked
            .iter()
            .map(|r| {
                (
                    r.clone(),
                    crate::council_view::route_label(
                        r,
                        &self.work.engine,
                        &self.work.extra_engines,
                    ),
                )
            })
            .collect();
        let mut groups = self.council_saved_groups();
        if groups.iter().any(|g| g.members == members) {
            return;
        }
        let labels: Vec<String> = members.iter().map(|(_, l)| l.clone()).collect();
        groups.push(SavedGroup {
            name: group_name(&labels),
            members,
        });
        if let (Some(p), Ok(json)) = (&self.persist, serde_json::to_string(&groups)) {
            p.set_state(GROUPS_KEY, json);
        }
        self.council_groups = Some(groups);
        cx.notify();
    }

    pub(crate) fn council_close_picker(&mut self, cx: &mut Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        if let Some(page) = self.councils.get_mut(&conv) {
            page.picker = None;
        }
        cx.notify();
    }

    /// "Use these N": seat the ticked models, take the unticked off (their
    /// answers stay), and remember the group for the next new council.
    pub(crate) fn council_apply_picker(&mut self, cx: &mut Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        let engine = self.work.engine.clone();
        let extras = self.work.extra_engines.clone();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        let Some(picked) = page.picker.take() else {
            return;
        };
        let now = crate::feed::unix_now();
        let present: Vec<(String, String)> = page
            .council
            .active_members()
            .iter()
            .map(|m| (m.id.clone(), m.route.clone()))
            .collect();
        let mut rows = Vec::new();
        for (id, route) in &present {
            if !picked.contains(route) && page.council.remove_member(id) {
                rows.push(id.clone());
            }
        }
        for route in &picked {
            if !present.iter().any(|(_, r)| r == route) {
                let label = crate::council_view::route_label(route, &engine, &extras);
                rows.push(page.council.add_member(route, &label));
            }
        }
        let group: Vec<(String, String)> = page
            .council
            .active_members()
            .iter()
            .map(|m| (m.route.clone(), m.label.clone()))
            .collect();
        let changed: Vec<awili_council::Member> = rows
            .iter()
            .filter_map(|id| page.council.member(id).cloned())
            .collect();
        if let Some(p) = &self.persist {
            for m in changed {
                p.send(PersistCmd::CouncilMember(awili_persist::CouncilMemberRow {
                    conversation_id: conv.clone(),
                    member_id: m.id.clone(),
                    position: i64::from(m.position),
                    route: m.route.clone(),
                    label: m.label.clone(),
                    added_ts: now,
                    removed_ts: m.removed.then_some(now),
                }));
            }
            if !group.is_empty() {
                p.set_state(LAST_GROUP_KEY, group_to_json(&group));
            }
        }
        if !group.is_empty() {
            self.council_last_group = Some(group);
        }
        // RC2-R3M4 (RC2-R3 review P3: Claude ticked after the council opened
        // started cold on its first question): the members changed, so the
        // processes made ready for the old ones end (a removed member's, and
        // the others', whose brief named the old members) and the members
        // now seated are made ready as when the council opened.
        if !rows.is_empty() {
            crate::codex_app_server::end_ready_for(&conv);
            self.council_prewarm(&conv);
        }
        cx.notify();
    }

    /// The bar's "+ Add or remove" (dim while the council is answering).
    pub(crate) fn council_add_remove_button(
        &self,
        page: Option<&CouncilPage>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = page.is_some_and(|p| p.council.is_running());
        crate::ui::Button::new(ADD_REMOVE_SELECTOR, "+ Add or remove")
            .selector(ADD_REMOVE_SELECTOR)
            .quiet()
            .disabled_if(running.then(|| {
                SharedString::from("Council members cannot change while responses are running")
            }))
            .on_click(cx.listener(|this, _ev, _window, cx| this.council_open_picker(cx)))
            .into_any_element()
    }

    fn council_pick_row(
        &self,
        p: &CouncilPick,
        ticked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let route = p.route.clone();
        let sel = pick_selector(&p.route);
        let offline = p.offline.clone();
        let trailing = if let Some(reason) = offline.as_ref() {
            crate::ui::text::label("Unavailable")
                .id(format!("{sel}-availability"))
                .full_text(reason.clone())
                .into_any_element()
        } else {
            crate::ui::text::label(p.cost.clone()).into_any_element()
        };
        let mut row = crate::ui::Row::new(SharedString::from(sel.clone()))
            .title(p.label.clone())
            .subtitle(p.note.clone())
            .icon(if ticked {
                crate::icons::Icon::Check
            } else {
                crate::icons::Icon::Circle
            })
            .trailing(trailing)
            .selected(ticked)
            .wrap();
        if let Some(reason) = offline.filter(|_| !ticked) {
            row = row.disabled(SharedString::from(reason));
        } else {
            row = row.on_click(
                cx.listener(move |this, _ev, _window, cx| this.council_toggle_pick(&route, cx)),
            );
        }
        div()
            .id(SharedString::from(sel.clone()))
            .debug_selector(move || sel.clone())
            .w_full()
            .min_w_0()
            .child(row)
            .into_any_element()
    }

    /// The "Who's on this council?" sheet, over the page.
    pub(crate) fn council_picker_sheet(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picked = self.active_council()?.picker.clone()?;
        let groups = self.council_groups.clone().unwrap_or_default();
        let scroll = window
            .use_keyed_state("council-picker-scroll", cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        #[cfg(test)]
        TEST_PICKER_SCROLL.with(|probe| *probe.borrow_mut() = Some(scroll.clone()));
        let scrollbar = window
            .use_keyed_state("council-picker-scrollbar", cx, |_, _| {
                crate::ui::scrollbar::ScrollBarState::default()
            })
            .read(cx)
            .clone();
        let mut saved = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .w_full()
            .min_w_0()
            .gap(theme::z(theme::SP_4));
        for (i, g) in groups.iter().enumerate() {
            let on = g.members.iter().map(|(r, _)| r).eq(picked.iter());
            let sel = group_selector(i);
            let mut chip = crate::ui::Chip::new(
                SharedString::from(format!("council-group-chip-{i}")),
                g.name.clone(),
            )
            .tone(if on {
                theme::Tone::Contrast
            } else {
                theme::Tone::Neutral
            })
            .on_click(cx.listener(move |this, _ev, _window, cx| this.council_use_group(i, cx)));
            if on {
                chip = chip.icon(crate::icons::Icon::Check);
            }
            saved = saved.child(
                div()
                    .id(SharedString::from(sel.clone()))
                    .debug_selector(move || sel.clone())
                    .min_w_0()
                    .max_w_full()
                    .child(chip),
            );
        }
        let save = crate::ui::Button::new(SAVE_GROUP_SELECTOR, "+ Save this group")
            .selector(SAVE_GROUP_SELECTOR)
            .quiet()
            .disabled_if(
                (picked.is_empty())
                    .then(|| SharedString::from("Choose at least one model before saving a group")),
            )
            .on_click(cx.listener(|this, _ev, _window, cx| this.council_save_group(cx)));
        saved = saved.child(save);
        let mut list_content = div()
            .id("council-picker-list-content")
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(theme::z(theme::SP_8));
        list_content = list_content.child(
            div().w_full().min_w_0().child(
                crate::ui::text::label(
                    "Every model you can use in Chat or Work. Pick as many as you like.",
                )
                .wrap(),
            ),
        );
        list_content = list_content.child(
            div()
                .id("council-picker-saved-groups")
                .w_full()
                .min_w_0()
                .child(saved),
        );
        for (group_index, (header, rows)) in self.council_picks().into_iter().enumerate() {
            let mut g = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .gap(theme::z(theme::SP_4))
                .child(
                    crate::ui::Row::new(format!("council-picker-section-{group_index}"))
                        .title(header)
                        .strong(),
                );
            for p in &rows {
                g = g.child(self.council_pick_row(p, picked.contains(&p.route), cx));
            }
            list_content = list_content.child(g);
        }
        let n = picked.len();
        let footer = div().flex().flex_col().w_full().min_w_0().child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .gap(theme::z(theme::SP_8))
                .child(
                    crate::ui::Button::new(PICK_CANCEL_SELECTOR, "Cancel")
                        .selector(PICK_CANCEL_SELECTOR)
                        .ghost()
                        .full_width()
                        .on_click(
                            cx.listener(|this, _ev, _window, cx| this.council_close_picker(cx)),
                        ),
                )
                .child(
                    crate::ui::Button::new(PICK_DONE_SELECTOR, format!("Use these {n}"))
                        .selector(PICK_DONE_SELECTOR)
                        .primary()
                        .full_width()
                        .on_click(
                            cx.listener(|this, _ev, _window, cx| this.council_apply_picker(cx)),
                        ),
                ),
        );
        let list = div()
            .id("council-picker-list")
            .debug_selector(|| "council-picker-list".to_string())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(crate::ui::panel::scroll_content(list_content))
            .child(
                crate::ui::scrollbar::ScrollBar::new("council-picker-scrollbar", scroll, scrollbar)
                    .label("Scroll available council models"),
            );
        let body = div().flex().flex_col().flex_1().min_h_0().child(list);
        let panel = crate::ui::Panel::new("council-picker")
            .width(COUNCIL_PICKER_WIDTH)
            .header("Who’s on this council?", None)
            .body_fill(body)
            .full_height()
            .on_close(cx.listener(|this, _ev, _window, cx| this.council_close_picker(cx)));
        if crate::ui::sheet::is_compact(window) {
            // Panel's compact size_full.relative host must be laid out as an
            // absolute full-window overlay, or it shrinks the Council column.
            Some(
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .child(panel.footer(footer))
                    .into_any_element(),
            )
        } else {
            let max_height =
                window.viewport_size().height - theme::z(theme::EDGE) - theme::z(theme::EDGE);
            Some(
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .id("council-picker-scrim")
                    .debug_selector(|| "council-picker-scrim".to_string())
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::p().scrim)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.council_close_picker(cx)))
                    .child(
                        div()
                            .w(theme::z(COUNCIL_PICKER_WIDTH))
                            .h(max_height)
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .child(panel.footer(footer)),
                    )
                    .into_any_element(),
            )
        }
    }
}
