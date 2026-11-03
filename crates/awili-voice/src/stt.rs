//! Speech-to-text via whisper-rs (whisper.cpp bindings).
//!
//! Compiles on every platform — the slice-3 DoD requires whisper-rs to build on
//! Linux — while the Metal GPU backend is enabled only on macOS through the
//! crate's `metal` feature (see `Cargo.toml`). The engine is a thin wrapper: load
//! a model once, then [`transcribe`](Whisper::transcribe) a buffer of 16 kHz mono
//! `f32` samples (whisper's required input format — the cpal capture path
//! resamples to this).
//!
//! Nothing here is exercised by `cargo test` (it needs a multi-hundred-MB model
//! and real audio); it is smoke-tested live on the Mac. The unit tests cover the
//! pure helper that flattens whisper's segment list into one transcript string.

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Whisper's required input sample rate. cpal capture resamples to this.
pub const SAMPLE_RATE: u32 = 16_000;

/// A loaded whisper model ready to transcribe. Holds the (immutable, shareable)
/// context; a fresh decode state is created per [`transcribe`](Self::transcribe)
/// call so the type can be `&self`-shared across the capture and UI threads.
pub struct Whisper {
    ctx: WhisperContext,
}

/// Why transcription failed.
#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("loading whisper model: {0}")]
    Load(String),
    #[error("whisper transcription: {0}")]
    Run(String),
}

impl Whisper {
    /// Load a model from a `ggml-*.bin` file. Expensive (hundreds of MB mmapped) —
    /// do it once on a worker thread, not per utterance.
    pub fn load(model_path: &str) -> Result<Self, SttError> {
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .map_err(|e| SttError::Load(format!("{e:?}")))?;
        Ok(Self { ctx })
    }

    /// Transcribe a buffer of 16 kHz mono `f32` samples to text. English-only and
    /// tuned for short push-to-talk utterances: greedy sampling, no timestamps,
    /// quiet (the C library's stdout chatter is suppressed).
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, SttError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| SttError::Run(format!("{e:?}")))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Use the available cores but leave headroom for the UI thread.
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        params.set_n_threads(threads.saturating_sub(1).max(1) as i32);

        state
            .full(params, samples)
            .map_err(|e| SttError::Run(format!("{e:?}")))?;
        let n = state
            .full_n_segments()
            .map_err(|e| SttError::Run(format!("{e:?}")))?;
        let segments = (0..n)
            .map(|i| {
                state
                    .full_get_segment_text(i)
                    .map_err(|e| SttError::Run(format!("{e:?}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(join_segments(&segments))
    }
}

/// Join whisper's per-segment texts into one transcript: trim each segment's
/// surrounding whitespace (whisper emits leading spaces) and join with single
/// spaces, collapsing the result. Pure, so it is unit-tested without a model.
pub fn join_segments(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_join_with_single_spaces_and_trim() {
        let segs = vec![" Hello".to_string(), " world.".to_string()];
        assert_eq!(join_segments(&segs), "Hello world.");
    }

    #[test]
    fn empty_and_blank_segments_drop_out() {
        let segs = vec!["".to_string(), "  ".to_string(), "ok".to_string()];
        assert_eq!(join_segments(&segs), "ok");
        assert_eq!(join_segments(&[]), "");
    }

    #[test]
    fn segment_trailing_whitespace_is_trimmed_not_just_leading() {
        // Whisper emits *leading* spaces, so the existing tests only ever exercise
        // leading-whitespace trimming; a mutation weakening `trim()` to
        // `trim_start()` would survive them. But a segment can also carry trailing
        // whitespace — left in, it doubles the join space (or trails the final
        // transcript). Pin that `trim()` strips both ends.
        let segs = vec![" mid ".to_string(), "end".to_string()];
        assert_eq!(join_segments(&segs), "mid end");
    }
}
