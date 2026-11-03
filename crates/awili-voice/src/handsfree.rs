//! Hands-free session orchestrator — the pure loop that ties wake-word
//! activation to utterance capture.
//!
//! The two halves of hands-free voice already exist as independent, pure state
//! machines, split the same way for the same reason (deterministic, Linux-CI
//! testable against synthetic input):
//!
//! * [`WakeGate`](crate::wake::WakeGate) owns the arm→disarm *lifecycle* — when
//!   a keyword-spotter confidence actually opens a session, how double-fires are
//!   debounced, and when an armed-but-silent session drops back to idle so the
//!   mic does not stay hot forever.
//! * [`UtteranceSegmenter`](crate::conversation::UtteranceSegmenter) owns
//!   *capture* — it runs the [`Vad`](crate::vad::Vad) over the audio and hands
//!   back one whole utterance (preroll + speech + endpoint silence) each time
//!   speech endpoints.
//!
//! Neither one is the hands-free loop by itself: the gate never sees audio, the
//! segmenter never sleeps. [`HandsFreeSession`] is the missing composition —
//! "the whole point of the hands-free loop" that [`vad`](crate::vad)'s own doc
//! names. It is still pure: audio only *flows* while the gate is armed, the
//! gate's idle-timeout is kept alive by the segmenter's own speech detection
//! (so a long utterance is never cut off mid-word), and each completed utterance
//! is surfaced for transcription. Owner says "Hey Awili", speaks, and the reply
//! is submitted without a keypress; after a lull the session sleeps and the mic
//! goes cold until the next wake word.
//!
//! What is *not* here, deliberately, is the same boundary the two halves already
//! draw: the keyword-spotter neural net that turns audio into the confidence
//! scores this loop consumes needs a trained model *and* Owner's ears to tune
//! its sensitivity against his voice and room. This orchestrator takes the
//! spotter's confidence as an input, exactly as [`WakeGate::detect`] does, so
//! the control flow is fully testable on Linux with synthetic scores today and
//! the model can be dropped in later without touching the lifecycle.
//!
//! Time is measured in **samples fed**, never wall-clock, inherited from both
//! sub-machines: the same audio replayed produces the same events.

use crate::conversation::UtteranceSegmenter;
use crate::vad::Vad;
use crate::wake::{WakeEvent, WakeGate};

/// What one [`HandsFreeSession::feed`] did — the *most significant* transition,
/// mirroring how [`Vad::feed`](crate::vad::Vad::feed) collapses a multi-window
/// slice to one event. For the resting question "is the mic live right now?"
/// query [`HandsFreeSession::is_active`] instead; it is authoritative even in the
/// rare feed that both finishes an utterance and times the session out.
#[derive(Clone, Debug, PartialEq)]
pub enum HandsFreeEvent {
    /// Asleep and still asleep: the confidence was below the wake threshold (or
    /// debouncing), so no audio was captured this feed.
    Idle,
    /// The wake word just fired: a session opened and capture has begun. The UI
    /// should show the mic as live.
    Activated,
    /// Armed and capturing, but no utterance has endpointed yet this feed.
    Listening,
    /// A complete utterance (preroll + speech + endpoint silence) is ready to
    /// transcribe and submit. The session normally stays armed for the next
    /// utterance; check [`is_active`](HandsFreeSession::is_active) if the same
    /// feed also reached the idle timeout.
    Utterance(Vec<f32>),
    /// The session closed on its own — [`idle_timeout`] of no speech elapsed, so
    /// the mic goes cold until the next wake word.
    ///
    /// [`idle_timeout`]: crate::wake::WAKE_IDLE_TIMEOUT_SAMPLES_DEFAULT
    Deactivated,
}

/// The pure hands-free loop: a [`WakeGate`] gating a per-session
/// [`UtteranceSegmenter`].
///
/// Drive it with one call per audio chunk — [`feed`](Self::feed) takes the
/// keyword-spotter's confidence for that chunk *and* the chunk's samples.
/// Everything is deterministic against the two sub-machines' shared sample
/// clock; a test advances time simply by feeding audio.
///
/// The VAD is rebuilt fresh (from the configured threshold and endpoint length)
/// at the start of every session, so one session's trailing pause can never
/// leak state into the next — each "Hey Awili" starts capture from a clean slate.
#[derive(Clone, Debug)]
pub struct HandsFreeSession {
    gate: WakeGate,
    /// The VAD tuning for each session's segmenter. Stored as the two config
    /// knobs (not a live `Vad`) so a fresh, stateless VAD is minted per arm.
    vad_threshold: f32,
    vad_silence_windows: usize,
    /// `Some` only while armed — the current session's capture pipeline.
    segmenter: Option<UtteranceSegmenter>,
}

