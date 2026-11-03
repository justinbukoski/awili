//! Wake-word activation gate — the pure half of hands-free voice activation.
//!
//! Some users have limited hand mobility, so push-to-talk (holding a key down for
//! the length of an utterance) is exactly the kind of sustained fine-motor
//! action voice is meant to *replace*. A wake word ("Hey Awili") lets him arm
//! dictation with his voice alone. The full feature has two halves, split the
//! same way [`Vad`](crate::vad) splits energy detection from utterance assembly:
//!
//! * A **keyword spotter** (future, model-gated) that scores a rolling audio
//!   window for "does this contain the wake phrase?" — a small KWS neural net.
//!   That half needs a trained model *and* Owner's ears to tune its
//!   sensitivity against his voice and room, so it is deliberately not built
//!   here.
//! * The **activation gate** (this module) — pure, deterministic, and
//!   unit-testable on Linux with synthetic scores. It consumes the spotter's
//!   confidence scores and owns the arm→disarm *lifecycle*: when a detection
//!   actually arms hands-free capture, how double-fires are debounced, and when
//!   an armed-but-unused session drops back to idle so the mic does not stay
//!   hot forever.
//!
//! Time is measured in **samples fed**, never wall-clock, so the gate is as
//! reproducible as the VAD: a test advances the clock by calling [`tick`] and
//! the refractory / idle-timeout boundaries land on exact sample counts.
//!
//! [`tick`]: WakeGate::tick

/// Whisper's sample rate, restated here so the timeout constants read in
/// seconds at the call site.
const HZ: u64 = 16_000;

/// Default confidence a spotter score must *exceed* to arm (strictly, mirroring
/// the VAD's `rms > threshold`). Scores are assumed in `0.0..=1.0`.
pub const WAKE_THRESHOLD_DEFAULT: f32 = 0.5;

/// Default debounce after a fire: further detections are ignored for 1 s. The
/// wake phrase itself takes a beat to finish and can echo across a couple of
/// spotter windows; without this the same "Hey Awili" would re-arm several times.
pub const WAKE_REFRACTORY_SAMPLES_DEFAULT: u64 = HZ; // 1.0 s

/// Default auto-disarm: an armed session with no speech for 8 s returns to
/// idle. If the wake word fires by accident (a bump, TV dialogue), the mic must
/// not stay live indefinitely waiting for an utterance that never comes.
pub const WAKE_IDLE_TIMEOUT_SAMPLES_DEFAULT: u64 = 8 * HZ; // 8.0 s

/// What one gate interaction did to the arm state (the *transition*, not the
/// resting state — query that with [`WakeGate::is_armed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeEvent {
    /// The wake word fired from idle: capture should start.
    Armed,
    /// The armed session ended (idle timeout or an explicit disarm): capture
    /// should stop.
    Disarmed,
    /// The interaction changed nothing (below threshold, still debouncing,
    /// already in the resulting state, or just time passing).
    NoChange,
}

/// The arm→disarm state machine for hands-free wake-word activation.
///
/// Drive it three ways, all deterministic against an internal sample clock that
/// only [`tick`](Self::tick) advances:
///
/// * [`detect`](Self::detect) — feed a spotter confidence score. Arms from idle
///   when the score clears `threshold` and the refractory window has elapsed.
/// * [`observe_speech`](Self::observe_speech) — the VAD saw speech; while armed
///   this refreshes the idle-timeout deadline so a real conversation keeps the
///   session alive.
/// * [`tick`](Self::tick) — advance the clock by N samples; fires
///   [`WakeEvent::Disarmed`] when an armed session has been idle past the
///   timeout, and expires the post-fire refractory window.
#[derive(Clone, Debug)]
pub struct WakeGate {
    /// A spotter score must be strictly above this to arm. Default
    /// [`WAKE_THRESHOLD_DEFAULT`].
    pub threshold: f32,
    /// Samples after a fire during which detections are ignored (debounce).
    pub refractory_samples: u64,
    /// Samples of no speech after which an armed session auto-disarms.
    pub idle_timeout_samples: u64,
    /// Monotonic sample clock; only [`tick`](Self::tick) advances it.
    clock: u64,
    /// `true` between an [`WakeEvent::Armed`] and its matching disarm.
    armed: bool,
    /// While armed, the clock value at which the idle timeout fires (last arm or
    /// speech + `idle_timeout_samples`).
    disarm_at: u64,
    /// Detections strictly before this clock value are ignored (post-fire
    /// debounce); the window clears *at* this value. Starts at 0 so the first
    /// detection (clock 0) is never gated.
    refractory_until: u64,
}

