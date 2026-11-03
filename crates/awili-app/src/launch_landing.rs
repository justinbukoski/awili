//! R2g — where a cold launch lands (RC1 round 2, R-A1 "NOT FIXED": two cold
//! launches "both landed on Work with an old conversation open … The Code
//! tab I left was not restored", and a long transcript opened scrolled to
//! the top).
//!
//! A cold launch returns to the main mode the user left (Chat, Work or
//! Code, remembered in `app_state` under [`LAST_MODE_KEY`] by every mode
//! switch). With none saved yet (the first launch after this change), the
//! old rule stays: a CLI-only install opens Work, anything else Chat. Work
//! always opens on a fresh conversation (a never-sent one is reused, so no
//! empty rows pile up); the older conversations are one click away at the
//! top of the sidebar, and nothing in them changes.

use awili_core::ConvKind;
use gpui::Context;

use crate::persistence::LAST_MODE_KEY;
use crate::work::Mode;

/// The `app_state` value for a main mode; `None` for the utility modes
/// (Skills, Cron, Sessions, …), which a launch never returns to.
pub(crate) fn mode_key(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Work => Some("work"),
        Mode::Code => Some("code"),
        Mode::Council => Some("council"),
        _ => None,
    }
}

/// The main mode a saved value names; anything else is `None`.
pub(crate) fn mode_from_key(raw: &str) -> Option<Mode> {
    match raw {
        // Legacy saved value: the answer-only surface is gone.
        "chat" => Some(Mode::Work),
        "work" => Some(Mode::Work),
        "code" => Some(Mode::Code),
        "council" => Some(Mode::Council),
        _ => None,
    }
}

impl crate::AwiliApp {
    /// Remember `mode` for the next cold launch (main modes only).
    pub(crate) fn remember_last_mode(&mut self, mode: Mode) {
        if let (Some(key), Some(p)) = (mode_key(mode), &self.persist) {
            // Keep the legacy adapter as a fallback for an absent/unreadable
            // snapshot. A valid Workshop snapshot restores its exact UUID
            // before this landing path, independently of this compatibility key.
            p.set_state(LAST_MODE_KEY, key.to_string());
        }
    }

    /// The end of `AwiliApp::new`: go to the saved main mode (else Work for
    /// a CLI-only install, else Chat), and in Work open a fresh conversation.
    /// Onboarding keeps Chat (`set_mode` refuses the others then).
    pub(crate) fn land_on_launch(
        &mut self,
        saved: Option<&str>,
        _cli_only: bool,
        cx: &mut Context<Self>,
    ) {
        let target = saved.and_then(mode_from_key).unwrap_or(Mode::Work);
        if target != self.mode {
            self.set_mode(target, cx);
        } else if target == Mode::Work {
            // The app opens on Work, so `set_mode` is a no-op: run the page's
            // entry work (gateway refresh, local model check, placeholder).
            self.enter_work_surface(cx);
        }
        // Work opens on a fresh conversation once a main mode has been
        // chosen; with none saved yet, the conversation last open returns.
        if self.mode == Mode::Work && saved.and_then(mode_from_key).is_some() {
            self.open_fresh_work_conversation();
        }
    }

    /// Select a fresh Work conversation without a window (no focus change):
    /// the launch twin of "+ New conversation". It runs before `new` restores
    /// the active conversation's last run, so the fresh conversation opens
    /// with a clean panel; opening the old one restores its last run.
    pub(crate) fn open_fresh_work_conversation(&mut self) {
        let (id, reused) = self.fresh_conversation(ConvKind::Work);
        self.set_active_conversation(&id);
        self.remember_active_kind();
        self.save_active_conversation();
        self.staged_attachments.clear();
        if reused {
            self.refresh_staged_attachments();
        }
    }
}

#[cfg(test)]
#[path = "launch_landing_tests.rs"]
mod tests;
