//! S3: the shared pieces of an error card: the run id line with a Copy
//! button, the primary-action button, and what each action does.
use gpui::{div, prelude::*, AnyElement, Context, ElementId, Window};

use crate::{run_error::RunErrorAction, theme, ui, AwiliApp};

/// The command that signs the helper that RAN signed-out back in. Taken from
/// the failed run's helper identity, never from words in the failure text.
pub(crate) fn sign_in_command(engine: Option<awili_code::CodeEngine>) -> Option<&'static str> {
    match engine? {
        awili_code::CodeEngine::Claude => Some("claude"),
        awili_code::CodeEngine::Codex => Some("codex login"),
        _ => None,
    }
}

impl AwiliApp {
    /// "Run id abc12345" with a Copy button that uses the app's clipboard path.
    pub(crate) fn run_id_row(
        &self,
        id: impl Into<ElementId>,
        selector: &'static str,
        run_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let copy = run_id.to_string();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .child(ui::label(format!("Run id {run_id}")))
            .child(
                ui::Button::new(id, "Copy run id")
                    .selector(selector)
                    .quiet()
                    .on_click(cx.listener(move |app, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                        app.show_info_toast("Run id copied", cx);
                    })),
            )
            .into_any_element()
    }

    /// The button for a primary action other than Retry and Choose folder
    /// (those keep their own wiring where they apply).
    pub(crate) fn run_error_action_button(
        &self,
        id: impl Into<ElementId>,
        selector: &'static str,
        action: RunErrorAction,
        cause: &str,
        engine: Option<awili_code::CodeEngine>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let label = action.label()?;
        let cause = cause.to_string();
        Some(
            ui::Button::new(id, label)
                .selector(selector)
                .ghost()
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.run_error_action(action, &cause, engine, window, cx)
                }))
                .into_any_element(),
        )
    }

    /// Do the action's side effect. Retry and Choose folder are dispatched by
    /// the owning surface (they need its row or its folder picker).
    pub(crate) fn run_error_action(
        &mut self,
        action: RunErrorAction,
        cause: &str,
        engine: Option<awili_code::CodeEngine>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            RunErrorAction::SignIn => {
                let _ = cause;
                let steps = engine.and_then(|e| {
                    crate::agent_signin::sign_in_steps(e)
                        .map(|s| format!("{} is signed out. {s} Then press Retry.", e.label()))
                });
                let steps = steps.unwrap_or_else(|| {
                    "Sign the helper in again in Terminal, then press Retry.".into()
                });
                match sign_in_command(engine) {
                    Some(cmd) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(cmd.to_string()));
                        self.show_info_toast(format!("{steps} The command is copied."), cx);
                    }
                    None => self.show_info_toast(steps, cx),
                }
            }
            RunErrorAction::OpenSettings => self.open_settings(cx),
            RunErrorAction::TryAnotherModel => self.open_workshop_helper_menu(window, cx),
            RunErrorAction::Retry | RunErrorAction::ChooseFolder | RunErrorAction::None => {}
        }
    }
}

impl AwiliApp {
    /// The helper CLI the current Work run was requested on, when it runs one.
    pub(crate) fn work_error_engine(&self) -> Option<awili_code::CodeEngine> {
        match self.work.panel.requested_identity.as_ref()? {
            crate::work_run_identity::WorkRunIdentity::Agent { engine, .. } => Some(*engine),
            _ => None,
        }
    }
}

impl AwiliApp {
    /// S3: the card under a failed Code reply: the category's plain title,
    /// the cause, its primary action and the run id with Copy.
    pub(crate) fn code_error_card(
        &self,
        tab_id: u64,
        idx: usize,
        cause: &str,
        run_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let classified = crate::run_error::classify(cause);
        let engine = self
            .work
            .code_tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| t.engine);
        let mut card = ui::Card::new(("code-error-card", idx))
            .tone(theme::Tone::ActNow)
            .label(classified.category.title())
            .body(cause.to_string());
        let action = match classified.action {
            RunErrorAction::Retry => Some(
                ui::Button::new(("code-error-retry", idx), "Retry")
                    .selector("code-error-retry")
                    .ghost()
                    .on_click(cx.listener(|app, _, _, cx| app.retry_code(cx)))
                    .into_any_element(),
            ),
            RunErrorAction::SignIn
            | RunErrorAction::OpenSettings
            | RunErrorAction::TryAnotherModel => self.run_error_action_button(
                ("code-error-action", idx),
                "code-error-action",
                classified.action,
                cause,
                engine,
                cx,
            ),
            RunErrorAction::ChooseFolder | RunErrorAction::None => None,
        };
        let mut parts: Vec<AnyElement> = action.into_iter().collect();
        if !classified.category.user_can_fix() {
            parts.push(
                ui::label(crate::inline_error::NOT_FIXABLE_HERE)
                    .wrap()
                    .into_any_element(),
            );
        }
        parts.push(self.run_id_row(("code-run-id-copy", idx), "code-run-id-copy", run_id, cx));
        card = card.children(parts);
        div()
            .id(("code-error-presentation", idx))
            .child(card)
            .into_any_element()
    }
}
