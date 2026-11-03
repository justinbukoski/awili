//! Energy-based voice activity detection (slice-5 spec §1).
//!
//! Conversation mode needs to know when Owner has *finished* an utterance so it
//! can auto-submit without a keypress — the whole point of the hands-free loop.
//! A full ML VAD is overkill for a quiet home office; RMS energy over 20 ms
//! windows with a consecutive-silence endpoint is simple, deterministic, and
//! unit-testable with synthetic samples. The threshold and endpoint length are
//! config knobs (`[voice] vad_threshold` / `vad_silence_windows`) because mic
//! gain and room noise vary per machine.

/// Samples per VAD window: 20 ms at whisper's 16 kHz.
pub const WINDOW_SAMPLES: usize = 320;

/// What one [`Vad::feed`] call observed (the most significant event when the
/// slice spans several windows — an endpoint outranks everything, a fresh
/// speech start outranks continuation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VadEvent {
    /// Speech energy appeared after silence — an utterance began.
    SpeechStart,
    /// Inside an utterance (speech, or a pause shorter than the endpoint).
    SpeechContinue,
    /// The utterance ended: `silence_windows` consecutive silent windows after
    /// speech was detected. The conversation loop submits on this.
    SilenceEndpoint,
    /// Nothing but silence; no utterance in progress.
    Silence,
}

/// Energy-based VAD: speech = window RMS above `threshold`; endpoint =
/// `silence_windows` consecutive silent windows after speech. State is carried
/// across [`feed`](Self::feed) calls (including a partial trailing window), so
/// the caller can hand over capture-buffer slices of any size.
#[derive(Clone, Debug)]
pub struct Vad {
    /// RMS above this is speech. Default 0.01 — well above electrical noise,
    /// well below a normal speaking voice at arm's length.
    pub threshold: f32,
    /// Consecutive silent windows that end an utterance. Default 30 (600 ms).
    pub silence_windows: usize,
    /// Tail samples that did not fill a 20 ms window yet.
    pending: Vec<f32>,
    /// An utterance is in progress (speech seen, endpoint not yet fired).
    in_speech: bool,
    /// Silent windows seen in a row while `in_speech`.
    silent_run: usize,
}

impl Default for Vad {
    fn default() -> Self {
        Self::new(0.01, 30)
    }
}

impl Vad {
    pub fn new(threshold: f32, silence_windows: usize) -> Self {
        Self {
            threshold,
            // 0 would endpoint on the first silent window mid-word; clamp to 1.
            silence_windows: silence_windows.max(1),
            pending: Vec::new(),
            in_speech: false,
            silent_run: 0,
        }
    }

    /// Feed captured samples (16 kHz mono). Processes every complete 20 ms
    /// window, buffering the remainder for the next call.
    pub fn feed(&mut self, samples: &[f32]) -> VadEvent {
        self.pending.extend_from_slice(samples);
        let mut endpoint = false;
        let mut started = false;
        let mut offset = 0;
        while self.pending.len() - offset >= WINDOW_SAMPLES {
            let window = &self.pending[offset..offset + WINDOW_SAMPLES];
            offset += WINDOW_SAMPLES;
            if rms(window) > self.threshold {
                if !self.in_speech {
                    self.in_speech = true;
                    started = true;
                }
                self.silent_run = 0;
            } else if self.in_speech {
                self.silent_run += 1;
                if self.silent_run >= self.silence_windows {
                    endpoint = true;
                    self.in_speech = false;
                    self.silent_run = 0;
                }
            }
        }
        self.pending.drain(..offset);
        if endpoint {
            VadEvent::SilenceEndpoint
        } else if started {
            VadEvent::SpeechStart
        } else if self.in_speech {
            VadEvent::SpeechContinue
        } else {
            VadEvent::Silence
        }
    }

    /// Whether an utterance is currently in progress.
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }
}

