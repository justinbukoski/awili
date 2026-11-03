//! The hold-to-talk (push-to-talk) hold guard (U3b2 D2), as a pure state
//! machine the UI drives, plus the small [`crate::AwiliApp`] methods the
//! machine's outcomes call into.
//!
//! GPUI reports the PTT key as an *aggregate modifier flag* (see
//! [`awili_voice::PttKey::modifier`]), so a physical *tap* of the key (down,
//! then up before any typing) must never start a dictation: the tap is a
//! keystroke the user is about to make, not a held intention to speak. The
//! guard delays the start by [`HOLD`]: the modifier going down only *arms* a
//! timer; the key reaching a focus target while still held disarms it (the
//! user is typing with the modifier); only the timer firing — the key is
//! still held and no key was typed — actually opens the session.
//!
//! [`PttGuard`] is stateless with respect to the app: all of its inputs are
//! the boolean edges the UI already computes, so the decision table is
//! unit-testable without a window. The UI owns the live state
//! (`dictating` / `ptt_session`) and the composer; the guard only decides.

use std::time::Duration;

use gpui::Context;

use crate::AwiliApp;

/// How long the PTT modifier must be held before a dictation starts. A tap
/// (release within this window) is treated as a keystroke, not a hold.
pub const HOLD: Duration = Duration::from_millis(300);

/// What the guard wants the UI to do after one input edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttAction {
    /// Nothing to do.
    None,
    /// Start (or re-arm) the [`HOLD`] timer; fire it under generation
    /// `u64` so a stale firing after a release is a no-op.
    ArmTimer(u64),
    /// The hold is confirmed and no key was typed: open the session.
    Start,
    /// The hold ended while dictating: end the session (and insert).
    Stop,
    /// A key was typed while holding: cancel the live PTT session without
    /// committing (the user switched to typing).
    Cancel,
}

/// The internal phase of the hold guard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// The PTT modifier is not held.
    #[default]
    Idle,
    /// The modifier is held, and the [`HOLD`] timer is armed under `gen`.
    Armed { gen: u64 },
    /// The timer fired: a PTT dictation session is live.
    Active,
}

/// The pure hold-to-talk state machine (U3b2 D2). See the module docs for the
/// timing contract; every transition is table-driven and covered by the
/// `u3b2_` tests below.
#[derive(Clone, Debug, Default)]
pub struct PttGuard {
    phase: Phase,
    next_gen: u64,
}

impl PttGuard {
    /// The PTT modifier changed: `held == true` on the down edge, `false` on
    /// the up edge.
    ///
    /// - Idle + down → Armed: arm the [`HOLD`] timer (a tap may be coming).
    /// - Armed + up → Idle: a tap is not a hold — it never dictated.
    /// - Active + up → Idle: the hold ended; stop the session (insert).
    pub fn modifier(&mut self, held: bool) -> PttAction {
        match (&self.phase, held) {
            (Phase::Idle, true) => {
                self.next_gen += 1;
                self.phase = Phase::Armed { gen: self.next_gen };
                PttAction::ArmTimer(self.next_gen)
            }
            (Phase::Armed { .. }, false) => {
                self.phase = Phase::Idle;
                PttAction::None
            }
            (Phase::Active, false) => {
                self.phase = Phase::Idle;
                PttAction::Stop
            }
            _ => PttAction::None,
        }
    }

    /// The armed [`HOLD`] timer fired. A matching generation confirms the
    /// hold (→ [`PttAction::Start`]); a stale generation (the key was released
    /// first) is a no-op.
    pub fn timer(&mut self, gen: u64) -> PttAction {
        match self.phase {
            Phase::Armed { gen: g } if g == gen => {
                self.phase = Phase::Active;
                PttAction::Start
            }
            _ => PttAction::None,
        }
    }

    /// A key reached a focus target. While the modifier is held (Armed or
    /// Active) the user is typing with it, so the hold is *not* a dictation
    /// intention: disarm, and cancel the session if it had already started.
    /// A key while Idle is unrelated.
    pub fn key_down(&mut self) -> PttAction {
        match self.phase {
            Phase::Armed { .. } => {
                self.phase = Phase::Idle;
                PttAction::None
            }
            Phase::Active => {
                self.phase = Phase::Idle;
                PttAction::Cancel
            }
            Phase::Idle => PttAction::None,
        }
    }
}

/// The `AwiliApp` hooks the guard's outcomes call into (U3b2: the guard owns
/// *when*, the app owns *what* — starting, stopping and cancelling
/// dictation).
impl AwiliApp {
    /// The armed [`HOLD`] timer fired with generation `gen`: confirm the hold
    /// and open the dictation session.
    pub fn ptt_timer(&mut self, gen: u64, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        match self.voice.ptt_guard.timer(gen) {
            PttAction::Start => {
                // Keep the edge flag in sync: held while the session is live.
                self.voice.ptt_held = true;
                if !self.voice.dictating {
                    self.voice.ptt_session = true;
                    self.start_dictation(cx);
                }
                cx.notify();
            }
            PttAction::None => {
                // Stale generation: the key was released first; nothing to do.
            }
            other => {
                debug_assert!(
                    matches!(other, PttAction::None),
                    "timer() may only yield None or Start"
                );
            }
        }
    }

