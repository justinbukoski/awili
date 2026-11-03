//! M2e (S3-F04, S3-F07-running): the Code tab shows that it is running, Stop
//! settles the turn at once, and a late event from a stopped turn cannot
//! touch the tab's next turn. Each Code turn gets a sequence number; the
//! turn's async task acts only while its number is the tab's live turn.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use gpui::{div, prelude::*, AnyElement, Context, FontWeight, SharedString};

use crate::theme;
use crate::work::Mode;

/// One live Code turn: its sequence number and when it started.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CodeRun {
    pub(crate) seq: u64,
    pub(crate) started: Instant,
}

/// The live turn per Code tab id.
#[derive(Debug, Default)]
pub(crate) struct CodeRuns {
    next_seq: u64,
    by_tab: HashMap<u64, CodeRun>,
    /// RC1-K (tests only): the last turn task that ran its last step, and
    /// whether it dropped a late event (one of a turn no longer live).
    #[cfg(test)]
    last_ended: Option<(u64, bool)>,
}

impl CodeRuns {
    /// Start a turn on `tab_id`: a new sequence number that replaces any
    /// earlier entry for the tab.
    pub(crate) fn start(&mut self, tab_id: u64, now: Instant) -> u64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        self.by_tab.insert(tab_id, CodeRun { seq, started: now });
        seq
    }

    /// Whether `seq` is still `tab_id`'s live turn.
    pub(crate) fn live(&self, tab_id: u64, seq: u64) -> bool {
        self.by_tab.get(&tab_id).is_some_and(|r| r.seq == seq)
    }

    /// `tab_id`'s live turn's sequence number (tests read it).
    #[cfg(test)]
    pub(crate) fn seq(&self, tab_id: u64) -> Option<u64> {
        self.by_tab.get(&tab_id).map(|r| r.seq)
    }

    /// When `tab_id`'s live turn started.
    pub(crate) fn started(&self, tab_id: u64) -> Option<Instant> {
        self.by_tab.get(&tab_id).map(|r| r.started)
    }

    /// End `tab_id`'s live turn (no turn is live afterwards).
    pub(crate) fn end(&mut self, tab_id: u64) {
        self.by_tab.remove(&tab_id);
    }

    /// RC1-K: turn task `seq` ran its last step; `dropped_late` says it
    /// dropped a late event. Recorded for tests only, which wait on it instead
    /// of a wall-clock budget; nothing in the app reads it.
    pub(crate) fn task_ended(&mut self, seq: u64, dropped_late: bool) {
        #[cfg(test)]
        {
            self.last_ended = Some((seq, dropped_late));
        }
        let _ = (seq, dropped_late);
    }

    /// RC1-K: the last turn task that ran its last step, and whether it
    /// dropped a late event (tests read it).
    #[cfg(test)]
    pub(crate) fn last_ended(&self) -> Option<(u64, bool)> {
        self.last_ended
    }
}

/// The longest activity text the strip shows, in characters.
pub(crate) const ACTIVITY_MAX: usize = 60;

/// "● Running · GLM · 1m 05s · replies at the end" /
/// "● Running · Claude · sonnet · 5s · Edit src/lib.rs".
pub(crate) fn strip_label(
    engine: CodeEngine,
    model: Option<&str>,
    elapsed: Duration,
    last_activity: Option<&str>,
) -> String {
    let mut parts = vec![format!("● Running · {}", engine.label())];
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        parts.push(m.to_string());
    }
    parts.push(crate::run_footer::fmt_elapsed(elapsed));
    if engine.buffers_whole_output() {
        parts.push("replies at the end".to_string());
    } else if let Some(a) = last_activity.map(str::trim).filter(|a| !a.is_empty()) {
        if a.chars().count() > ACTIVITY_MAX {
            let cut: String = a.chars().take(ACTIVITY_MAX - 1).collect();
            parts.push(format!("{cut}…"));
        } else {
            parts.push(a.to_string());
        }
    }
    parts.join(" · ")
}

/// The toast for an Enter that cannot send because `mode` is busy.
pub(crate) fn busy_hint(mode: Mode) -> &'static str {
    match mode {
        Mode::Code => "Still running — press Stop (⌘.) or wait. Your message is still in the box.",
        Mode::Work => {
            "Work is still running — press Stop (⌘.) or wait. Your message is still in the box."
        }
        _ => "Still answering — press Stop (⌘.) or wait. Your message is still in the box.",
    }
}

