//! N1 glue (`docs/daily-driver/specs/N1-macos-notifications.md` §2.3): builds
//! events from app state at the hook points, runs the kernel's actions on the
//! backend, drains backend signals, and routes clicks. Events carry only
//! Awili-owned labels: never a prompt, command, env var name or error text.

use std::time::Instant;

use awili_config::NotificationsConfig;
use awili_core::ConvKind;
use awili_work::run_events::RunStatus;
use gpui::Context;

use crate::notifications::{
    classify_work_end, parse_route, route_id, Action, Event, EventKind, Notifier, Permission,
    Suppressed, Target,
};
use crate::notify_platform::{signals, BackendSignal, NotifyBackend};
use crate::work::{CredentialKind, Mode};

/// The app's notification state: the kernel plus the backend it drives.
pub(crate) struct NotifyState {
    pub(crate) core: Notifier,
    backend: Box<dyn NotifyBackend>,
    /// Stable origin for per-run dedupe keys (`run_started` as nanoseconds).
    epoch: Instant,
    /// RC2-FIXB4: a Settings test waits for macOS's answer.
    test_waiting: bool,
    /// RC2-R64: how often macOS was asked for the current permission.
    #[cfg(test)]
    pub(crate) queries: usize,
}

/// RC2-FIXB4 (RC2-UX review P2): what a Settings test notification did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestOutcome {
    /// Posted to macOS.
    Shown,
    /// Posted through the Script Editor fallback.
    ShownByScriptEditor,
    /// macOS is being asked whether Awili may notify; the test waits.
    Asking,
    /// macOS has Awili's notifications turned off.
    Refused,
    /// Nothing could post it here (no native notifications, fallback off).
    NotShown,
}

/// What a test's decision (`result`, the kernel's actions or why it held the
/// event back) did, given the `permission` after it.
pub(crate) fn test_outcome(
    result: &Result<Vec<Action>, Suppressed>,
    permission: &Permission,
) -> TestOutcome {
    let test_id = route_id(&Target::Test);
    for action in result.iter().flatten() {
        match action {
            Action::PostNative(n) if n.id == test_id => return TestOutcome::Shown,
            Action::PostFallback(n) if n.id == test_id => return TestOutcome::ShownByScriptEditor,
            _ => {}
        }
    }
    match permission {
        Permission::Unknown | Permission::Requesting => TestOutcome::Asking,
        Permission::Denied => TestOutcome::Refused,
        Permission::Authorized | Permission::Unavailable(_) => TestOutcome::NotShown,
    }
}

impl NotifyState {
    /// From the loaded config (so a saved `enabled = false` holds from the
    /// first frame), with the backend's launch-time permission.
    pub(crate) fn new(cfg: NotificationsConfig) -> Self {
        let backend = crate::notify_platform::default_backend();
        let mut core = Notifier::new(cfg);
        core.set_initial_permission(backend.initial_permission());
        Self {
            core,
            backend,
            epoch: Instant::now(),
            test_waiting: false,
            #[cfg(test)]
            queries: 0,
        }
    }

    pub(crate) fn observe_window(&mut self, active: bool, now: Instant) {
        self.core.observe_window(active, now);
    }

    pub(crate) fn set_config(&mut self, cfg: NotificationsConfig) {
        self.core.set_config(cfg);
    }

    pub(crate) fn note_user_stop(&mut self, t: Target, now: Instant) {
        self.core.note_user_stop(t, now);
    }

    pub(crate) fn status_line(&self) -> &'static str {
        self.core.status_line()
    }

    /// RC2-R64: ask macOS for Awili's current permission (no prompt).
    pub(crate) fn query_permission(&mut self) {
        #[cfg(test)]
        {
            self.queries += 1;
        }
        self.backend.query_permission();
    }

    /// Everything the test backend recorded so far.
    #[cfg(test)]
    pub(crate) fn test_log(&self) -> Vec<crate::notify_platform::Recorded> {
        self.backend
            .recorded()
            .expect("a test build dispatches through the recording backend")
    }

    fn run(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::RequestPermission => self.backend.request_permission(),
                Action::PostNative(n) => self.backend.post_native(&n),
                Action::PostFallback(n) => self.backend.post_fallback(&n),
            }
        }
    }

    /// A per-run key part: `run_started` relative to this state's epoch.
    fn run_key(&self, started: Option<Instant>) -> String {
        match started {
            None => "none".to_string(),
            Some(t) => match t.checked_duration_since(self.epoch) {
                Some(d) => d.as_nanos().to_string(),
                None => format!("-{}", self.epoch.duration_since(t).as_nanos()),
            },
        }
    }
}