impl Default for WakeGate {
    fn default() -> Self {
        Self::new(
            WAKE_THRESHOLD_DEFAULT,
            WAKE_REFRACTORY_SAMPLES_DEFAULT,
            WAKE_IDLE_TIMEOUT_SAMPLES_DEFAULT,
        )
    }
}

impl WakeGate {
    pub fn new(threshold: f32, refractory_samples: u64, idle_timeout_samples: u64) -> Self {
        Self {
            threshold,
            refractory_samples,
            // 0 would auto-disarm on the very next tick after arming, before any
            // speech could arrive; a session must survive at least one sample.
            idle_timeout_samples: idle_timeout_samples.max(1),
            clock: 0,
            armed: false,
            disarm_at: 0,
            refractory_until: 0,
        }
    }

    /// Feed one keyword-spotter confidence score. Arms (returns
    /// [`WakeEvent::Armed`]) only from idle, only when `confidence` is strictly
    /// above `threshold`, and only once the post-fire refractory window has
    /// passed. A qualifying detection *while already armed* is not a re-arm — it
    /// counts as activity and refreshes the idle-timeout deadline, returning
    /// [`WakeEvent::NoChange`].
    pub fn detect(&mut self, confidence: f32) -> WakeEvent {
        if confidence <= self.threshold {
            return WakeEvent::NoChange;
        }
        if self.armed {
            // Hearing the wake phrase again mid-session keeps it alive rather
            // than firing a spurious second Armed.
            self.disarm_at = self.clock.saturating_add(self.idle_timeout_samples);
            return WakeEvent::NoChange;
        }
        if self.clock < self.refractory_until {
            return WakeEvent::NoChange;
        }
        self.armed = true;
        self.disarm_at = self.clock.saturating_add(self.idle_timeout_samples);
        self.refractory_until = self.clock.saturating_add(self.refractory_samples);
        WakeEvent::Armed
    }

    /// The VAD reported speech this window. While armed it pushes the
    /// idle-timeout deadline out so an ongoing utterance is never cut off; while
    /// idle it is a no-op (speech without a wake word must not arm anything).
    pub fn observe_speech(&mut self) {
        if self.armed {
            self.disarm_at = self.clock.saturating_add(self.idle_timeout_samples);
        }
    }

    /// Advance the sample clock by `samples` and settle any time-based
    /// transition. Fires [`WakeEvent::Disarmed`] once when an armed session has
    /// gone `idle_timeout_samples` without speech.
    pub fn tick(&mut self, samples: u64) -> WakeEvent {
        self.clock = self.clock.saturating_add(samples);
        if self.armed && self.clock >= self.disarm_at {
            self.armed = false;
            // Debounce re-arming on the wake word's own trailing echo right after
            // the session closes.
            self.refractory_until = self.clock.saturating_add(self.refractory_samples);
            return WakeEvent::Disarmed;
        }
        WakeEvent::NoChange
    }

    /// Force the session closed (e.g. the utterance was submitted, or Owner
    /// took over with the keyboard). Returns [`WakeEvent::Disarmed`] if it was
    /// armed, else [`WakeEvent::NoChange`]. Also opens a refractory window so the
    /// closing echo cannot immediately re-arm.
    pub fn disarm(&mut self) -> WakeEvent {
        if self.armed {
            self.armed = false;
            self.refractory_until = self.clock.saturating_add(self.refractory_samples);
            WakeEvent::Disarmed
        } else {
            WakeEvent::NoChange
        }
    }

