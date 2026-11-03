//! SXf: the Code page (design: the settings design notes).
//! The four coding helpers as status cards (installed or not, in words and a
//! mark), one setup guide and a Check again button each, and one shared
//! instruction instead of four copies. Which helper new project threads start
//! with is the catalog's `work.default_executor` row, listed here too.
use super::*;
use crate::ui::{text, Card};

/// One helper's install state: the path when found.
pub(crate) fn helper_found(index: usize, agents: &awili_config::AgentBinsConfig) -> Option<String> {
    let (engine, _, _) = harness_setup::RECIPES.get(index)?;
    harness_setup::installed_binary(*engine, agents)
}

impl AwiliApp {
    /// Keep the runtime copy of the new-thread preference current. Saving a
    /// default never changes an existing thread or the remembered last pick.
    pub(crate) fn after_settings_change(&mut self, id: &str) {
        if id != "work.default_executor" {
            return;
        }
        self.work.default_helper = self.saved_work_helper();
    }

    /// A new project thread starts with an explicitly configured helper when
    /// it parses; with no valid preference, the owner's remembered last pick
    /// remains the fallback. Neither path changes `last_pick`.
    pub(crate) fn start_on_default_helper(&mut self) {
        // RC1-O (pre-review 4): never while a run is live — its approvals are
        // routed by the chip; the new conversation gets its helper when the
        // run ends (`settle_work_chip`).
        if self.work.running || self.work.gateway_stop_pending {
            return;
        }
        let Some(helper) = self.starting_project_helper() else {
            return;
        };
        self.apply_starting_helper(helper);
    }

    /// Apply a new thread's already-resolved helper without recording it as a
    /// manual picker choice. Keep the same truthful Ask warning as the picker.
    pub(crate) fn apply_starting_helper(&mut self, helper: work::WorkExecutor) {
        let changed = self.work.executor != helper;
        self.work.executor = helper;
        if !changed {
            return;
        }
        // Seeding a thread from its default is not a manual picker choice:
        // keep the saved executor key (and therefore last_pick after restart)
        // intact. Preserve the Ask warning that a deliberate pick would show.
        let conv = crate::ask_mode::AskKey::Work(self.sidebar.active_id().to_string());
        if self.work.ask_modes.mode_for(&conv) != awili_work::ApprovalMode::AskBeforeChanges {
            return;
        }
        let support = crate::ask_mode::work_support(&self.work.executor, self.work_engine_kind());
        if matches!(support, crate::ask_mode::AskSupport::Unavailable(_)) {
            self.work.no_ask_notice = Some(crate::ask_mode::no_ask_toast(
                support,
                &self.work.executor.label(),
            ));
        }
    }

    /// Resolve the helper a newly created project thread should start with.
    /// An explicit Settings preference wins; absent or invalid values fall
    /// back to the owner's last helper pick.
    pub(crate) fn starting_project_helper(&self) -> Option<work::WorkExecutor> {
        self.work
            .default_helper
            .as_deref()
            .and_then(work::WorkExecutor::from_persist)
            .or_else(|| self.work.last_pick.clone())
    }

    /// SXf-FIX1: the default Work helper as the settings file holds it.
    pub(crate) fn saved_work_helper(&self) -> Option<String> {
        self.settings
            .doc
            .as_ref()
            .and_then(|doc| doc.config().work.default_executor)
    }

