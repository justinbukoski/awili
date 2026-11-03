//! First-run connection onboarding — a pure state machine
//! (FIRST-RUN-SPEC-20260906).
//!
//! Deliberately dependency-free (`std` only) so the transition logic is
//! testable on any machine with
//!
//! ```sh
//! rustc --edition 2021 --test crates/awili-app/src/onboarding.rs
//! ```
//!
//! — no workspace, no GPUI, no app build. The app owns the *facts* this
//! machine consumes (a config file was actually written to disk; the
//! configured + discovered model cycle contains a usable non-embedding Chat
//! choice or a selected installed vendor CLI; a re-resolution of the ordered config paths found a file on
//! disk). This module owns the *decisions*: which welcome state to show, and
//! whether the explicit start action is permitted.
//!
//! Readiness requires a saved config with either a usable Chat choice or an
//! explicitly selected installed vendor CLI for Work/Code. Executable discovery
//! does not verify vendor authentication. The explicit Start chatting/Open Work
//! action alone completes a fresh setup; save and discovery never execute a run.
//! Existing valid-config launches remain authoritative, including offline or
//! provider-less configurations. There is no persistent onboarding flag.

/// The four setup states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No config file exists and none has been saved yet.
    Missing,
    /// A config was saved, but no Chat choice or selected installed CLI exists.
    SavedIncomplete,
    /// A config was saved with a usable Chat choice or selected installed CLI.
    /// Only the explicit start action completes a fresh setup.
    Ready,
    /// Setup is done: the user explicitly started chatting, or the app
    /// launched with an existing (loaded) config, which is authoritative
    /// regardless of its contents or connectivity.
    Complete,
}

impl State {
    /// Whether the first-run welcome owns the center for this state.
    pub fn is_active(self) -> bool {
        !matches!(self, State::Complete)
    }

    /// Whether the "Start chatting" action is offered (and permitted).
    pub fn is_ready(self) -> bool {
        matches!(self, State::Ready)
    }

    /// Whether setup is complete (welcome gone).
    pub fn is_complete(self) -> bool {
        matches!(self, State::Complete)
    }

    /// Whether a config file was actually saved in this session
    /// (`Missing` is the only pre-save state; `Complete` includes the
    /// existing-config launch, where the file of course exists).
    pub fn is_saved(self) -> bool {
        !matches!(self, State::Missing)
    }
}

/// The welcome state machine. In-memory only — see the module docs for why
/// no persistent flag exists.
#[derive(Clone, Debug)]
pub struct Onboarding {
    state: State,
}

impl Onboarding {
    /// A launch where the loader reported NO config file exists. The welcome
    /// is active in `Missing`.
    pub fn fresh() -> Self {
        Self {
            state: State::Missing,
        }
    }

    /// A launch with an EXISTING (loaded) config — or the invalid-config
    /// recovery path. An existing config is authoritative: no new-install
    /// onboarding at all, even for an empty, offline, or provider-less file.
    pub fn existing() -> Self {
        Self {
            state: State::Complete,
        }
    }

    /// The current state.
    #[allow(dead_code)] // exercised only by the onboarding tests
    pub fn state(&self) -> State {
        self.state
    }

    /// Whether the first-run welcome owns the center (all execution
    /// boundaries check this; configured launches never set it).
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// Whether "Start chatting" is offered and permitted.
    pub fn is_ready(&self) -> bool {
        self.state.is_ready()
    }

    /// Whether setup is complete (the welcome is gone).
    pub fn is_complete(&self) -> bool {
        self.state.is_complete()
    }

    /// Whether a config file exists or was saved (`Missing` is the only
    /// pre-save state).
    pub fn is_saved(&self) -> bool {
        self.state.is_saved()
    }

    /// A config file was ACTUALLY written (the first save, or a later save
    /// while the welcome is still up). `has_choice`: the saved document
    /// contains a usable Chat choice or an explicitly selected, installed vendor CLI.
    ///
    /// A save alone never completes setup — at best it reaches `Ready`. An
    /// unsaved document (the networkless seed before Save) is never reported
    /// through this method, so it can never count as a saved config. No-op
    /// once `Complete` (configured-launch saves are normal behaviour, not
    /// onboarding).
    pub fn on_saved(&mut self, has_choice: bool) {
        if !self.state.is_active() {
            return;
        }
        self.state = if has_choice {
            State::Ready
        } else {
            State::SavedIncomplete
        };
    }