/// Root-mean-square energy of a window.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(amplitude: f32, n: usize) -> Vec<f32> {
        // A constant-amplitude square-ish signal: RMS == |amplitude| exactly,
        // so tests reason about the threshold directly.
        vec![amplitude; n * WINDOW_SAMPLES]
    }

    #[test]
    fn silence_only_never_endpoints() {
        let mut vad = Vad::default();
        for _ in 0..100 {
            assert_eq!(vad.feed(&windows(0.0, 1)), VadEvent::Silence);
        }
        // Quiet noise below the threshold is still silence.
        assert_eq!(vad.feed(&windows(0.005, 50)), VadEvent::Silence);
        assert!(!vad.in_speech());
    }

    #[test]
    fn speech_then_silence_fires_endpoint_after_n_windows() {
        let mut vad = Vad::new(0.01, 30);
        assert_eq!(vad.feed(&windows(0.1, 5)), VadEvent::SpeechStart);
        // 29 silent windows: still inside the utterance, no endpoint yet.
        for i in 0..29 {
            assert_eq!(
                vad.feed(&windows(0.0, 1)),
                VadEvent::SpeechContinue,
                "window {i}"
            );
        }
        // The 30th consecutive silent window ends the utterance.
        assert_eq!(vad.feed(&windows(0.0, 1)), VadEvent::SilenceEndpoint);
        // After the endpoint, we are back to plain silence.
        assert_eq!(vad.feed(&windows(0.0, 1)), VadEvent::Silence);
    }

    #[test]
    fn speech_never_ending_does_not_endpoint() {
        let mut vad = Vad::new(0.01, 30);
        assert_eq!(vad.feed(&windows(0.2, 1)), VadEvent::SpeechStart);
        for _ in 0..200 {
            assert_eq!(vad.feed(&windows(0.2, 1)), VadEvent::SpeechContinue);
        }
        assert!(vad.in_speech());
    }

    #[test]
    fn zero_energy_window_is_silence_not_speech_at_the_threshold() {
        // The speech test is `rms > threshold` — STRICTLY above (module doc: "RMS
        // above this is speech"). A window whose RMS lands exactly ON the threshold
        // must read as silence, not speech. Every other test picks an amplitude that
        // differs from the threshold (0.01 vs 0.005 / 0.05 / 0.1), so none exercises
        // the exact boundary: weakening `>` to `>=` keeps all 9 passing while making
        // a window sitting exactly at the configured gate trigger a spurious
        // SpeechStart — the VAD would open an utterance on room tone tuned to the
        // threshold. A zero threshold with pure digital silence is the robust
        // witness: rms(all-zero) is exactly 0.0, so `0.0 > 0.0` is unambiguously
        // false (no float-rounding edge), and `0.0 >= 0.0` would flip it to speech.
        let mut vad = Vad::new(0.0, 5);
        assert_eq!(
            vad.feed(&windows(0.0, 1)),
            VadEvent::Silence,
            "a zero-energy window at threshold 0 is silence: rms must be STRICTLY above"
        );
        assert!(
            !vad.in_speech(),
            "no utterance opened by exact-threshold energy"
        );
        // But any energy genuinely above the (zero) floor is still speech — the
        // strict boundary must not become an over-strict reject of all input.
        assert_eq!(
            vad.feed(&windows(0.0001, 1)),
            VadEvent::SpeechStart,
            "energy above the zero floor is speech"
        );
    }

    #[test]
    fn threshold_is_configurable() {
        // 0.05 amplitude: speech under the default threshold, silence under a
        // raised one.
        let mut sensitive = Vad::new(0.01, 30);
        assert_eq!(sensitive.feed(&windows(0.05, 1)), VadEvent::SpeechStart);
        let mut strict = Vad::new(0.1, 30);
        assert_eq!(strict.feed(&windows(0.05, 1)), VadEvent::Silence);
    }

    #[test]
    fn silence_run_resets_when_speech_resumes() {
        // A mid-sentence pause shorter than the endpoint must not accumulate
        // across resumed speech.
        let mut vad = Vad::new(0.01, 30);
        vad.feed(&windows(0.1, 1));
        for _ in 0..29 {
            vad.feed(&windows(0.0, 1));
        }
        // Speech resumes on window 29 — the silent run starts over.
        assert_eq!(vad.feed(&windows(0.1, 1)), VadEvent::SpeechContinue);
        for i in 0..29 {
            assert_eq!(
                vad.feed(&windows(0.0, 1)),
                VadEvent::SpeechContinue,
                "window {i}"
            );
        }
        assert_eq!(vad.feed(&windows(0.0, 1)), VadEvent::SilenceEndpoint);
    }

    #[test]
    fn second_utterance_fires_a_second_endpoint() {
        let mut vad = Vad::new(0.01, 5);
        vad.feed(&windows(0.1, 2));
        assert_eq!(vad.feed(&windows(0.0, 5)), VadEvent::SilenceEndpoint);
        assert_eq!(vad.feed(&windows(0.1, 2)), VadEvent::SpeechStart);
        assert_eq!(vad.feed(&windows(0.0, 5)), VadEvent::SilenceEndpoint);
    }

    #[test]
    fn partial_windows_buffer_across_feed_calls() {
        // Sample-dribbling callers (tiny capture callbacks) must see the same
        // events as whole-window callers.
        let mut vad = Vad::new(0.01, 2);
        let speech = windows(0.1, 1);
        let mut last = VadEvent::Silence;
        for chunk in speech.chunks(7) {
            let ev = vad.feed(chunk);
            if ev != VadEvent::Silence {
                last = ev;
            }
        }
        assert_eq!(last, VadEvent::SpeechStart);
        let silence = windows(0.0, 2);
        let mut got_endpoint = false;
        for chunk in silence.chunks(7) {
            if vad.feed(chunk) == VadEvent::SilenceEndpoint {
                got_endpoint = true;
            }
        }
        assert!(got_endpoint);
    }

    #[test]
    fn zero_silence_windows_clamps_to_one() {
        let mut vad = Vad::new(0.01, 0);
        vad.feed(&windows(0.1, 1));
        assert_eq!(vad.feed(&windows(0.0, 1)), VadEvent::SilenceEndpoint);
    }

    #[test]
    fn whole_buffer_speech_plus_silence_reports_endpoint() {
        // One big feed spanning start AND endpoint: the endpoint (what the
        // conversation loop acts on) wins.
        let mut vad = Vad::new(0.01, 30);
        let mut buf = windows(0.1, 10);
        buf.extend(windows(0.0, 30));
        assert_eq!(vad.feed(&buf), VadEvent::SilenceEndpoint);
    }
}