    /// Whether a hands-free session is currently open.
    pub fn is_armed(&self) -> bool {
        self.armed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A gate with tiny, round timeouts so the tests read in samples directly.
    fn gate() -> WakeGate {
        WakeGate::new(0.5, 100, 1_000)
    }

    #[test]
    fn a_confident_detection_arms_from_idle() {
        let mut g = gate();
        assert!(!g.is_armed());
        assert_eq!(g.detect(0.9), WakeEvent::Armed);
        assert!(g.is_armed());
    }

    #[test]
    fn a_weak_detection_does_not_arm() {
        let mut g = gate();
        assert_eq!(g.detect(0.4), WakeEvent::NoChange, "below threshold");
        assert!(!g.is_armed());
    }

    #[test]
    fn score_exactly_at_threshold_does_not_arm() {
        // The gate is `confidence > threshold` — STRICTLY above, mirroring the
        // VAD's `rms > threshold`. A score landing exactly on the threshold must
        // read as no-fire. Every other test uses 0.9 (well above) or 0.4 (well
        // below), so none sits on the boundary: relaxing `>` to `>=` would keep
        // them all green while making a spotter that plateaus at exactly the
        // configured gate arm on ambient audio. This is the mutation witness.
        let mut g = WakeGate::new(0.5, 100, 1_000);
        assert_eq!(
            g.detect(0.5),
            WakeEvent::NoChange,
            "a score exactly at the threshold must not arm: gate is strictly above"
        );
        assert!(!g.is_armed());
        // But anything above the same threshold still arms — the strict boundary
        // must not become an over-strict reject of qualifying input.
        assert_eq!(g.detect(0.5001), WakeEvent::Armed);
    }

    #[test]
    fn detection_within_the_refractory_window_is_ignored() {
        let mut g = gate(); // refractory = 100 samples
        assert_eq!(g.detect(0.9), WakeEvent::Armed);
        assert_eq!(g.disarm(), WakeEvent::Disarmed); // opens refractory_until = clock+100
                                                     // Still inside the debounce window: a second "Hey Awili" echo is ignored.
        g.tick(50);
        assert_eq!(g.detect(0.9), WakeEvent::NoChange, "still debouncing");
        assert!(!g.is_armed());
    }

    #[test]
    fn detection_just_before_the_refractory_boundary_is_still_gated() {
        let mut g = gate(); // refractory = 100
        g.detect(0.9);
        g.disarm(); // refractory_until = 0 + 100 = 100
        g.tick(99); // clock 99 < 100: still inside the window
        assert_eq!(
            g.detect(0.9),
            WakeEvent::NoChange,
            "one sample short of the boundary is gated"
        );
        assert!(!g.is_armed());
    }

    #[test]
    fn refractory_window_clears_exactly_at_its_boundary() {
        // detect() gates on `clock < refractory_until`, so the window clears AT
        // the boundary sample. Pin the off-by-one both ways: at clock ==
        // refractory_until - 1 it is gated, at clock == refractory_until it
        // arms. The boundary-arm assert kills `<`→`<=` (which would still gate at
        // the boundary); the just-before-gate assert kills `<`→`==`-style slips.
        let mut g = WakeGate::new(0.5, 100, 1_000);
        g.detect(0.9);
        g.disarm(); // refractory_until = 100
        g.tick(99);
        assert_eq!(
            g.detect(0.9),
            WakeEvent::NoChange,
            "clock 99 < 100 is still gated"
        );
        g.tick(1); // clock == 100 == refractory_until
        assert_eq!(
            g.detect(0.9),
            WakeEvent::Armed,
            "the window clears at its boundary"
        );
    }

    #[test]
    fn armed_session_auto_disarms_after_the_idle_timeout() {
        let mut g = gate(); // idle_timeout = 1_000
        assert_eq!(g.detect(0.9), WakeEvent::Armed); // disarm_at = 1_000
                                                     // Just short of the timeout: still armed.
        assert_eq!(g.tick(999), WakeEvent::NoChange);
        assert!(g.is_armed());
        // The sample that reaches the deadline disarms.
        assert_eq!(g.tick(1), WakeEvent::Disarmed);
        assert!(!g.is_armed());
    }

    #[test]
    fn idle_timeout_boundary_fires_at_the_deadline_not_after() {
        // Disarm is `clock >= disarm_at` — it fires AT the deadline sample. A
        // test crossing the deadline in one big tick would pass under `>` too;
        // land the clock exactly on disarm_at to pin `>=` vs `>`.
        let mut g = WakeGate::new(0.5, 100, 1_000);
        g.detect(0.9); // disarm_at = 1_000
        assert_eq!(
            g.tick(1_000),
            WakeEvent::Disarmed,
            "clock == disarm_at must fire"
        );
    }

    #[test]
    fn speech_keeps_an_armed_session_alive_past_the_timeout() {
        let mut g = gate(); // idle_timeout = 1_000
        g.detect(0.9); // disarm_at = 1_000
        g.tick(900);
        g.observe_speech(); // disarm_at pushed to 900 + 1_000 = 1_900
                            // What would have been the original deadline passes with no disarm.
        assert_eq!(
            g.tick(200),
            WakeEvent::NoChange,
            "clock 1_100 < refreshed 1_900"
        );
        assert!(g.is_armed());
    }

    #[test]
    fn speech_while_idle_does_not_arm() {
        let mut g = gate();
        g.observe_speech();
        g.tick(500);
        assert!(!g.is_armed(), "speech without a wake word must never arm");
    }

    #[test]
    fn a_repeat_detection_while_armed_refreshes_but_does_not_rearm() {
        let mut g = gate(); // idle_timeout = 1_000
        assert_eq!(g.detect(0.9), WakeEvent::Armed); // disarm_at = 1_000
        g.tick(800);
        // Hearing the phrase again is activity, not a second Armed event.
        assert_eq!(
            g.detect(0.9),
            WakeEvent::NoChange,
            "already armed: no second Armed"
        );
        assert!(g.is_armed());
        // ...and it refreshed the deadline to 800 + 1_000 = 1_800.
        assert_eq!(
            g.tick(900),
            WakeEvent::NoChange,
            "clock 1_700 < refreshed 1_800"
        );
        assert!(g.is_armed());
    }

    #[test]
    fn explicit_disarm_reports_only_when_it_was_armed() {
        let mut g = gate();
        assert_eq!(
            g.disarm(),
            WakeEvent::NoChange,
            "disarming an idle gate changes nothing"
        );
        g.detect(0.9);
        assert_eq!(g.disarm(), WakeEvent::Disarmed);
        assert_eq!(
            g.disarm(),
            WakeEvent::NoChange,
            "second disarm is idempotent"
        );
    }

    #[test]
    fn tick_on_an_idle_gate_never_disarms() {
        let mut g = gate();
        for _ in 0..100 {
            assert_eq!(g.tick(1_000), WakeEvent::NoChange);
        }
        assert!(!g.is_armed());
    }

    #[test]
    fn a_full_cycle_can_arm_again_after_the_refractory_clears() {
        let mut g = gate(); // refractory = 100, idle_timeout = 1_000
        assert_eq!(g.detect(0.9), WakeEvent::Armed);
        assert_eq!(g.tick(1_000), WakeEvent::Disarmed); // auto-disarm, refractory_until = 1_100
                                                        // Inside the post-disarm refractory: ignored.
        assert_eq!(g.detect(0.9), WakeEvent::NoChange);
        g.tick(101); // clock 1_101 > 1_100
        assert_eq!(
            g.detect(0.9),
            WakeEvent::Armed,
            "a fresh wake word arms the next session"
        );
    }

    #[test]
    fn zero_idle_timeout_clamps_to_one_sample() {
        // 0 would disarm on the first tick after arming, before any speech could
        // arrive; the session must survive at least one sample.
        let mut g = WakeGate::new(0.5, 0, 0);
        assert_eq!(g.detect(0.9), WakeEvent::Armed);
        assert_eq!(
            g.tick(1),
            WakeEvent::Disarmed,
            "clamped to a 1-sample session"
        );
    }

    #[test]
    fn disarm_opens_the_refractory_so_the_closing_echo_cannot_rearm() {
        // The wake phrase's own tail can still be scoring high the instant the
        // session closes; disarm must debounce it exactly like a fire does.
        let mut g = WakeGate::new(0.5, 100, 1_000);
        g.detect(0.9);
        assert_eq!(g.disarm(), WakeEvent::Disarmed);
        // Same clock, high score: without the disarm-time refractory this would
        // immediately re-arm on the echo.
        assert_eq!(
            g.detect(0.9),
            WakeEvent::NoChange,
            "closing echo is debounced"
        );
    }
}