impl crate::AwiliApp {
    /// Hand one event to the kernel and run what it decides.
    fn notify_event(&mut self, ev: Event) {
        let visible = self.notify_visible();
        let idle = self.notifier.backend.idle_secs();
        if let Ok(actions) = self
            .notifier
            .core
            .handle(ev, visible.as_ref(), idle, Instant::now())
        {
            self.notifier.run(actions);
        }
    }

    /// The surface on screen (the "visible" target of §1).
    fn notify_visible(&self) -> Option<Target> {
        match self.mode {
            Mode::Work => Some(Target::Work(self.sidebar.active_id().to_string())),
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .map(|s| Target::Code(s.id)),
            _ => None,
        }
    }

    /// The Work card the agent is waiting on, as a key part; `None` when
    /// neither an approval nor a credential card is up.
    pub(crate) fn notify_waiting_key(&self) -> Option<String> {
        if let Some(a) = &self.work.panel.approval {
            return Some(if a.approval_id.is_empty() {
                format!("approval:{}:{}", self.work.run_seq, a.kind)
            } else {
                format!("approval:{}", a.approval_id)
            });
        }
        self.work
            .panel
            .credential
            .as_ref()
            .map(|c| format!("cred:{}", c.request_id))
    }

    /// End of `apply_work_update`: a newly raised approval, question or
    /// credential card notifies (reads only `approval_id`, `kind`, the typed
    /// `risk` class and `request_id`; never `prompt` or `env_var`).
    /// RC2-R69: an approval says which helper wants to do what kind of step,
    /// where ([`approval_step`]), not only "w1 · command".
    pub(crate) fn notify_work_waiting(&mut self, before: Option<String>) {
        let Some(key) = self.notify_waiting_key() else {
            return;
        };
        if before.as_deref() == Some(key.as_str()) {
            return;
        }
        let kind = if let Some(a) = &self.work.panel.approval {
            if a.kind == "clarify" {
                EventKind::Question
            } else {
                EventKind::Approval {
                    kind: format!(
                        "{} wants to {}",
                        self.notify_helper(),
                        approval_step(a.risk, self.notify_work_folder().as_deref())
                    ),
                }
            }
        } else if let Some(c) = &self.work.panel.credential {
            EventKind::Credential {
                sudo: c.kind == CredentialKind::Sudo,
            }
        } else {
            return;
        };
        let conv = self.work_conv_id();
        self.notify_event(Event {
            target: Target::Work(conv.clone()),
            kind,
            key: format!("work:{conv}:{key}"),
            label: self.notify_work_label(&conv),
            detail: None,
            elapsed: None,
        });
    }

    /// The shared end of every Work run (gateway, agent executor, local
    /// build): one event per run, classified from the panel. A Stop the owner
    /// pressed is silenced by the kernel's user-stop mark.
    pub(crate) fn notify_work_end(&mut self) {
        let Some(kind) = classify_work_end(
            self.work.panel.status,
            self.work.panel.error.is_some(),
            false,
        ) else {
            return;
        };
        let conv = self.work_conv_id();
        let run = self.notifier.run_key(self.work.run_started);
        self.notify_event(Event {
            target: Target::Work(conv.clone()),
            kind,
            key: format!("work:{conv}:end:{}:{run}", self.work.run_seq),
            label: self.notify_work_label(&conv),
            detail: Some(self.notify_work_detail()),
            elapsed: self.work.run_started.map(|t| t.elapsed()),
        });
    }

    /// The Stop button on Work: the next Stopped/Failed on this conversation
    /// is the owner's own doing.
    pub(crate) fn notify_work_user_stop(&mut self) {
        let conv = self.work_conv_id();
        self.notifier
            .note_user_stop(Target::Work(conv), Instant::now());
    }

