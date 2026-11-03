//! `awili-voice` — slice-3 voice: dictation in, read-aloud out.
//!
//! Some users have limited hand mobility; voice is the *primary* input path, not polish
//! (slice-3 spec preamble). This crate provides the two halves:
//!
//! - **Dictation (STT).** [`stt::Whisper`] wraps whisper-rs (whisper.cpp); a model
//!   is [auto-downloaded](model::ensure_model) on first use. On macOS,
//!   [`capture::MicCapture`] records the mic via cpal and resamples to whisper's
//!   16 kHz. The transcript is inserted into the composer's [`EditBuffer`] through
//!   [`transcript::Dictation`] — partials as ghost (marked) text, the final
//!   transcript committed through the buffer's public API, so typing still works
//!   mid-dictation.
//! - **Read-aloud (TTS).** [`tts::TtsBackend`] has a macOS `say` fallback and a
//!   configurable HTTP endpoint; [`chunk::SentenceChunker`] splits a streaming
//!   reply into sentences and [`tts::speak_all`] plays them with a stop button.
//!
//! Everything compiles on Linux for CI (whisper-rs included); only the cpal
//! capture path is `cfg`-gated to macOS, and the `say`/`afplay` playback is mac at
//! runtime. The pure pieces — config, transcript insertion, the chunker, model
//! path/url resolution, the resampler — are unit-tested here.

pub mod audio;
pub mod capture;
pub mod chunk;
pub mod command;
pub mod config;
pub mod conversation;
pub mod handsfree;
pub mod model;
pub mod permission;
pub mod stt;
pub mod transcript;
pub mod tts;
pub mod vad;
pub mod wake;

pub use capture::{CaptureError, MicCapture};
pub use chunk::SentenceChunker;
pub use command::{classify as classify_command, VoiceCommand};
pub use config::{
    ModelSpec, PttKey, PttModifier, VoiceConfig, VAD_SILENCE_WINDOWS_MAX, VAD_SILENCE_WINDOWS_MIN,
    VAD_THRESHOLD_MAX, VAD_THRESHOLD_MIN,
};
pub use conversation::{ConversationCapture, UtteranceSegmenter};
pub use handsfree::{HandsFreeEvent, HandsFreeSession};
pub use model::{ensure_model, ModelError, Progress};
pub use stt::{SttError, Whisper};
pub use transcript::Dictation;
pub use tts::{
    backend_for, speak_all, CancelToken, HttpTts, SayTts, SpeechQueue, TtsBackend, TtsError,
};
pub use vad::{Vad, VadEvent};
pub use wake::{
    WakeEvent, WakeGate, WAKE_IDLE_TIMEOUT_SAMPLES_DEFAULT, WAKE_REFRACTORY_SAMPLES_DEFAULT,
    WAKE_THRESHOLD_DEFAULT,
};