    /// A discovery/merge pass finished. `has_choice`: a usable (enabled,
    /// non-embedding) Chat choice or an explicitly selected installed CLI exists.
    ///
    /// Discovery can promote `SavedIncomplete` → `Ready` (a saved endpoint's
    /// catalog supplies models) and revoke `Ready` → `SavedIncomplete` (a
    /// choice was lost, e.g. its provider was disabled or its merged models
    /// are gone). It never completes setup on its own, and does nothing in
    /// `Missing` — discovery is only offered for explicitly SAVED providers,
    /// and there are none before a save. No-op once `Complete`.
    pub fn on_discovery(&mut self, has_choice: bool) {
        match self.state {
            State::Missing | State::Complete => {}
            State::SavedIncomplete => {
                if has_choice {
                    self.state = State::Ready;
                }
            }
            State::Ready => {
                if !has_choice {
                    self.state = State::SavedIncomplete;
                }
            }
        }
    }

    /// "Check again": the app re-resolved the ordered candidate paths.
    /// `found_existing`: a config file now exists on disk.
    ///
    /// Only a genuine `Missing` state transitions: an existing config —
    /// externally created, or saved while the welcome was up but not yet
    /// observed — is PRESENTED (loaded for the runtime and kept, never
    /// overwritten) and ends new-install onboarding, even if it is empty,
    /// offline, or provider-less. A still-missing config stays `Missing`.
    /// In the saved states the file is the one this session wrote; the
    /// machine stays put (the app still re-probes on the caller's side).
    pub fn on_retry(&mut self, found_existing: bool) {
        if self.state == State::Missing && found_existing {
            self.state = State::Complete;
        }
    }

    /// "Check again" re-resolution found the file this session saved is GONE
    /// (`load_error` = false) or an invalid file now blocks the path
    /// (`load_error` = true): any Ready is revoked. A load error keeps the
    /// `SavedIncomplete` setup state (the caller shows its redacted
    /// actionable error); a vanished file returns to the fresh pre-save
    /// `Missing` state. No-op in `Missing` (nothing to revoke) and in
    /// `Complete` (an existing-config launch is authoritative and never
    /// re-enters setup).
    pub fn on_retry_lost(&mut self, load_error: bool) {
        if matches!(self.state, State::Missing | State::Complete) {
            return;
        }
        self.state = if load_error {
            State::SavedIncomplete
        } else {
            State::Missing
        };
    }

    /// The explicit "Start chatting" action — the ONLY completion path from
    /// the setup states. Permitted only from `Ready`: a save or a discovery
    /// result alone must never bypass it. Returns whether the transition
    /// happened (`false` = refused, state unchanged).
    pub fn start_chatting(&mut self) -> bool {
        if self.state == State::Ready {
            self.state = State::Complete;
            true
        } else {
            false
        }
    }
}

/// One saved endpoint's connection status, as the welcome displays it.
///
/// Honesty contract (spec §"Configured"): a probe success is *any* HTTP
/// answer — 200, 401, 500 alike — so [`ConnectionStatus::Reachable`] means
/// "the endpoint is there and answered", and must never be read as
/// authenticated, working, or a successful inference. A model-catalog probe
/// proves nothing about whether the model can serve chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// The provider is explicitly saved/configured, but no probe has been
    /// issued for this endpoint yet.
    Configured,
    /// A probe is in flight.
    Checking,
    /// The endpoint answered with ANY HTTP response — reachable, not proven
    /// working.
    Reachable,
    /// Transport failure: no HTTP answer at all.
    Unreachable,
}

impl ConnectionStatus {
    /// The word the welcome renders next to the endpoint name.
    pub fn label(self) -> &'static str {
        match self {
            ConnectionStatus::Configured => "configured",
            ConnectionStatus::Checking => "checking",
            ConnectionStatus::Reachable => "reachable",
            ConnectionStatus::Unreachable => "unreachable",
        }
    }
}

