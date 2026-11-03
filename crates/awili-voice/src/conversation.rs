//! Conversation-mode capture (slice-5 spec §2): mic audio flows through the
//! [`Vad`] and each utterance — speech up to a silence endpoint — is handed to a
//! callback as one buffer for transcription.
//!
//! Split in two so the interesting part is testable on Linux:
//!
//! * [`UtteranceSegmenter`] — pure: feed samples, get back a finished utterance
//!   when the VAD endpoints. No threads, no audio hardware.
//! * [`ConversationCapture`] — the worker thread that polls a capture's
//!   [`SharedSamples`] buffer (draining it, unlike dictation's grow-only
//!   polling), feeds the segmenter, and fires the callback. It only needs the
//!   shared buffer, so it compiles and runs everywhere; the *mic* feeding that
//!   buffer is [`MicCapture`](crate::MicCapture), macOS-gated exactly as in
//!   dictation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::capture::SharedSamples;
use crate::vad::{Vad, VadEvent};

/// Samples of pre-speech audio kept while idle (0.5 s at 16 kHz), so the first
/// syllable — which is what *triggers* SpeechStart — is included in the
/// utterance rather than clipped off.
pub const PREROLL_SAMPLES: usize = 8_000;

/// Hard ceiling on a single utterance: 30 s at 16 kHz, whisper's native window
/// (codex review SF3). Sustained noise that keeps window RMS above the
/// threshold (music, a fan over a mis-tuned threshold) never reaches a silence
/// endpoint, so without a cap the accumulator grows ~64 KB/s indefinitely and
/// an eventual endpoint would hand whisper a multi-minute buffer in one piece.
/// At the cap the utterance is force-flushed as if an endpoint had fired.
pub const MAX_UTTERANCE_SAMPLES: usize = 480_000;

/// How often the capture worker polls the shared buffer: 3 VAD windows.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// Pure utterance assembly: accumulates fed samples and returns the whole
/// utterance (preroll + speech + endpoint silence) when the VAD endpoints.
/// While no speech is in progress the accumulator is trimmed to
/// [`PREROLL_SAMPLES`], so hours of silence cannot grow memory; while speech
/// *is* in progress it is force-flushed at [`MAX_UTTERANCE_SAMPLES`], so
/// never-ending noise cannot either (codex review SF3).
#[derive(Clone, Debug)]
pub struct UtteranceSegmenter {
    vad: Vad,
    acc: Vec<f32>,
}

impl UtteranceSegmenter {
    pub fn new(vad: Vad) -> Self {
        Self {
            vad,
            acc: Vec::new(),
        }
    }

    /// Feed captured samples; `Some(utterance)` when a silence endpoint closed
    /// one — or when the accumulator hit [`MAX_UTTERANCE_SAMPLES`] mid-speech.
    pub fn feed(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        self.acc.extend_from_slice(samples);
        match self.vad.feed(samples) {
            VadEvent::SilenceEndpoint => Some(std::mem::take(&mut self.acc)),
            VadEvent::Silence => {
                if self.acc.len() > PREROLL_SAMPLES {
                    self.acc.drain(..self.acc.len() - PREROLL_SAMPLES);
                }
                None
            }
            VadEvent::SpeechStart | VadEvent::SpeechContinue => {
                // Codex SF3: speech (or noise) that never endpoints must not
                // accumulate without bound — flush at whisper's native window.
                // The VAD stays in-speech; whatever follows continues as a
                // fresh accumulator and ships on its own endpoint (or cap).
                if self.acc.len() >= MAX_UTTERANCE_SAMPLES {
                    Some(std::mem::take(&mut self.acc))
                } else {
                    None
                }
            }
        }
    }

    /// Whether an utterance is currently in progress (delegates to the inner
    /// [`Vad`]). The hands-free loop uses this to refresh the wake gate's
    /// idle-timeout while speech is ongoing, so a long utterance is never cut
    /// off mid-word.
    pub fn in_speech(&self) -> bool {
        self.vad.in_speech()
    }
}

