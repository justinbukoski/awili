//! The Workshop job face owns presentation state only. Workers keep running
//! when the face closes, a tab changes, or the owner navigates elsewhere.
use gpui::{div, prelude::*, AnyElement, Context, Window};

use crate::{consolidation, icons::Icon, theme, ui, work::Mode, AwiliApp};

/// Dock only when the remaining Workshop can hold a readable editor plus
/// all five possible controls (including Stop reading aloud). Hit targets
/// retain their 44-point floor when zoomed out. Other panels keep their
/// normal compact policy.
pub(crate) fn uses_sheet(window: &Window) -> bool {
    uses_sheet_at(window.bounds().size.width)
}

pub(crate) fn uses_sheet_at(width: gpui::Pixels) -> bool {
    let remaining = width - theme::z(theme::STAGE_LEFT + theme::PANEL_W + theme::EDGE);
    width < theme::z(theme::COMPACT_W) || remaining < theme::job_composer_min_width()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum JobTab {
    #[default]
    History,
    Terminal,
    Previews,
    Context,
}

impl JobTab {
    pub(crate) const ALL: [Self; 4] =
        [Self::History, Self::Terminal, Self::Previews, Self::Context];

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap()
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::History => "History",
            Self::Terminal => "Terminal",
            Self::Previews => "Previews",
            Self::Context => "Context",
        }
    }
}

impl AwiliApp {
    pub(crate) fn show_job_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != Mode::Work {
            return;
        }
        self.shell.inspector.selection = None;
        self.shell.job_open = true;
        if uses_sheet(window) {
            self.shell.conversations = false;
        }
        window.focus(&self.shell.job_focus, cx);
        cx.notify();
    }

    pub(crate) fn toggle_job_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Work {
            if !self.shell.job_open {
                self.show_job_panel(window, cx);
            } else {
                self.shell.job_open = false;
                self.focus_shell_page(window, cx);
            }
        } else {
            self.shell.composer_details = !self.shell.composer_details;
            self.focus_shell_page(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_job_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.job_open = false;
        self.focus_shell_page(window, cx);
        cx.notify();
    }

    /// A running Work job remains discoverable across Workshop conversations.
    /// Stop is offered on its owner; another conversation offers Go. Completed
    /// successful runs leave the capsule. A current owned error stays reachable
    /// through Review until it is resolved or replaced by a new accepted run.
    ///
    /// The capsule is the one run indicator in the top bar. It carries only the
    /// capsule itself: the bar reserves its fixed-width slot, so starting or
    /// ending a run moves nothing. `level` says how much fits.
    pub(crate) fn job_activity_island(
        &self,
        level: BarLevel,
        cx: &mut Context<Self>,
    ) -> Option<(AnyElement, bool)> {
        if !consolidation::workshop_mode(self.mode) || self.shell.archived_reader.is_some() {
            return None;
        }
        let owner = self
            .current_work_error()
            .map(|e| &e.owner)
            .or(self.work.conv_id.as_ref())?;
        let blocked = self.work.panel.approval.is_some()
            || self.work.panel.credential.is_some()
            || self
                .work
                .plan_gates
                .open_for(&crate::ask_mode::AskKey::Work(owner.clone()))
                .is_some();
        let error = (!blocked)
            .then(|| self.current_work_error().cloned())
            .flatten();
        if !self.work.running && !blocked && error.is_none() {
            return None;
        }
        let owns_run = self.mode == Mode::Work && self.sidebar.active_id() == owner;
        let title = self
            .conversations
            .get(owner)
            .map(|c| c.title.as_str())
            .unwrap_or("another conversation");
        let label = if let Some(error) = &error {
            let state = if self.work_error_is_setup(error) {
                "Needs you"
            } else {
                "Job failed"
            };
            if error.owner == self.sidebar.active_id() {
                state.to_string()
            } else {
                format!("{state} in {title}")
            }
        } else {
            match (owns_run, blocked) {
                (true, true) => "Needs you".to_string(),
                (true, false) => "Running".to_string(),
                (false, true) => format!("Waiting in {title}"),
                (false, false) => format!("Running in {title}"),
            }
        };
        let error_tone = error.as_ref().map(|e| self.work_error_tone(e));
        let has_error = error.is_some();
        let running_now = !blocked && !has_error && self.work.running;
        // "1:05" beside the word Running; the plan's step and the plan-first
        // posture when the run has a plan and the bar is wide enough.
        let clock = running_now.then(|| {
            fmt_clock(
                self.work
                    .run_started
                    .map(|started| started.elapsed())
                    .unwrap_or_default(),
            )
        });
        let step = (owns_run && running_now)
            .then(|| self.plan_step_text())
            .flatten();
        let plan_first = owns_run && running_now && step.is_some() && self.owner_is_plan_first();
        let tooltip = {
            let mut parts = vec![label.clone()];
            parts.extend(clock.clone());
            parts.extend(step.clone());
            if plan_first {
                parts.push("Plan first".into());
            }
            parts.join(" \u{b7} ")
        };
        let action_label = if blocked || has_error {
            "Review"
        } else if owns_run {
            "Job"
        } else {
            "Go"
        };
        let owner = owner.clone();
        let open = {
            let error = error.clone();
            cx.listener(move |app, _: &gpui::ClickEvent, window, cx| {
                if error.as_ref().is_some_and(|e| !app.work_error_matches(e)) {
                    return;
                }
                app.select_conversation(&owner, window, cx);
                if blocked || has_error {
                    app.shell.more = false;
                    app.shell.job_open = false;
                    app.shell.conversations = false;
                    if has_error {
                        app.focus_work_error(window, cx);
                    } else {
                        app.focus_work_blocking_card(window, cx);
                    }
                }
                if !blocked && !has_error {
                    app.show_job_panel(window, cx);
                }
                cx.notify();
            })
        };
        let narrow = level == BarLevel::Narrow;
        let action: AnyElement = if narrow {
            let bounds = self.shell.tabs.header_action_bounds[5].clone();
            div()
                .flex_none()
                .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
                .child(
                    ui::IconButton::new("job-island-open", Icon::Layers)
                        .selector("ui-button-job-island-open")
                        .label(action_label)
                        .tooltip(action_label)
                        .focus_handle(&self.shell.tabs.header_action_focus[5])
                        .on_click(open),
                )
                .into_any_element()
        } else {
            ui::Button::new("job-island-open", action_label)
                .quiet()
                .focus_handle(&self.shell.tabs.header_action_focus[5])
                .bounds_handle(self.shell.tabs.header_action_bounds[5].clone())
                .on_click(open)
                .into_any_element()
        };
        let stop = (owns_run && running_now).then(|| {
            let bounds = self.shell.tabs.header_action_bounds[6].clone();
            div()
                .flex_none()
                .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
                .child(
                    ui::IconButton::new("job-island-stop", Icon::Square)
                        .selector("ui-button-job-island-stop")
                        .label("Stop")
                        .tooltip("Stop \u{2014} \u{2318}.")
                        .focus_handle(&self.shell.tabs.header_action_focus[6])
                        .on_click(cx.listener(|app, _, _, cx| app.cancel_work_run(cx))),
                )
                .into_any_element()
        });
        let has_stop = stop.is_some();
        let mut capsule = ui::Pill::new("job-activity-island")
            .island()
            .bounded()
            .scroll_through()
            .tooltip(tooltip)
            .child(
                ui::Dot::new(if let Some(tone) = error_tone {
                    tone
                } else if blocked {
                    theme::Tone::ActNow
                } else {
                    theme::Tone::Neutral
                })
                .id("job-island-state")
                .label(label.clone()),
            );
        if !narrow || clock.is_none() {
            // The label gives way first: a long cross-conversation title ends in
            // an ellipsis (the tooltip carries it whole).
            capsule = capsule.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(ui::text::label(label).id("job-island-label")),
            );
        }
        if let Some(clock) = clock {
            capsule = capsule.child(ui::text::caption(clock).id("job-island-clock"));
        }
        // The step and posture only where the slot is sized for them.
        if level == BarLevel::Wide {
            if let Some(step) = step {
                let text = if plan_first {
                    format!("{step} \u{b7} Plan first")
                } else {
                    step
                };
                capsule = capsule.child(
                    div()
                        .flex_none()
                        .child(ui::text::caption(text).id("job-island-step")),
                );
            }
        }
        capsule = capsule.child(action);
        if let Some(stop) = stop {
            capsule = capsule.child(stop);
        }
        Some((capsule.into_any_element(), has_stop))
    }

    /// "step 2 of 4" for the running plan, from the Progress checklist.
    fn plan_step_text(&self) -> Option<String> {
        let steps = &self.work.panel.steps;
        let running = steps
            .iter()
            .position(|s| s.state == crate::work::StepState::Running)?;
        Some(format!("step {} of {}", running + 1, steps.len()))
    }

    /// Whether the owner's next turn is plan-first (the mode chip's "Plan first").
    fn owner_is_plan_first(&self) -> bool {
        let key = self.work_chip_key();
        self.next_ask(&key).is_some_and(|next| {
            crate::ask_mode::effective(self.work.ask_modes.mode_for(&key), next.support)
                == crate::ask_mode::Effective::AskPlanFirst
        })
    }
}