/// Map a saved endpoint's probe record to its displayed status.
///
/// `answered`: the completed probe's result — `true` for ANY HTTP response
/// (including 401: the point is that the server is THERE, not that it
/// authenticated us), `false` for a transport failure, `None` when no probe
/// has completed. `in_flight`: a probe for this endpoint was issued and has
/// not reported back. An in-flight retry always wins: a re-probe in progress
/// must display Checking even though an earlier probe already answered.
pub fn endpoint_status(answered: Option<bool>, in_flight: bool) -> ConnectionStatus {
    if in_flight {
        return ConnectionStatus::Checking;
    }
    match answered {
        Some(true) => ConnectionStatus::Reachable,
        Some(false) => ConnectionStatus::Unreachable,
        None => ConnectionStatus::Configured,
    }
}

/// The welcome's one-line status. "Ready" means Work can run (a chosen CLI,
/// the in-app engine or the gateway); saved provider rows alone leave it
/// unready and say what is missing.
pub fn welcome_subtext(
    ready: bool,
    saved: bool,
    cli_only: bool,
    has_providers: bool,
) -> &'static str {
    if ready && cli_only {
        "Your CLI is installed. Open Work, pick a folder, and submit a task. Sign-in is managed by the vendor CLI and has not been checked."
    } else if ready {
        "Your connection is configured. Start chatting when you are ready."
    } else if saved && has_providers {
        "Council can use your models. Work needs Claude Code or Codex installed and signed in, or a model server."
    } else if saved {
        "Settings saved. Choose an installed coding agent or add a model server for Work."
    } else {
        "Choose Claude Code, Codex, ZCode, or DeepSeek (dsh) for Work and Code, or connect a model for Chat."
    }
}

#[cfg(test)]
mod tests {
    /// Providers alone must not read as a working setup: the welcome says
    /// plainly what Work still needs.
    #[test]
    fn providers_only_is_not_ready_and_says_what_work_needs() {
        let text = welcome_subtext(false, true, false, true);
        assert!(text.contains("Council can use your models"), "{text}");
        assert!(text.contains("Claude Code or Codex"), "{text}");
        assert!(text.contains("model server"), "{text}");
        // Ready with a CLI keeps the sign-in caveat; no providers, no mention.
        assert!(welcome_subtext(true, true, true, false).contains("Sign-in is managed"));
        assert!(!welcome_subtext(false, true, false, false).contains("Council"));
    }

    use super::*;

    // ---- missing / existing launch -------------------------------------

