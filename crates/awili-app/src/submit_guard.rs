//! R1a — a typed message never vanishes (RC1 usability round 1, Blocker 1).
//!
//! The composer is emptied BEFORE the submit is routed (Chat, Work, Code), so
//! any path that returns early without recording the text used to drop it:
//! a Work folder that was moved away, a
//! prompt parked for a deleted conversation, a mode with no submit path. One
//! shared mechanism covers all of them: `submit_composer_draft` arms a
//! [`SubmitGuard`] holding the text, every path that TAKES the text clears
//! it ([`crate::AwiliApp::accept_submit`]: the user row is in a transcript, the
//! send is parked, or the text was handed back already), and whatever is
//! left after the dispatch goes back into the composer with the reason in a
//! toast that stays until it is clicked ([`crate::AwiliApp::settle_submit_guard`]).
//! A refusal outside a composer submit (an approved plan's follow-up, a
//! replay) still shows its reason ([`crate::AwiliApp::refuse_submit`]).

use gpui::Context;

/// The text of the composer submit being dispatched, and the reason a path
/// gave for not taking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitGuard {
    pub text: String,
    pub reason: Option<String>,
}

/// The fallback reason when a submit was not taken and no path said why.
pub(crate) const NOT_STARTED_NOTICE: &str = "Not sent \u{2014} Awili could not start this \
      request. Your message is still in the box; press Enter to try again.";

/// R1d: a Work turn refused by the memory gate (S5: the memory service
/// answered with rules Awili could not check). The message is already in the
/// transcript; the panel keeps S5's exact text.
pub(crate) const POLICY_REFUSED_NOTICE: &str = "Not sent \u{2014} Awili\u{2019}s memory service \
      answered with rules Awili could not check, so nothing ran, to stay safe. Your message is \
      in the conversation; try again in a few minutes. If it keeps happening, tell whoever set \
      up Awili.";

/// The folder the Work conversation points at is gone (moved, renamed,
/// deleted). Names the folder by its last part only (no path).
pub(crate) fn missing_folder_notice(name: &str) -> String {
    format!(
        "Not sent \u{2014} the folder \u{201C}{name}\u{201D} is no longer there (it may have \
         been moved or renamed). Your message is still in the box. Click \u{201C}Choose \
         folder\u{201D} to pick it again, then press Enter."
    )
}

/// Match this formatter's complete contract, including its named-folder slot.
pub(crate) fn is_missing_folder_notice(text: &str) -> bool {
    text.strip_prefix("Not sent — the folder “").is_some_and(|rest|
        rest.ends_with("” is no longer there (it may have been moved or renamed). Your message is still in the box. Click “Choose folder” to pick it again, then press Enter."))
}

/// RC1-M: the notice for a Work submit whose folder no longer exists (the
/// S1 folder-instructions read that used to catch it is gone with CLAUDE.md;
/// R1a's check stays). `None` when the folder is there.
pub(crate) fn work_folder_gone_notice(folder: Option<&std::path::Path>) -> Option<String> {
    let f = folder.filter(|f| !f.is_dir())?;
    let name = f
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "chosen".to_string());
    Some(missing_folder_notice(&name))
}

impl crate::AwiliApp {
    /// Arm the guard for one composer submit (`submit_composer_draft`).
    /// RC1-H7: sending again is the user acting, so whatever toast is up
    /// (a refusal included) closes; a new refusal shows its own.
    pub(crate) fn guard_submit(&mut self, text: &str) {
        self.toast.dismiss();
        self.submit_guard = Some(SubmitGuard {
            text: text.to_string(),
            reason: None,
        });
    }

    /// The submit's text was taken: a transcript row, a parked send, or a
    /// hand-back. No-op outside a composer submit.
    pub(crate) fn accept_submit(&mut self) {
        self.submit_guard = None;
    }

    /// A path refused the submit before recording it. Inside a composer
    /// submit the reason rides on the guard (shown when the text goes back);
    /// otherwise it is shown now, in a toast that stays until clicked.
    pub(crate) fn refuse_submit(&mut self, reason: String, cx: &mut Context<Self>) {
        match self.submit_guard.as_mut() {
            Some(guard) => guard.reason = Some(reason),
            None => self.show_sticky_toast(reason, cx),
        }
    }

    /// R1d: the plain S5 refusal, in a toast that stays.
    pub(crate) fn show_policy_refused_notice(&mut self, cx: &mut Context<Self>) {
        self.show_sticky_toast(POLICY_REFUSED_NOTICE, cx);
    }

    /// After the dispatch: a submit nothing took goes back into the composer
    /// (merged with anything typed since) and the reason stays on screen.
    pub(crate) fn settle_submit_guard(&mut self, cx: &mut Context<Self>) {
        let Some(guard) = self.submit_guard.take() else {
            return;
        };
        if guard.text.trim().is_empty() {
            return;
        }
        self.restore_draft(&guard.text, cx);
        let reason = guard
            .reason
            .unwrap_or_else(|| NOT_STARTED_NOTICE.to_string());
        self.show_sticky_toast(reason, cx);
    }
}

#[cfg(test)]
#[path = "submit_guard_tests.rs"]
mod tests;