    /// RC2-R69 (usability RC2 round 5, Minor 6: "Work finished" said only
    /// "x1"): the conversation's own title (what was asked) when it has
    /// one, else the folder basename, else "Work session".
    fn notify_work_label(&self, conv: &str) -> String {
        let title = self
            .conversations
            .get(conv)
            .map(|c| c.title.clone())
            .filter(|t| !t.trim().is_empty() && t != awili_core::convs::DEFAULT_TITLE);
        title
            .or_else(|| self.notify_work_folder())
            .unwrap_or_else(|| "Work session".to_string())
    }

    /// RC2-R69: the Work folder's basename, if one is bound.
    fn notify_work_folder(&self) -> Option<String> {
        self.work
            .folder
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
    }

    /// RC2-R69: the helper doing the work, by its own label ("Gemma 4 26B
    /// (Studio)", "Codex", "Fleet gateway").
    fn notify_helper(&self) -> String {
        match &self.work.executor {
            crate::work::WorkExecutor::InApp => self.work.engine.display_label(),
            other => crate::executor_labels::chip_label(
                other,
                &self.work.engine,
                &self.work.extra_engines,
                &self.work.build_targets,
            ),
        }
    }

    /// RC2-R69: a finished run's second part: the helper, and the folder
    /// when the first part is the title ("Codex in x1").
    fn notify_work_detail(&self) -> String {
        let helper = self.notify_helper();
        let conv = self.work_conv_id();
        match self.notify_work_folder() {
            Some(folder) if self.notify_work_label(&conv) != folder => {
                format!("{helper} in {folder}")
            }
            _ => helper,
        }
    }

    /// Top of `finish_code_turn` (before it clears `stream_abort_tx`): a
    /// streaming turn that the owner did not cancel notifies.
    pub(crate) fn notify_code_turn_end(&mut self, tab_id: u64) {
        let Some(s) = self.work.code_tabs.iter().find(|s| s.id == tab_id) else {
            return;
        };
        if !s.streaming {
            return;
        }
        let user_stopped = s.stream_abort_tx.is_none();
        let errored = s
            .rows
            .len()
            .checked_sub(1)
            .is_some_and(|i| s.errored.contains(&i));
        let key = format!("code:{tab_id}:{}", s.rows.len());
        let detail = Some(s.engine.label().to_string());
        let status = user_stopped.then_some(RunStatus::Stopped);
        let Some(kind) = classify_work_end(status, errored, user_stopped) else {
            return;
        };
        let label = self
            .active_code_project()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Code".to_string());
        self.notify_event(Event {
            target: Target::Code(tab_id),
            kind,
            key,
            label,
            detail,
            elapsed: None,
        });
    }