    #[test]
    fn fresh_launch_is_missing_and_blocks_every_completion_action() {
        let mut onb = Onboarding::fresh();
        assert_eq!(onb.state(), State::Missing);
        assert!(onb.is_active());
        assert!(!onb.is_saved());
        assert!(!onb.is_complete());

        // Explicit continue is refused while nothing is saved.
        assert!(!onb.start_chatting());
        assert_eq!(onb.state(), State::Missing);
        // Discovery without a saved provider is not offered and changes
        // nothing (no choice must never produce Ready).
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Missing);
    }

    #[test]
    fn existing_launch_is_complete_even_for_empty_or_offline_configs() {
        // An existing file — empty, offline, provider-less — is an existing
        // configuration, not first run: the welcome never appears, and no
        // save/discovery/retry event drags it back into onboarding.
        let mut onb = Onboarding::existing();
        assert_eq!(onb.state(), State::Complete);
        assert!(onb.is_complete());
        assert!(!onb.is_active());

        onb.on_saved(false);
        onb.on_discovery(true);
        onb.on_retry(false);
        assert_eq!(onb.state(), State::Complete);
    }

    // ---- save -----------------------------------------------------------

    #[test]
    fn save_without_a_usable_choice_is_saved_incomplete() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(false);
        assert_eq!(onb.state(), State::SavedIncomplete);
        assert!(onb.is_saved());
        assert!(onb.is_active());

        // Still no choice: explicit continue is refused.
        assert!(!onb.start_chatting());
        assert_eq!(onb.state(), State::SavedIncomplete);
        // A discovery pass that finds nothing keeps the state (an enabled
        // endpoint awaiting discovery can remain here).
        onb.on_discovery(false);
        assert_eq!(onb.state(), State::SavedIncomplete);
    }

    #[test]
    fn save_with_a_usable_choice_reaches_ready_but_not_complete() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(true);
        assert_eq!(onb.state(), State::Ready);
        assert!(onb.is_ready());

        // A mere save does NOT bypass the explicit action.
        assert!(!onb.is_complete());
        // And discovery arriving with the choice still present does not
        // complete it either.
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Ready);
        assert!(!onb.is_complete());
    }

    // ---- discovery ------------------------------------------------------

    #[test]
    fn discovered_choice_promotes_saved_incomplete_to_ready() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(false);
        assert_eq!(onb.state(), State::SavedIncomplete);
        // The saved endpoint's catalog returns a non-embedding model.
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Ready);
        assert!(onb.is_ready());
        assert!(!onb.is_complete());
    }

    #[test]
    fn ready_is_revoked_by_a_lost_choice() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(true);
        assert_eq!(onb.state(), State::Ready);

        // The user re-saves with the provider disabled: the saved document
        // no longer offers a usable choice.
        onb.on_saved(false);
        assert_eq!(onb.state(), State::SavedIncomplete);
        assert!(!onb.is_ready());
        assert!(!onb.start_chatting());

        // Discovery revocation works the same way (e.g. a re-probe after a
        // config edit left no merged models behind).
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Ready);
        onb.on_discovery(false);
        assert_eq!(onb.state(), State::SavedIncomplete);
        assert!(!onb.start_chatting());

        // And the state can be regained, then completed explicitly.
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Ready);
        assert!(onb.start_chatting());
        assert_eq!(onb.state(), State::Complete);
    }

    // ---- retry / "Check again" ------------------------------------------

    #[test]
    fn retry_recognizes_an_externally_created_config() {
        let mut onb = Onboarding::fresh();
        // Still no file: the state is preserved.
        onb.on_retry(false);
        assert_eq!(onb.state(), State::Missing);
        // A config appeared on disk (external creation, or a save the
        // welcome had not observed): presented, not overwritten — the
        // machine completes without the explicit action, because an
        // existing config is authoritative.
        onb.on_retry(true);
        assert_eq!(onb.state(), State::Complete);
        assert!(onb.is_complete());
    }

    #[test]
    fn retry_is_a_noop_outside_the_missing_state() {
        // An existing-config launch: retrying changes nothing (it is
        // already authoritative-complete).
        let mut onb = Onboarding::existing();
        onb.on_retry(true);
        assert_eq!(onb.state(), State::Complete);

        // Saved states: the file on disk is the one this session wrote;
        // "Check again" re-probes but does not move the machine (the app
        // decides the loaded contents are its own).
        let mut onb = Onboarding::fresh();
        onb.on_saved(false);
        onb.on_retry(true);
        assert_eq!(onb.state(), State::SavedIncomplete);
        onb.on_saved(true);
        onb.on_retry(true);
        assert_eq!(onb.state(), State::Ready);
    }

    // ---- explicit continue ----------------------------------------------

    #[test]
    fn explicit_continue_is_only_permitted_from_ready() {
        // Missing.
        let mut onb = Onboarding::fresh();
        assert!(!onb.start_chatting());
        assert_eq!(onb.state(), State::Missing);

        // SavedIncomplete.
        let mut onb = Onboarding::fresh();
        onb.on_saved(false);
        assert!(!onb.start_chatting());
        assert_eq!(onb.state(), State::SavedIncomplete);

        // Ready → the only transition that completes.
        let mut onb = Onboarding::fresh();
        onb.on_saved(true);
        assert!(onb.start_chatting());
        assert_eq!(onb.state(), State::Complete);
        assert!(!onb.is_active());

        // Complete: already done; the action is a refused no-op.
        assert!(!onb.start_chatting());
        assert_eq!(onb.state(), State::Complete);
    }

    #[test]
    fn a_later_save_never_reopens_completed_onboarding() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(true);
        onb.start_chatting();
        assert_eq!(onb.state(), State::Complete);
        // Normal configured-user saves afterwards never drag the app back
        // into the welcome.
        onb.on_saved(false);
        onb.on_discovery(true);
        onb.on_retry(false);
        assert_eq!(onb.state(), State::Complete);
    }

    // ---- connection status honesty ---------------------------------------

    #[test]
    fn endpoint_status_maps_probe_records_honestly() {
        assert_eq!(
            endpoint_status(Some(true), false),
            ConnectionStatus::Reachable
        );
        assert_eq!(
            endpoint_status(Some(false), false),
            ConnectionStatus::Unreachable
        );
        assert_eq!(endpoint_status(None, true), ConnectionStatus::Checking);
        assert_eq!(endpoint_status(None, false), ConnectionStatus::Configured);
    }

    #[test]
    fn reachable_is_never_claimed_as_working() {
        // The displayed word is exactly "reachable" — the probe bool means
        // ANY HTTP response (including 401), so the status must not be
        // upgraded to authenticated/working/ok wording.
        let st = endpoint_status(Some(true), false);
        assert_eq!(st, ConnectionStatus::Reachable);
        assert_eq!(st.label(), "reachable");
        assert_ne!(st.label(), "working");
        assert_ne!(st.label(), "authenticated");
        assert_ne!(st.label(), "ok");
        // And the other three words are the configured/checking/unreachable
        // vocabulary.
        assert_eq!(ConnectionStatus::Configured.label(), "configured");
        assert_eq!(ConnectionStatus::Checking.label(), "checking");
        assert_eq!(ConnectionStatus::Unreachable.label(), "unreachable");
    }

    #[test]
    fn an_in_flight_retry_reports_checking_despite_a_stale_answer() {
        // A re-probe issued after a previous answer must not display the old
        // result while it is in flight.
        assert_eq!(
            endpoint_status(Some(true), true),
            ConnectionStatus::Checking
        );
        assert_eq!(
            endpoint_status(Some(false), true),
            ConnectionStatus::Checking
        );
        // And once it lands, the new verdict replaces the old one.
        assert_eq!(
            endpoint_status(Some(false), false),
            ConnectionStatus::Unreachable
        );
    }

    // ---- "Check again" after the saved config disappears / errors --------

    #[test]
    fn check_again_revokes_ready_when_the_saved_file_is_gone() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(true);
        assert_eq!(onb.state(), State::Ready);
        // The file vanished: Ready is revoked and the machine returns to the
        // fresh pre-save state — no stale Ready survives.
        onb.on_retry_lost(false);
        assert_eq!(onb.state(), State::Missing);
        assert!(!onb.is_ready());
        assert!(!onb.start_chatting());
        // And cached discovery must not be able to revive the lost choice.
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Missing);
        assert!(!onb.is_ready());
    }

    #[test]
    fn check_again_revoke_on_load_error_keeps_the_setup_state() {
        let mut onb = Onboarding::fresh();
        onb.on_saved(true);
        assert_eq!(onb.state(), State::Ready);
        // An invalid file now blocks the path: Ready is revoked down to the
        // SavedIncomplete setup state (the caller surfaces its redacted,
        // actionable error), and the explicit action stays refused.
        onb.on_retry_lost(true);
        assert_eq!(onb.state(), State::SavedIncomplete);
        assert!(!onb.is_ready());
        assert!(!onb.start_chatting());
        assert!(onb.is_active());
    }

    #[test]
    fn check_again_lost_is_a_noop_outside_the_saved_states() {
        // Fresh (nothing saved yet): a vanished or invalid file revokes
        // nothing — the app stays in the fresh setup state.
        let mut onb = Onboarding::fresh();
        onb.on_retry_lost(false);
        assert_eq!(onb.state(), State::Missing);
        onb.on_retry_lost(true);
        assert_eq!(onb.state(), State::Missing);
        // An existing-config launch is authoritative: never re-enters setup.
        let mut onb = Onboarding::existing();
        onb.on_retry_lost(false);
        assert_eq!(onb.state(), State::Complete);
        onb.on_retry_lost(true);
        assert_eq!(onb.state(), State::Complete);
        // SavedIncomplete + error stays SavedIncomplete (the error is the
        // caller's to display); the state can still be promoted normally.
        let mut onb = Onboarding::fresh();
        onb.on_saved(false);
        onb.on_retry_lost(true);
        assert_eq!(onb.state(), State::SavedIncomplete);
        onb.on_discovery(true);
        assert_eq!(onb.state(), State::Ready);
    }
}