    /// A key reached a focus target: the held modifier is being used for
    /// typing, not for dictation — cancel the live PTT session without
    /// committing (the cancel idiom: discard the capture and drop the ghost).
    pub fn ptt_key_down(&mut self, cx: &mut Context<Self>) {
        match self.voice.ptt_guard.key_down() {
            PttAction::Cancel => {
                self.voice.ptt_held = false;
                if self.voice.dictating && self.voice.ptt_session {
                    self.invalidate_dictation();
                    self.composer.update(cx, |c, cx| c.dictation_cancel(cx));
                }
                self.voice.ptt_session = false;
                cx.notify();
            }
            PttAction::None => {
                // Disarmed (a keystroke interrupted the arming) or idle: the
                // key continues on to its target, nothing to cancel.
            }
            other => {
                debug_assert!(
                    matches!(other, PttAction::None),
                    "key_down() may only yield None or Cancel"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tap (down, then up before the timer) never dictates.
    #[test]
    fn u3b2_ptt_guard_tap_never_dictates() {
        let mut g = PttGuard::default();
        let gen = match g.modifier(true) {
            PttAction::ArmTimer(gen) => gen,
            other => panic!("down edge arms: {other:?}"),
        };
        assert_eq!(g.modifier(false), PttAction::None, "a tap stops the arming");
        // The (stale) timer still fires — nothing happens.
        assert_eq!(g.timer(gen), PttAction::None, "a stale timer never starts");
        assert_eq!(g.modifier(true), PttAction::ArmTimer(gen + 1));
        g.modifier(false);
        assert_eq!(g.timer(gen + 1), PttAction::None);
    }

    /// A real hold (timer fires while held) starts the session; releasing
    /// stops it.
    #[test]
    fn u3b2_ptt_guard_hold_starts_and_release_stops() {
        let mut g = PttGuard::default();
        let gen = match g.modifier(true) {
            PttAction::ArmTimer(gen) => gen,
            other => panic!("down edge arms: {other:?}"),
        };
        assert_eq!(g.timer(gen), PttAction::Start, "held past HOLD starts");
        assert_eq!(g.modifier(false), PttAction::Stop, "release stops");
    }

    /// A key typed *while arming* disarms — the subsequent (stale) timer
    /// never starts a session.
    #[test]
    fn u3b2_ptt_guard_key_during_arm_disarms() {
        let mut g = PttGuard::default();
        let gen = match g.modifier(true) {
            PttAction::ArmTimer(gen) => gen,
            other => panic!("down edge arms: {other:?}"),
        };
        assert_eq!(g.key_down(), PttAction::None, "keying while armed disarms");
        assert_eq!(g.timer(gen), PttAction::None, "stale timer is a no-op");
    }

    /// A key typed *after* the session started cancels it (the user switched
    /// to typing while holding the modifier).
    #[test]
    fn u3b2_ptt_guard_key_after_start_cancels() {
        let mut g = PttGuard::default();
        let gen = match g.modifier(true) {
            PttAction::ArmTimer(gen) => gen,
            other => panic!("down edge arms: {other:?}"),
        };
        assert_eq!(g.timer(gen), PttAction::Start);
        assert_eq!(g.key_down(), PttAction::Cancel, "keying while live cancels");
        // A second key is just typing now — the guard is idle.
        assert_eq!(g.key_down(), PttAction::None);
    }

    /// A stale timer after the session ended is a no-op (no double-start from
    /// a racing timer of an older hold).
    #[test]
    fn u3b2_ptt_guard_stale_timer_is_noop() {
        let mut g = PttGuard::default();
        let g1 = match g.modifier(true) {
            PttAction::ArmTimer(gen) => gen,
            other => panic!("down edge arms: {other:?}"),
        };
        assert_eq!(g.timer(g1), PttAction::Start);
        assert_eq!(g.modifier(false), PttAction::Stop);
        // The next hold re-arms under a fresh generation.
        let g2 = match g.modifier(true) {
            PttAction::ArmTimer(gen) => gen,
            other => panic!("re-arms with a new gen: {other:?}"),
        };
        assert!(g2 > g1, "generations are monotonic");
        // Firing the OLD generation after the new hold started must not
        // double-start or otherwise misfire.
        assert_eq!(g.timer(g1), PttAction::None);
        assert_eq!(g.timer(g2), PttAction::Start);
    }

    /// The hold window is exactly the documented constant (D2: taps under
    /// 300 ms never dictate).
    #[test]
    fn u3b2_hold_window_is_300ms() {
        assert_eq!(HOLD, Duration::from_millis(300));
    }
}