    /// End of `AwiliApp::new`: drain backend signals on the UI thread until
    /// the entity is gone. (The launch-time permission is set in
    /// [`NotifyState::new`]; nothing here prompts.)
    pub(crate) fn start_notifications(&mut self, cx: &mut Context<Self>) {
        // RC2-R64: the real permission from the start, not "will ask".
        self.notifier.query_permission();
        let rx = signals().1.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(sig) = rx.recv().await {
                if this
                    .update(cx, |app, cx| app.on_notify_signal(sig, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// One backend signal.
    pub(crate) fn on_notify_signal(&mut self, sig: BackendSignal, cx: &mut Context<Self>) {
        match sig {
            BackendSignal::Permission(p) => {
                let actions = self.notifier.core.permission_changed(p);
                // RC2-FIXB4: macOS answered while a test waited — say what
                // became of it.
                let follow = self
                    .notifier
                    .test_waiting
                    .then(|| test_outcome(&Ok(actions.clone()), self.notifier.core.permission()))
                    .filter(|o| *o != TestOutcome::Asking);
                self.notifier.run(actions);
                if let Some(outcome) = follow {
                    self.notifier.test_waiting = false;
                    self.show_info_toast(crate::settings_pages::test_notice(outcome), cx);
                }
            }
            // RC2-R64: what macOS allows now.
            BackendSignal::Current(p) => {
                let actions = self.notifier.core.current_permission(p);
                self.notifier.run(actions);
            }
            BackendSignal::PostFailed(n) => {
                // RC2-FIXB4: a test macOS could not show says so.
                let test = n.id == route_id(&Target::Test);
                let actions = self.notifier.core.post_failed(n);
                let follow = test
                    .then(|| test_outcome(&Ok(actions.clone()), self.notifier.core.permission()));
                self.notifier.run(actions);
                if let Some(outcome) = follow {
                    self.show_info_toast(crate::settings_pages::test_notice(outcome), cx);
                }
            }
            BackendSignal::PostRefused(n) => {
                // RC2-FIXC2 (RC2-FIXB review P2: the toast still said "sent"
                // after macOS refused the test): a refused post is the
                // owner's denial; a refused test says so, whenever it comes.
                let actions = self.notifier.core.permission_changed(Permission::Denied);
                self.notifier.run(actions);
                if n.id == route_id(&Target::Test) {
                    self.notifier.test_waiting = false;
                    self.show_info_toast(
                        crate::settings_pages::test_notice(TestOutcome::Refused),
                        cx,
                    );
                }
            }
            BackendSignal::Clicked(id) => self.route_notification_click(&id, cx),
        }
        cx.notify();
    }

    /// A click: bring Awili forward, then navigate to the route. Navigation
    /// only: a click never answers an approval, sends text or starts a run.
    fn route_notification_click(&mut self, id: &str, cx: &mut Context<Self>) {
        cx.activate(true);
        if self.onboarding.is_active() {
            return;
        }
        let Some(target) = parse_route(id) else {
            return;
        };
        match target {
            Target::Work(conv) => {
                if self
                    .conversations
                    .get(&conv)
                    .is_some_and(|c| c.kind == ConvKind::Work)
                {
                    self.open_conversation_from_history(&conv, cx);
                } else {
                    self.set_mode(Mode::Work, cx);
                }
            }
            Target::Code(tab) => {
                self.set_mode(Mode::Code, cx);
                if let Some(i) = self.work.code_tabs.iter().position(|s| s.id == tab) {
                    self.work.code_active = i;
                }
            }
            Target::Test => {}
        }
    }

    /// Settings "send test": always asks macOS again unless already allowed
    /// or unavailable, then posts the event (ignores toggles and gates).
    ///
    /// RC2-FIXB4 (RC2-UX review P2: Settings said "sent" while macOS was
    /// still being asked): returns what the press actually did; while macOS
    /// is asked, its answer later says what became of the test.
    pub(crate) fn notify_send_test(&mut self) -> TestOutcome {
        self.notifier.core.rearm();
        let visible = self.notify_visible();
        let idle = self.notifier.backend.idle_secs();
        let result = self.notifier.core.handle(
            Event {
                target: Target::Test,
                kind: EventKind::Test,
                key: "test".to_string(),
                label: String::new(),
                detail: None,
                elapsed: None,
            },
            visible.as_ref(),
            idle,
            Instant::now(),
        );
        let outcome = test_outcome(&result, self.notifier.core.permission());
        if let Ok(actions) = result {
            self.notifier.run(actions);
        }
        self.notifier.test_waiting = outcome == TestOutcome::Asking;
        outcome
    }

    /// Settings "open System Settings" (shown when permission is Denied).
    pub(crate) fn notify_open_system_settings(&self) {
        self.notifier.backend.open_system_settings();
    }
}

/// RC2-R69 (usability RC2 round 5, Minor 6): an approval's step in plain
/// words, from its typed risk class (Awili's own words, never the agent's),
/// with the folder it is in.
pub(crate) fn approval_step(
    risk: awili_work::run_events::ApprovalRisk,
    folder: Option<&str>,
) -> String {
    use awili_work::run_events::ApprovalRisk;
    let what = match risk {
        ApprovalRisk::ReadOnly => "read something",
        ApprovalRisk::Mutating => "make a change",
        ApprovalRisk::Irreversible => "make a change that can't be undone",
        ApprovalRisk::Privileged => "run a step that needs your OK",
    };
    match folder {
        Some(f) => format!("{what} in {f}"),
        None => what.to_string(),
    }
}

#[cfg(test)]
#[path = "notify_hooks_tests.rs"]
mod tests;