impl crate::AwiliApp {
    /// Called by `submit_code_turn`: start the tab's turn, plus a 1 s ticker
    /// that repaints (the strip's elapsed time) while the turn is live and
    /// exits once it is not.
    pub(crate) fn start_code_run(&mut self, tab_id: u64, cx: &mut Context<Self>) -> u64 {
        let seq = self.work.code_runs.start(tab_id, Instant::now());
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let live = this
                .update(cx, |app, cx| {
                    let live = app.code_run_live(tab_id, seq);
                    if live {
                        cx.notify();
                    }
                    live
                })
                .unwrap_or(false);
            if !live {
                break;
            }
        })
        .detach();
        seq
    }

    /// Whether `seq` is still `tab_id`'s live Code turn.
    pub(crate) fn code_run_live(&self, tab_id: u64, seq: u64) -> bool {
        self.work.code_runs.live(tab_id, seq)
    }

    /// The turn task's last step: settle the tab only while `seq` is still
    /// its live turn. A Stop already settled it, and a newer turn must not be
    /// ended by an older turn's late finish.
    pub(crate) fn finish_code_turn_if_live(
        &mut self,
        tab_id: u64,
        seq: u64,
        cx: &mut Context<Self>,
    ) {
        if self.code_run_live(tab_id, seq) {
            self.finish_code_turn(tab_id, cx);
        }
    }

    /// The running strip for the active Code tab, or `None` when the tab is
    /// not streaming: what runs, for how long, the last activity, and Stop.
    pub(crate) fn code_run_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.work.code_tabs.get(self.work.code_active)?;
        if !session.streaming {
            return None;
        }
        let elapsed = self
            .work
            .code_runs
            .started(session.id)
            .map(|s| s.elapsed())
            .unwrap_or_default();
        let last_activity = session
            .rows
            .last()
            .and_then(|r| r.activity.last())
            .map(|a| a.to_string());
        let label = strip_label(
            session.engine,
            session.model.as_deref(),
            elapsed,
            last_activity.as_deref(),
        );
        let stop = div()
            .id("code-stop")
            .debug_selector(|| "code-stop".into())
            .flex_none()
            .px(theme::z(10.))
            .py(theme::z(3.))
            .rounded(theme::z(theme::RADIUS))
            .border_1()
            .bg(theme::p().chip_bg)
            .border_color(theme::p().chip_warn)
            .text_color(theme::p().chip_warn)
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .hover(|s| s.border_color(theme::p().accent))
            .child(crate::shortcuts::localize("■ Stop  ⌘.").into_owned())
            .tooltip(crate::icons::tip(
                crate::shortcuts::localize("Stop — \u{2318}. or Esc").into_owned(),
            ))
            .on_click(cx.listener(|this, _ev, _w, cx| this.cancel_code_turn(cx)));
        Some(
            div()
                .id("code-run-strip")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(theme::z(8.))
                .px(theme::z(12.))
                .py(theme::z(4.))
                .text_size(theme::z(theme::LABEL_SIZE + 1.0))
                .text_color(theme::p().muted)
                .child(SharedString::from(label))
                .child(stop)
                .into_any_element(),
        )
    }

    /// Enter while the composer is busy: say why nothing was sent.
    pub(crate) fn explain_busy_composer(&mut self, cx: &mut Context<Self>) {
        self.show_toast(busy_hint(self.mode), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m2e_runs_are_per_turn() {
        let mut runs = CodeRuns::default();
        let now = Instant::now();
        let a = runs.start(1, now);
        assert!(runs.live(1, a));
        let b = runs.start(1, now);
        assert_ne!(a, b);
        assert!(!runs.live(1, a), "a newer turn retires the older one");
        assert!(runs.live(1, b));
        let c = runs.start(2, now);
        assert!(runs.live(2, c), "tab 2 is independent");
        assert_eq!(runs.started(1), Some(now));
        runs.end(1);
        assert!(!runs.live(1, b));
        assert_eq!(runs.started(1), None);
        assert!(runs.live(2, c), "ending tab 1 leaves tab 2 alone");
    }

    #[test]
    fn m2e_strip_label_says_what_is_running() {
        assert_eq!(
            strip_label(CodeEngine::Glm, None, Duration::from_secs(65), None),
            "● Running · GLM · 1m 05s · replies at the end"
        );
        assert_eq!(
            strip_label(
                CodeEngine::Claude,
                Some("sonnet"),
                Duration::from_secs(5),
                Some("Edit src/lib.rs")
            ),
            "● Running · Claude · sonnet · 5s · Edit src/lib.rs"
        );
        let long = "x".repeat(ACTIVITY_MAX + 1);
        let label = strip_label(CodeEngine::Codex, None, Duration::from_secs(1), Some(&long));
        let shown = label.rsplit(" · ").next().unwrap();
        assert!(shown.ends_with('…'), "{label}");
        assert_eq!(shown.chars().count(), ACTIVITY_MAX, "{label}");
        assert_eq!(
            strip_label(CodeEngine::Codex, None, Duration::from_secs(1), None),
            "● Running · Codex · 1s"
        );
    }

    #[test]
    fn m2e_busy_hint_per_mode() {
        assert_eq!(
            busy_hint(Mode::Code),
            "Still running — press Stop (⌘.) or wait. Your message is still in the box."
        );
        assert_eq!(
            busy_hint(Mode::Work),
            "Work is still running — press Stop (⌘.) or wait. Your message is still in the box."
        );
    }
}

#[cfg(test)]
#[path = "code_run_tests.rs"]
mod code_run_tests;