    /// RC2-R5M4 (slice review befb867d, P2): the sign-in state of the
    /// helper CLI at `path`, as far as it is known, without waiting on it:
    /// `None` while it is being checked. The first time (and after "Check
    /// again") the check starts off the UI thread, bounded by
    /// [`crate::agent_signin::STATUS_DEADLINE`]; its answer is remembered and
    /// the page is drawn again. Before, building these rows ran the CLI's
    /// status command on the UI thread with no deadline.
    pub(crate) fn helper_sign_in(
        &self,
        engine: awili_code::CodeEngine,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Option<crate::agent_signin::SignIn> {
        use crate::agent_signin as signin;
        if let Some(known) = signin::known_sign_in(engine, path) {
            return Some(known);
        }
        if signin::status_command(engine).is_none() {
            return Some(signin::SignIn::Unknown);
        }
        if signin::begin_sign_in_check(engine, path) {
            let bin = path.to_string();
            let check = cx.background_executor().spawn(async move {
                signin::sign_in_state(engine, &bin);
                signin::end_sign_in_check(engine, &bin);
            });
            cx.spawn(async move |this, cx| {
                check.await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        None
    }

    /// The Code page's own rows (above its catalog rows).
    pub(crate) fn settings_code_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let agents = self
            .settings
            .doc
            .as_ref()
            .map(|doc| doc.config().work.agents)
            .unwrap_or_default();
        let mut rows = vec![div()
            .px(theme::z(theme::SP_16))
            .pt(theme::z(theme::SP_8))
            .child(
                text::ui_text(
                    "Each helper is a program from its maker. Install it and sign in once in \
                     Terminal, then come back and press Check again. No key is needed here.",
                )
                .wrap(),
            )
            .into_any_element()];
        for (i, (engine, name, _)) in harness_setup::RECIPES.iter().enumerate() {
            let found = helper_found(i, &agents);
            // RC2-R57 (usability RC2 round 4: "✓ Installed" while Claude Code
            // was signed out): the CLI's own sign-in state, asked without a
            // model call. RC2-R5M4: asked off the UI thread; `None` while
            // the answer is on its way.
            // RC2-R610: ZCode and the DeepSeek Harness work with a key in a
            // file: whether it is there.
            let home = std::path::PathBuf::from(crate::home::home_os().unwrap_or_default());
            let dsh_env = agents.dsh_env_file();
            let key = crate::agent_signin::key_state(*engine, &home, &dsh_env);
            let sign_in = match key {
                Some(k) if found.is_some() => Some(k),
                _ => found
                    .as_deref()
                    .and_then(|path| self.helper_sign_in(*engine, path, cx)),
            };
            let signed_out_text = match crate::agent_signin::key_steps(*engine, &dsh_env) {
                Some(steps) => format!("Installed, but its key is not set. {steps}"),
                None => format!(
                    "Installed, but signed out. {}",
                    crate::agent_signin::sign_in_steps(*engine).unwrap_or("")
                ),
            };
            let signed_in_text = if key.is_some() {
                "\u{2713} Installed, and its key is set"
            } else {
                "\u{2713} Installed and signed in"
            };
            let status = match &found {
                Some(path) if sign_in == Some(crate::agent_signin::SignIn::SignedOut) => div()
                    .debug_selector(move || format!("sx-helper-ok-{i}"))
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_4))
                    .child(
                        div()
                            .debug_selector(move || format!("sx-helper-signed-out-{i}"))
                            .child(
                                text::ui_text(SharedString::from(signed_out_text))
                                    .wrap()
                                    .id(("sx-helper-status", i))
                                    .text_color_override(theme::p().chip_warn),
                            ),
                    )
                    .child(
                        text::caption(SharedString::from(path.clone())).id(("sx-helper-path", i)),
                    ),
                Some(path) => div()
                    .debug_selector(move || format!("sx-helper-ok-{i}"))
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_4))
                    .child(
                        div()
                            .when(sign_in.is_none(), |d| {
                                d.debug_selector(move || format!("sx-helper-checking-{i}"))
                            })
                            // RC2-R610: signed in, or its key set.
                            .when(
                                sign_in == Some(crate::agent_signin::SignIn::SignedIn),
                                |d| d.debug_selector(move || format!("sx-helper-signed-in-{i}")),
                            )
                            .child(
                                text::ui_text(match sign_in {
                                    Some(crate::agent_signin::SignIn::SignedIn) => signed_in_text,
                                    // RC2-R5M4: the sign-in check is on its way.
                                    None => "✓ Installed \u{00B7} checking sign-in\u{2026}",
                                    _ => "✓ Installed",
                                })
                                .wrap()
                                .id(("sx-helper-status", i))
                                .text_color_override(theme::p().accent_text),
                            ),
                    )
                    .child(
                        text::caption(SharedString::from(path.clone())).id(("sx-helper-path", i)),
                    ),
                None => div()
                    .debug_selector(move || format!("sx-helper-missing-{i}"))
                    .child(text::ui_text("Not found").text_color_override(theme::p().chip_warn)),
            };
            // RC2-R65 (usability RC2 round 5, Minor 8: "Check again" showed no
            // feedback): when it was pressed.
            let checked = self.checked_at.get(&code_check_key(i)).map(|at| {
                text::caption(SharedString::from(crate::hermes_pages::checked_line(at)))
                    .id(("rc2r65-code-checked", i))
                    .into_any_element()
            });
            let actions = div()
                .flex()
                .flex_wrap()
                .gap(theme::z(theme::SP_8))
                .child(
                    self.sx_button(("sx-guide", i), "Setup guide", false, cx, move |_, _| {
                        let _ = open::that_detached(harness_setup::GUIDES[i].1);
                    }),
                )
                .child(self.sx_button(
                    ("sx-check", i),
                    "Check again",
                    false,
                    cx,
                    move |this, cx| {
                        // RC2-R57: ask the CLIs about their sign-in anew.
                        crate::agent_signin::forget_sign_in();
                        this.checked_at
                            .insert(code_check_key(i), crate::hermes_pages::check_time());
                        cx.notify()
                    },
                ));
            let mut parts = vec![
                text::body(*name)
                    .strong()
                    .id(("sx-helper-name", i))
                    .into_any_element(),
                status.into_any_element(),
            ];
            parts.extend(checked);
            parts.push(
                text::label(harness_setup::GUIDES[i].0)
                    .dim()
                    .wrap()
                    .into_any_element(),
            );
            parts.push(actions.into_any_element());
            rows.push(
                div()
                    .px(theme::z(theme::SP_16))
                    .pt(theme::z(theme::SP_12))
                    .child(Card::new(("sx-helper-card", i)).children(parts))
                    .into_any_element(),
            );
        }
        rows
    }
}

/// RC2-R65: the [`crate::AwiliApp::checked_at`] key of helper row `i`'s Check again.
pub(crate) fn code_check_key(i: usize) -> String {
    format!("code:{i}")
}

#[cfg(test)]
#[path = "settings_code_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "work_default_helper_tests.rs"]
mod default_helper_tests;