impl HandsFreeSession {
    /// Build a session from a configured [`WakeGate`] and the [`Vad`] tuning to
    /// use for capture. Only the VAD's `threshold` and `silence_windows` are
    /// read; any accumulated state on the passed VAD is ignored, since each
    /// session gets its own fresh VAD.
    pub fn new(gate: WakeGate, vad: &Vad) -> Self {
        Self {
            gate,
            vad_threshold: vad.threshold,
            vad_silence_windows: vad.silence_windows,
            segmenter: None,
        }
    }

    /// A stateless segmenter for a new session, minted from the stored tuning.
    fn fresh_segmenter(&self) -> UtteranceSegmenter {
        UtteranceSegmenter::new(Vad::new(self.vad_threshold, self.vad_silence_windows))
    }

    /// Feed one audio chunk together with the keyword-spotter's `confidence` for
    /// it. While asleep, only `confidence` matters — a qualifying score opens a
    /// session (and this same chunk primes the capture buffer, so the wake
    /// word's tail becomes preroll rather than being clipped). While armed, the
    /// samples flow through the session's segmenter; any speech in them refreshes
    /// the idle-timeout so an ongoing utterance is never cut off, and a
    /// completed utterance is returned.
    pub fn feed(&mut self, confidence: f32, samples: &[f32]) -> HandsFreeEvent {
        let n = samples.len() as u64;

        if !self.gate.is_armed() {
            // Asleep: the spotter is the only thing that can wake us. Speech
            // alone must never arm (WakeGate enforces this), so idle audio is
            // discarded rather than captured.
            let armed = self.gate.detect(confidence) == WakeEvent::Armed;
            // Advance the clock even while idle so the post-session refractory
            // window expires in real (sample) time; an idle tick never disarms.
            self.gate.tick(n);
            if armed {
                let mut seg = self.fresh_segmenter();
                // Prime the fresh segmenter with the waking chunk: it carries the
                // wake phrase's own tail, which is the preroll before Owner's
                // first real word. (A wake phrase this short will not endpoint in
                // one chunk, so this never spuriously emits an utterance.)
                let utt = seg.feed(samples);
                if seg.in_speech() {
                    self.gate.observe_speech();
                }
                self.segmenter = Some(seg);
                // The improbable case that the priming chunk already endpointed:
                // surface the utterance rather than dropping it.
                if let Some(u) = utt {
                    return HandsFreeEvent::Utterance(u);
                }
                return HandsFreeEvent::Activated;
            }
            return HandsFreeEvent::Idle;
        }

        // Armed: capture. Feed audio first, then refresh the idle timer on any
        // speech this chunk, then advance the clock so the timeout can fire.
        let seg = self
            .segmenter
            .as_mut()
            .expect("armed session always has a segmenter");
        let utt = seg.feed(samples);
        if seg.in_speech() {
            self.gate.observe_speech();
        }
        let disarmed = self.gate.tick(n) == WakeEvent::Disarmed;
        if disarmed {
            // The session ended; drop the capture pipeline so the next arm starts
            // clean. (Ordering: an utterance that endpointed this same chunk left
            // the VAD in silence, so observe_speech above did NOT refresh the
            // timer — the two can only coincide when the endpoint's own trailing
            // silence is what crossed the deadline. The finished utterance still
            // ships; is_active() now reads false so the caller stops the mic.)
            self.segmenter = None;
        }

        if let Some(u) = utt {
            return HandsFreeEvent::Utterance(u);
        }
        if disarmed {
            return HandsFreeEvent::Deactivated;
        }
        HandsFreeEvent::Listening
    }

    /// Force the session closed — Owner took over with the keyboard, or the
    /// utterance was handled and the caller wants the mic cold immediately.
    /// Returns [`HandsFreeEvent::Deactivated`] if a session was open, else
    /// [`HandsFreeEvent::Idle`]. The post-session refractory window opens so the
    /// closing echo cannot instantly re-arm.
    pub fn end_session(&mut self) -> HandsFreeEvent {
        if self.gate.disarm() == WakeEvent::Disarmed {
            self.segmenter = None;
            HandsFreeEvent::Deactivated
        } else {
            HandsFreeEvent::Idle
        }
    }