/// How much of the top bar fits: every label, the short forms, or icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarLevel {
    /// Wide: full labels, the plan step and posture in the run capsule (and a
    /// slot sized for them).
    Wide,
    /// Below 1400 px of bar: full labels, no step or posture in the capsule.
    Mid,
    /// Below 1180 px of bar: project to its icon, model to its short name, mode
    /// to "Plan"/"Auto", New to "+", the capsule to a dot and the timer.
    Narrow,
}

impl BarLevel {
    /// The run capsule's reserved slot at this level, in logical px.
    pub(crate) fn slot_w(self) -> f32 {
        match self {
            Self::Wide => theme::RUN_SLOT_WIDE_W,
            Self::Mid => theme::RUN_SLOT_W,
            Self::Narrow => theme::RUN_SLOT_NARROW_W,
        }
    }

    pub(crate) fn for_width(width: gpui::Pixels) -> Self {
        if width < theme::z(theme::BAR_NARROW_W) {
            Self::Narrow
        } else if width < theme::z(theme::BAR_WIDE_W) {
            Self::Mid
        } else {
            Self::Wide
        }
    }
}

/// "0:42", "12:05", "1:02:03": the capsule's elapsed time.
pub(crate) fn fmt_clock(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clock_reads_minutes_and_seconds() {
        assert_eq!(fmt_clock(Duration::from_secs(0)), "0:00");
        assert_eq!(fmt_clock(Duration::from_secs(42)), "0:42");
        assert_eq!(fmt_clock(Duration::from_secs(725)), "12:05");
        assert_eq!(fmt_clock(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn bar_level_follows_the_window_width() {
        crate::theme::set_ui_scale(1.0);
        assert_eq!(BarLevel::for_width(gpui::px(1179.)), BarLevel::Narrow);
        assert_eq!(BarLevel::for_width(gpui::px(1180.)), BarLevel::Mid);
        assert_eq!(BarLevel::for_width(gpui::px(1399.)), BarLevel::Mid);
        assert_eq!(BarLevel::for_width(gpui::px(1400.)), BarLevel::Wide);
    }
}