/// A running conversation-mode listener: a worker thread that drains the mic's
/// shared buffer through an [`UtteranceSegmenter`] and fires `on_utterance` with
/// each finished utterance. Stop it with [`stop`](Self::stop) (also fired on
/// drop, so an abandoned handle cannot leak a polling thread).
pub struct ConversationCapture {
    stop: Arc<AtomicBool>,
}

impl ConversationCapture {
    /// Spawn the polling worker. `buffer` is the capture's
    /// [`shared_buffer`](crate::MicCapture::shared_buffer); it is *drained* each
    /// poll (conversation mode owns it — dictation's snapshot readers are never
    /// active at the same time).
    pub fn spawn(
        buffer: SharedSamples,
        vad: Vad,
        mut on_utterance: impl FnMut(Vec<f32>) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            let mut segmenter = UtteranceSegmenter::new(vad);
            loop {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let fresh = {
                    let mut buf = buffer.lock().unwrap();
                    if buf.is_empty() {
                        continue;
                    }
                    std::mem::take(&mut *buf)
                };
                if let Some(utterance) = segmenter.feed(&fresh) {
                    on_utterance(utterance);
                }
            }
        });
        Self { stop }
    }

    /// Ask the worker to exit. Idempotent; the thread notices within one poll
    /// interval.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for ConversationCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::WINDOW_SAMPLES;
    use std::sync::{Arc, Mutex};

    fn windows(amplitude: f32, n: usize) -> Vec<f32> {
        vec![amplitude; n * WINDOW_SAMPLES]
    }

    #[test]
    fn segmenter_returns_the_accumulated_utterance_on_endpoint() {
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 5));
        assert!(seg.feed(&windows(0.1, 3)).is_none(), "speech still open");
        assert!(seg.feed(&windows(0.0, 4)).is_none(), "endpoint not reached");
        let utt = seg
            .feed(&windows(0.0, 1))
            .expect("endpoint closes the utterance");
        // Everything fed since the start is in the buffer: 3 speech + 5 silence.
        assert_eq!(utt.len(), 8 * WINDOW_SAMPLES);
        assert!(
            utt[..WINDOW_SAMPLES].iter().any(|s| *s > 0.05),
            "speech included"
        );
    }

    #[test]
    fn segmenter_keeps_only_preroll_during_silence() {
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 5));
        // A minute of silence must not accumulate: the buffer stays at preroll.
        for _ in 0..3_000 {
            assert!(seg.feed(&windows(0.0, 1)).is_none());
        }
        assert!(seg.acc.len() <= PREROLL_SAMPLES);
    }

    #[test]
    fn preroll_kept_during_silence_is_the_most_recent_not_the_oldest() {
        // The preroll exists to keep the audio *immediately before* speech onset
        // so the first syllable isn't clipped — so the silence trim must keep the
        // NEWEST PREROLL_SAMPLES, not the oldest. Every other test feeds all-0.0
        // silence, where "keep newest" and "keep oldest" are indistinguishable
        // (same length, identical content), so nothing pins the drain direction:
        // mutating `drain(..len - PREROLL_SAMPLES)` to `drain(PREROLL_SAMPLES..)`
        // keeps the same COUNT of samples but the wrong ones, and survives.
        //
        // Feed two distinguishable-but-still-silent markers below the threshold:
        // an old 0.001 block big enough to fill past preroll, then a fresh 0.002
        // block of exactly PREROLL_SAMPLES. After trimming, the accumulator must
        // hold only the newest marker, so the utterance's preroll is all 0.002.
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 2));
        assert!(
            seg.feed(&windows(0.001, 60)).is_none(),
            "0.001 is below threshold: silence"
        );
        assert!(
            seg.feed(&windows(0.002, 25)).is_none(),
            "0.002 is below threshold: silence"
        );
        // 25 windows == exactly PREROLL_SAMPLES, so the newest preroll is all 0.002.
        assert!(
            seg.feed(&windows(0.1, 2)).is_none(),
            "speech opens, no endpoint yet"
        );
        let utt = seg
            .feed(&windows(0.0, 2))
            .expect("endpoint closes the utterance");
        assert!(
            utt[..PREROLL_SAMPLES].iter().all(|&s| s == 0.002),
            "preroll must be the MOST RECENT silence before speech (0.002), not the oldest (0.001)"
        );
    }

    #[test]
    fn segmenter_preroll_precedes_the_speech_in_the_utterance() {
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 2));
        // Idle silence first (trimmed to preroll), then speech, then endpoint.
        for _ in 0..50 {
            seg.feed(&windows(0.0, 1));
        }
        seg.feed(&windows(0.1, 2));
        let utt = seg.feed(&windows(0.0, 2)).expect("endpoint");
        // preroll + 2 speech + 2 silence windows.
        assert_eq!(utt.len(), PREROLL_SAMPLES + 4 * WINDOW_SAMPLES);
        assert_eq!(utt[0], 0.0, "utterance starts with the kept preroll");
    }

    #[test]
    fn sustained_speech_force_flushes_at_the_cap() {
        // Codex SF3: noise that keeps window RMS above the threshold never
        // reaches a silence endpoint — the segmenter must flush at
        // MAX_UTTERANCE_SAMPLES instead of growing ~64 KB/s forever.
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 30));
        let mut flushed = None;
        let mut fed = 0;
        while flushed.is_none() {
            flushed = seg.feed(&windows(0.1, 3));
            fed += 3 * WINDOW_SAMPLES;
            assert!(
                seg.acc.len() < MAX_UTTERANCE_SAMPLES,
                "the accumulator must never sit above the cap"
            );
            assert!(
                fed <= MAX_UTTERANCE_SAMPLES + 3 * WINDOW_SAMPLES,
                "cap never fired"
            );
        }
        let utt = flushed.unwrap();
        // The flush is prompt: within one feed slice of the ceiling.
        assert!(utt.len() >= MAX_UTTERANCE_SAMPLES);
        assert!(utt.len() < MAX_UTTERANCE_SAMPLES + 3 * WINDOW_SAMPLES);
        assert!(
            seg.acc.is_empty(),
            "the accumulator restarts after the forced flush"
        );
    }

    #[test]
    fn endpoint_still_fires_after_a_forced_flush() {
        // The forced flush must not wedge the loop: speech continuing past the
        // cap ships its remainder on a normal silence endpoint.
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 5));
        while seg.feed(&windows(0.1, 10)).is_none() {}
        seg.feed(&windows(0.1, 2));
        let rest = seg
            .feed(&windows(0.0, 5))
            .expect("endpoint after the flush");
        // Exactly what was fed since the flush: 2 speech + 5 silence windows.
        assert_eq!(rest.len(), 7 * WINDOW_SAMPLES);
    }

    #[test]
    fn capture_worker_fires_callback_with_the_utterance() {
        // End-to-end across the thread: fill a shared buffer the way a mic
        // callback would, and the worker must drain it, detect the endpoint,
        // and hand the utterance to the callback.
        let buffer: SharedSamples = Arc::new(Mutex::new(Vec::new()));
        let got: Arc<Mutex<Vec<Vec<f32>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        let cap = ConversationCapture::spawn(buffer.clone(), Vad::new(0.01, 5), move |u| {
            sink.lock().unwrap().push(u);
        });
        {
            let mut b = buffer.lock().unwrap();
            b.extend(windows(0.1, 3));
            b.extend(windows(0.0, 5));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while got.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        cap.stop();
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1, "exactly one utterance");
        assert_eq!(got[0].len(), 8 * WINDOW_SAMPLES);
        assert!(
            buffer.lock().unwrap().is_empty(),
            "worker drains the shared buffer"
        );
    }

    #[test]
    fn stopped_worker_stops_consuming() {
        let buffer: SharedSamples = Arc::new(Mutex::new(Vec::new()));
        let cap = ConversationCapture::spawn(buffer.clone(), Vad::default(), |_| {});
        cap.stop();
        // Give the thread time to exit, then verify it no longer drains.
        std::thread::sleep(Duration::from_millis(200));
        buffer.lock().unwrap().extend(windows(0.0, 2));
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !buffer.lock().unwrap().is_empty(),
            "a stopped worker must not drain"
        );
    }
}