    /// Whether the mic is live right now (a session is open). Authoritative
    /// resting-state query, distinct from the per-feed transition events.
    pub fn is_active(&self) -> bool {
        self.gate.is_armed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::WINDOW_SAMPLES;

    // Round numbers so the tests read in samples directly: wake threshold 0.5,
    // refractory 100 samples, idle-timeout large enough to span several chunks.
    fn gate() -> WakeGate {
        WakeGate::new(0.5, 100, 8_000)
    }

    fn vad() -> Vad {
        Vad::new(0.01, 5)
    }

    fn session() -> HandsFreeSession {
        HandsFreeSession::new(gate(), &vad())
    }

    // n windows of constant amplitude; RMS == |amplitude| so tests reason about
    // the VAD threshold directly (same helper convention as vad/conversation).
    fn windows(amplitude: f32, n: usize) -> Vec<f32> {
        vec![amplitude; n * WINDOW_SAMPLES]
    }

    #[test]
    fn silence_while_asleep_stays_idle_and_captures_nothing() {
        let mut s = session();
        for _ in 0..100 {
            assert_eq!(s.feed(0.0, &windows(0.2, 1)), HandsFreeEvent::Idle);
        }
        assert!(!s.is_active(), "loud speech without a wake word never arms");
    }

    #[test]
    fn a_confident_score_activates_the_session() {
        let mut s = session();
        assert_eq!(s.feed(0.9, &windows(0.0, 1)), HandsFreeEvent::Activated);
        assert!(s.is_active());
    }

    #[test]
    fn a_weak_score_does_not_activate() {
        let mut s = session();
        assert_eq!(s.feed(0.4, &windows(0.0, 1)), HandsFreeEvent::Idle);
        assert!(!s.is_active());
    }

    #[test]
    fn armed_speech_then_silence_yields_the_utterance() {
        let mut s = session();
        // Wake on a silent chunk so the utterance is only the speech we feed next.
        assert_eq!(s.feed(0.9, &windows(0.0, 1)), HandsFreeEvent::Activated);
        // Speech, still open.
        assert_eq!(s.feed(0.0, &windows(0.1, 3)), HandsFreeEvent::Listening);
        // Four silent windows: not yet the 5-window endpoint.
        assert_eq!(s.feed(0.0, &windows(0.0, 4)), HandsFreeEvent::Listening);
        // The fifth consecutive silent window endpoints the utterance.
        match s.feed(0.0, &windows(0.0, 1)) {
            HandsFreeEvent::Utterance(u) => {
                // The 1 silent priming window (kept as preroll) + 3 speech + 5
                // silence windows fed since arming.
                assert_eq!(u.len(), 9 * WINDOW_SAMPLES);
                assert!(u.iter().any(|&x| x > 0.05), "the speech is in the buffer");
            }
            other => panic!("expected an utterance, got {other:?}"),
        }
        assert!(
            s.is_active(),
            "the session stays armed for the next utterance"
        );
    }

    #[test]
    fn the_waking_chunk_primes_capture_so_the_first_word_is_not_clipped() {
        let mut s = session();
        // The wake chunk itself contains speech (the phrase's tail). It must seed
        // the segmenter, so the following silence endpoints an utterance that
        // includes that speech rather than an empty buffer.
        assert_eq!(s.feed(0.9, &windows(0.1, 2)), HandsFreeEvent::Activated);
        match s.feed(0.0, &windows(0.0, 5)) {
            HandsFreeEvent::Utterance(u) => {
                assert!(
                    u.iter().any(|&x| x > 0.05),
                    "the wake chunk's speech was captured as the utterance's onset"
                );
            }
            other => panic!("expected the primed utterance, got {other:?}"),
        }
    }

    #[test]
    fn a_long_utterance_is_not_cut_off_by_the_idle_timeout() {
        // idle_timeout = 8_000 samples = 25 windows. Feed 40 windows of unbroken
        // speech: without observe_speech refreshing the deadline, the gate would
        // disarm mid-utterance. It must stay armed instead.
        let mut s = session();
        s.feed(0.9, &windows(0.0, 1));
        for i in 0..40 {
            let ev = s.feed(0.0, &windows(0.1, 1));
            assert_eq!(ev, HandsFreeEvent::Listening, "window {i} mid-utterance");
            assert!(s.is_active(), "speech keeps the session alive: window {i}");
        }
    }

    #[test]
    fn an_armed_session_with_no_speech_times_out() {
        let mut s = session(); // idle_timeout = 8_000
        s.feed(0.9, &windows(0.0, 1)); // arm; disarm_at = 8_000
                                       // Feed silence in chunks until the deadline. 8_000 / 320 = 25 windows.
        let mut deactivated_at = None;
        for i in 0..30 {
            if s.feed(0.0, &windows(0.0, 1)) == HandsFreeEvent::Deactivated {
                deactivated_at = Some(i);
                break;
            }
        }
        assert!(deactivated_at.is_some(), "the idle session must time out");
        assert!(!s.is_active(), "the mic goes cold after the timeout");
    }

    #[test]
    fn a_new_wake_word_starts_a_second_session_after_the_refractory_clears() {
        let mut s = session(); // refractory = 100 samples
        s.feed(0.9, &windows(0.0, 1)); // arm; clock -> 320
        assert_eq!(s.end_session(), HandsFreeEvent::Deactivated); // refractory_until = 420
                                                                  // A high score on the *same* clock (320 < 420) is the closing echo and
                                                                  // must be debounced. `detect` reads the clock before this feed advances
                                                                  // it, so an empty chunk cannot clear the window.
        assert_eq!(
            s.feed(0.9, &windows(0.0, 0)),
            HandsFreeEvent::Idle,
            "the closing echo cannot instantly re-arm"
        );
        // A silent chunk advances the clock past the refractory (320 -> 640) but
        // does not arm (no wake score).
        assert_eq!(s.feed(0.0, &windows(0.0, 1)), HandsFreeEvent::Idle);
        // Now the window has cleared (640 >= 420): a fresh wake word arms again.
        assert_eq!(s.feed(0.9, &windows(0.0, 1)), HandsFreeEvent::Activated);
        assert!(s.is_active());
    }

    #[test]
    fn end_session_on_an_idle_loop_changes_nothing() {
        let mut s = session();
        assert_eq!(s.end_session(), HandsFreeEvent::Idle);
        assert!(!s.is_active());
    }

    #[test]
    fn end_session_stops_an_active_session() {
        let mut s = session();
        s.feed(0.9, &windows(0.0, 1));
        assert!(s.is_active());
        assert_eq!(s.end_session(), HandsFreeEvent::Deactivated);
        assert!(!s.is_active());
    }

    #[test]
    fn each_session_captures_independently_no_state_leak() {
        // A first session with an in-progress (never-endpointed) utterance is
        // ended; the second session must start from a clean VAD, so its own
        // speech-then-silence endpoints a fresh utterance rather than inheriting
        // the first session's open-speech state.
        let mut s = session();
        s.feed(0.9, &windows(0.0, 1));
        s.feed(0.0, &windows(0.1, 3)); // speech opens, never endpoints
        assert_eq!(s.end_session(), HandsFreeEvent::Deactivated);
        // Let the refractory clear, then arm a second session on silence.
        s.feed(0.0, &windows(0.0, 1));
        assert_eq!(s.feed(0.9, &windows(0.0, 1)), HandsFreeEvent::Activated);
        // Fresh speech + endpoint: the utterance holds ONLY this session's audio.
        s.feed(0.0, &windows(0.1, 2));
        match s.feed(0.0, &windows(0.0, 5)) {
            HandsFreeEvent::Utterance(u) => {
                // 1 silent priming window + 2 speech + 5 silence — and crucially
                // NOT the 3 speech windows from session one (would be 11 windows
                // if the open-speech VAD state had leaked across sessions).
                assert_eq!(u.len(), 8 * WINDOW_SAMPLES);
            }
            other => panic!("expected a clean second-session utterance, got {other:?}"),
        }
    }

    #[test]
    fn two_utterances_in_one_session_both_fire() {
        let mut s = session();
        s.feed(0.9, &windows(0.0, 1));
        // First utterance.
        s.feed(0.0, &windows(0.1, 2));
        assert!(matches!(
            s.feed(0.0, &windows(0.0, 5)),
            HandsFreeEvent::Utterance(_)
        ));
        assert!(s.is_active(), "still armed after the first utterance");
        // Second utterance in the same session.
        s.feed(0.0, &windows(0.1, 2));
        assert!(matches!(
            s.feed(0.0, &windows(0.0, 5)),
            HandsFreeEvent::Utterance(_)
        ));
    }
}
