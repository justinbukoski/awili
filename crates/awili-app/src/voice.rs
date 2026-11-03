//! The app-side voice state and the blocking worker helpers that back it.
//!
//! GPUI orchestration (spawning workers, updating the composer) lives in
//! `main.rs`; this module holds the plain state container [`Voice`] and the
//! `Send`, off-the-UI-thread helpers the workers call — model loading and
//! transcription — so they stay testable and out of the render file.
//!
//! Capture and `say`/`afplay` playback are macOS at runtime; everything here
//! compiles on Linux (the [`MicCapture`] stub returns "unsupported"), so the whole
//! integration is type-checked by the Linux CI build.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use awili_voice::{
    classify_command, ensure_model, ConversationCapture, HandsFreeEvent, HandsFreeSession,
    MicCapture, ModelSpec, Progress, SpeechQueue, Vad, VadEvent, VoiceCommand, VoiceConfig,
    Whisper,
};

/// Lazily-loaded whisper model, shared between the loader thread (which fills it)
/// and the transcription workers (which read it).
pub type ModelSlot = Arc<Mutex<ModelState>>;

/// The loading lifecycle of the whisper model.
pub enum ModelState {
    /// Not yet requested.
    Idle,
    /// Download/load in progress; the payload is the download fraction so far
    /// (`0.0..=1.0`) so the mic/voice buttons can show progress (slice-5 §4 —
    /// the first click used to download hundreds of MB with no visual feedback).
    Loading(f32),
    /// Ready to transcribe.
    Ready(Arc<Whisper>),
    /// Load failed; the message is shown in the composer-row status.
    Failed(String),
}

impl ModelState {
    /// Whether a (re)load may be started: never yet attempted, or a previous
    /// attempt failed (so the user can retry by toggling the mic again).
    pub fn can_load(&self) -> bool {
        matches!(self, ModelState::Idle | ModelState::Failed(_))
    }

    /// The message from a prior failed load, for re-surfacing on retry.
    pub fn failure(&self) -> Option<&str> {
        match self {
            ModelState::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// The download fraction while loading (`0.0..=1.0`), `None` otherwise.
    pub fn progress(&self) -> Option<f32> {
        match self {
            ModelState::Loading(frac) => Some(*frac),
            _ => None,
        }
    }
}

/// Record a download [`Progress`] tick in the model slot, so render-time readers
/// see the fraction. Kept tiny and pure-ish so the Loading transitions are unit
/// tested without a network (slice-5 §4); an unknown total leaves the previous
/// fraction in place rather than jumping back to 0.
pub fn note_download_progress(slot: &ModelSlot, p: Progress) {
    if let Some(frac) = p.fraction() {
        let mut s = slot.lock().unwrap();
        if matches!(&*s, ModelState::Loading(_)) {
            *s = ModelState::Loading(frac);
        }
    }
}

/// Monotonic dictation-session ids (review slice-3 blocker 1). Every utterance
/// gets a fresh id from [`begin`](Self::begin); every transcription worker
/// carries the id of the session it serves, and its results are applied only
/// while [`is_current`](Self::is_current) — so a `Whisper::transcribe` already
/// in flight when its session ends can never overwrite a newer session's ghost
/// nor commit into a newer (or already-submitted) draft. Taking/submitting the
/// draft calls [`invalidate`](Self::invalidate), which retires every
/// outstanding id without starting a session.
#[derive(Debug, Default)]
pub struct SessionCounter {
    current: u64,
}

impl SessionCounter {
    /// Start a new session, retiring all previous ids. Returns the new id.
    pub fn begin(&mut self) -> u64 {
        self.current += 1;
        self.current
    }

    /// Retire every outstanding id without starting a session (draft taken,
    /// message submitted).
    pub fn invalidate(&mut self) {
        self.current += 1;
    }

    /// The id of the most recently begun session (what a stop-worker should
    /// stamp its final-transcript result with).
    pub fn current(&self) -> u64 {
        self.current
    }

    /// Whether results stamped with `id` may still be applied.
    pub fn is_current(&self, id: u64) -> bool {
        id == self.current
    }
}

/// All voice state hung off the app. Audio devices live here (never moved across
/// threads); workers get cheap `Arc` handles to the buffer and model slot.
pub struct Voice {
    pub cfg: VoiceConfig,
    /// The lazily-loaded model, shared with workers.
    pub model: ModelSlot,
    /// The active read-aloud queue, if speaking.
    pub speech: Option<SpeechQueue>,
    /// Whether read-aloud is currently playing (drives the stop button).
    pub speaking: bool,
    /// Monotonic id for the active read-aloud, so a stale "done" watcher from a
    /// superseded utterance does not clear a newer one.
    pub speech_gen: u64,
    /// `true` while the active read-aloud is the in-flight reply being streamed
    /// (auto-speak), so token chunks are fed to it.
    pub reply_streaming: bool,
    /// The conversation whose streaming reply owns [`speech`](Self::speech),
    /// set when the reply's read-aloud begins and cleared when it finishes or
    /// is stopped. `finish_turn` keys on THIS — not on the on-screen
    /// conversation — so a reply the user switched away from still closes its
    /// queue (dropping the sender so the worker exits and the `done` watcher
    /// can re-arm the conversation-mode mic), and another conversation's live
    /// queue is left alone (codex review SF1).
    pub reply_conv: Option<String>,
    /// Auto-speak every assistant reply (composer-row toggle).
    pub auto_speak: bool,
    /// Transient status shown by the mic chip (model download, errors).
    pub status: Option<String>,
    /// Whether a dictation session is active (mic on).
    pub dictating: bool,
    /// Generation gate for transcription results (review blocker 1): partial and
    /// final workers stamp their session id and stale results are dropped.
    pub session: SessionCounter,
    /// `true` when the current session was started by holding push-to-talk (so the
    /// modifier release ends it); `false` when started by the mic toggle button.
    pub ptt_session: bool,
    /// Last-seen state of the push-to-talk modifier, to detect down/up edges.
    pub ptt_held: bool,
    /// The U3b2 hold guard: turns the modifier edges (and typing while held)
    /// into PTT decisions, so a *tap* of the key never starts a dictation.
    /// `Default`-initiated; the `AwiliApp::ptt_*` hooks drive it.
    pub ptt_guard: crate::ptt_guard::PttGuard,
    /// Set to halt the partial-transcript polling worker.
    pub stop_partials: Arc<AtomicBool>,
    /// The live mic capture while dictating (macOS; a stub elsewhere). Held on the
    /// UI thread — the cpal stream is not `Send`.
    pub capture: Option<MicCapture>,
    /// A worker thread is currently raising the OS microphone-permission prompt
    /// (N1 pre-flight); further mic starts are no-ops until it resolves.
    pub mic_permission_pending: bool,
    // ---- slice 5 (conversation mode) ----
    /// Hands-free conversation loop active (the Voice toggle): VAD endpoints
    /// auto-submit, replies are spoken, and the mic re-arms after each reply.
    pub conversation_mode: bool,
    /// Hot-mic ceremony (Codex P0, 2026-08-22): `false` until the human has
    /// SENT the first voice utterance of this conversation-mode session
    /// themselves. Until then nothing is auto-submitted — the first transcript
    /// is staged into the composer and waits for Enter. Reset on every arm.
    pub first_send_confirmed: bool,
    /// The mic feeding conversation mode, while listening. Separate from
    /// [`capture`](Self::capture) so the dictation paths never fight over it.
    pub conv_mic: Option<Mic>,
    /// The VAD polling worker, while listening; stopping it ends the thread.
    pub conv_capture: Option<ConversationCapture>,
    // ---- RC2-R4 (F0956): barge-in while a reply is read aloud ----
    /// The talk-over listener (its own mic), only in conversation mode while
    /// Awili reads a reply; never at the same time as [`conv_mic`](Self::conv_mic).
    pub barge: Option<BargeRun>,
    /// Bumped whenever the listener starts or stops, so a late transcript
    /// from an earlier reply is dropped.
    pub barge_gen: u64,
    /// The sentences of the reply being read aloud (queued so far): a
    /// talk-over made only of them is Awili's own voice.
    pub spoken: Vec<String>,
    /// RC2-R4M2: tests only: when set, every conversation and talk-over mic
    /// is a stand-in (see [`Mic`]) and the OS permission is not asked.
    #[cfg(test)]
    pub test_mics: Option<TestMics>,
    /// RC2-R4M2: tests only: the read-aloud voice (instead of the configured one).
    #[cfg(test)]
    pub test_tts: Option<fn() -> Box<dyn awili_voice::tts::TtsBackend>>,
}

impl Voice {
    pub fn new(cfg: VoiceConfig) -> Self {
        let auto_speak = cfg.auto_speak;
        Self {
            cfg,
            model: Arc::new(Mutex::new(ModelState::Idle)),
            speech: None,
            speaking: false,
            mic_permission_pending: false,
            speech_gen: 0,
            reply_streaming: false,
            reply_conv: None,
            auto_speak,
            status: None,
            dictating: false,
            session: SessionCounter::default(),
            ptt_session: false,
            ptt_held: false,
            ptt_guard: crate::ptt_guard::PttGuard::default(),
            stop_partials: Arc::new(AtomicBool::new(false)),
            capture: None,
            conversation_mode: false,
            first_send_confirmed: false,
            conv_mic: None,
            conv_capture: None,
            barge: None,
            barge_gen: 0,
            spoken: Vec::new(),
            #[cfg(test)]
            test_mics: None,
            #[cfg(test)]
            test_tts: None,
        }
    }
}

/// What conversation mode should do with a finished utterance's transcript
/// (slice-5 spec §3). Pure, so the stop-word and submit decisions are unit
/// tested without audio or a UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceTurnAction {
    /// Submit the transcript as a user turn.
    Submit(String),
    /// A stop word — leave conversation mode without submitting.
    Exit,
    /// Nothing usable was said; keep listening.
    Ignore,
}

/// Strip whisper's non-speech *annotations* from a raw transcript before it is
/// treated as a spoken turn. On near-silence — common in conversation mode, where
/// the mic is held open *through* the read-aloud reply — whisper emits bracketed
/// or parenthesised sound tags as ordinary segment text: `[BLANK_AUDIO]`,
/// `[ Silence ]`, `(typing)`, `[MUSIC]`. (`set_print_special(false)` in
/// [`Whisper::transcribe`](awili_voice::Whisper::transcribe) suppresses whisper's
/// *special tokens*, not these text-level annotations.) Left in, a pure-silence
/// window transcribes to one such tag and would be *submitted as a user turn* —
/// garbage to the model, and with barge-in the system voice could even trip it.
///
/// Only bracketed/parenthesised spans are removed: a real speaker never utters a
/// literal `[` or `(`, while whisper's annotations are *always* delimited, so the
/// rule only ever drops text *inside* a delimiter span — never bare spoken words.
/// (Whisper text it chooses to bracket is removed wholesale, which is the intent.)
/// Bare hallucinations like "you" or "Thank you."
/// are deliberately NOT touched — they collide one-for-one with real one-word
/// utterances, and an all-annotation transcript already collapses to "" here,
/// which [`voice_turn_action`] maps to [`VoiceTurnAction::Ignore`].
///
/// Returns the speech with every annotation span removed and surrounding/internal
/// whitespace collapsed to single spaces. An unterminated opener (a truncated
/// annotation) is dropped through end-of-string.
pub fn strip_voice_annotations(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        let closer = match chars[i] {
            '[' => Some(']'),
            '(' => Some(')'),
            _ => None,
        };
        match closer {
            Some(closer) => {
                // Skip the whole delimited span (to the matching closer of the
                // same kind, or to end-of-string if whisper truncated it).
                let mut j = i + 1;
                while j < chars.len() && chars[j] != closer {
                    j += 1;
                }
                i = if j < chars.len() { j + 1 } else { chars.len() };
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decide what to do with a conversation-mode transcript. Whisper's non-speech
/// annotations are stripped first (see [`strip_voice_annotations`]). Stop words
/// ("stop", "cancel", "exit", "quit" — case-insensitive, surrounding punctuation
/// ignored, because whisper typically renders them as e.g. "Stop.") exit the
/// loop; anything else non-empty is submitted as spoken. RC2-UX (review
/// F0956, app half): every way of saying stop that the voice crate knows
/// ("stop talking", "shut up", "that's enough", "Awili, stop", "stop stop")
/// exits as well — `on_voice_transcript` acts on this decision, so without it
/// those phrases were sent to the model as a message.
pub fn voice_turn_action(transcript: &str) -> VoiceTurnAction {
    let stripped = strip_voice_annotations(transcript);
    let text = stripped.trim();
    if text.is_empty() {
        return VoiceTurnAction::Ignore;
    }
    if classify_command(text) == Some(VoiceCommand::Stop) {
        return VoiceTurnAction::Exit;
    }
    let normalized: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .trim()
        .to_lowercase();
    match normalized.as_str() {
        "stop" | "cancel" | "exit" | "quit" => VoiceTurnAction::Exit,
        "" => VoiceTurnAction::Ignore, // punctuation-only "transcript"
        _ => VoiceTurnAction::Submit(text.to_string()),
    }
}

/// What a finished hands-free utterance resolves to, once both the verbal-command
/// classifier and the dictation path have had a say — the single decision the
/// [`TurnController`]-driven loop acts on per completed turn.
///
/// This is the unification of the two overlapping kernels that grew up
/// separately: [`voice_turn_action`] (the slice-3 dictation decision —
/// Submit / Exit / Ignore, where the only "control" word was the Exit stop word)
/// and [`classify_command`](awili_voice::classify_command) (the slice-5 verbal
/// *control* set — Stop / Send / Scratch / Repeat / NewConversation). A hands-free
/// user says both kinds of thing into the same mic, so one resolver has to route
/// the utterance to exactly one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TurnDirective {
    /// A recognized hands-free control command — act on the app (halt the
    /// read-aloud, submit the draft, discard it, re-read the reply, start fresh),
    /// do *not* send it to the model.
    Command(VoiceCommand),
    /// Dictation: submit this text as a user turn.
    Submit(String),
    /// Nothing usable was said (silence, or whisper's non-speech annotations
    /// that strip to nothing); keep listening. A bare address word like "awili"
    /// with no command core is *not* ignored here — the classifier returns
    /// `None` for it and it falls through to dictation, matching
    /// [`voice_turn_action`]'s pre-existing behaviour (see the tests).
    Ignore,
}

/// Resolve a finished conversation-mode utterance into the one thing the turn
/// loop should do. Whisper's non-speech annotations are stripped first (shared
/// with [`voice_turn_action`]), then the **verbal-command classifier is given
/// precedence**: a whole-utterance control phrase ("stop", "send it", "scratch
/// that", "repeat", "new conversation") becomes a [`TurnDirective::Command`], so
/// hands-free control never has to round-trip through the model. Only if the
/// utterance is *not* a command does it fall through to the dictation decision —
/// where [`voice_turn_action`]'s legacy Exit words ("exit"/"quit", which the
/// command classifier deliberately does not claim) still resolve to a
/// [`VoiceCommand::Stop`], keeping the stop-the-conversation behaviour intact,
/// and any other non-empty transcript is [`TurnDirective::Submit`].
///
/// The precedence order is the whole point: the classifier's whole-utterance
/// guard means a *dictated sentence* that merely contains a command word ("please
/// don't stop helping me") returns `None` there and is correctly submitted as
/// dictation — so giving commands precedence cannot swallow real speech. Pure and
/// Linux-CI-tested, like the kernels it composes; GPUI orchestration of each
/// directive (stopping the [`SpeechQueue`], clearing the composer, re-reading the
/// last reply, opening a new conversation) is the later [LIVE] slice.
pub fn resolve_turn(transcript: &str) -> TurnDirective {
    let stripped = strip_voice_annotations(transcript);
    if stripped.trim().is_empty() {
        return TurnDirective::Ignore;
    }
    if let Some(cmd) = classify_command(&stripped) {
        return TurnDirective::Command(cmd);
    }
    match voice_turn_action(&stripped) {
        // "exit"/"quit": not in the command classifier's vocabulary, but the
        // slice-3 stop words mean the same as Stop here — halt the read-aloud /
        // leave the exchange. Fold them into the unified command set.
        VoiceTurnAction::Exit => TurnDirective::Command(VoiceCommand::Stop),
        VoiceTurnAction::Submit(text) => TurnDirective::Submit(text),
        VoiceTurnAction::Ignore => TurnDirective::Ignore,
    }
}

/// Whether a typed draft in the composer blocks a conversation-mode auto-submit
/// (codex review SF2). `dictation_commit` with no ghost splices the transcript
/// into the buffer at the caret, so a half-typed thought would be merged with
/// the utterance and sent. A non-empty draft means the keyboard is mid-thought:
/// leave it alone and drop the utterance. Whitespace-only is not a thought.
pub fn draft_blocks_voice_submit(draft: &str) -> bool {
    !draft.trim().is_empty()
}

/// Resolve a hands-free **"send it"** command ([`VoiceCommand::Send`] →
/// [`TurnAction::SendDraft`], mem 3049 voice priority #1) against the live
/// composer draft. "Send it" submits whatever is already in the composer — but an
/// *empty* composer has nothing to send, and firing the submit anyway posts a
/// blank user turn (the gateway rejects it, or an empty message reaches the
/// model). So this folds the command to [`TurnAction::KeepListening`] when the
/// draft carries no thought — floor unchanged, mic still live, no turn lost and
/// no blank turn posted — and only yields [`TurnAction::SendDraft`] when there is
/// something to send. Same "a dropped/empty input must not silently lose or
/// corrupt the turn" discipline as [`TurnController::resolve_utterance`]'s
/// empty-transcription fold, one command down. It is the pure guard the [LIVE]
/// send-wiring consults before acting on a [`TurnAction::SendDraft`] (which
/// [`resolve`](TurnController::resolve) can only emit unconditionally, the draft
/// being app state it cannot see).
///
/// **Polarity note — do not reuse [`draft_blocks_voice_submit`] for this.** That
/// guard answers the *opposite* question on the *dictation auto-submit* path
/// (SF2): a non-empty draft BLOCKS splicing a spoken utterance into a half-typed
/// thought. Numerically the two predicates coincide — both key off "the draft has
/// non-whitespace content", deliberately the same rule so they agree on what
/// counts as content — but their intent is inverted: a non-empty draft is exactly
/// what *fires* an explicit "send it" and exactly what *stands down* a dictation
/// auto-submit. Each intent gets its own resolver so a caller cannot wire one with
/// the other's polarity.
#[allow(dead_code)] // wired by the voice controller (ears-tuned slice)
pub fn resolve_send_draft(draft: &str) -> TurnAction {
    if draft.trim().is_empty() {
        TurnAction::KeepListening
    } else {
        TurnAction::SendDraft
    }
}

/// Resolve a hands-free **"repeat"** command ([`VoiceCommand::Repeat`] →
/// [`TurnAction::Repeat`], mem 3049 voice priority #1) against whether there is any
/// reply to re-read. "Repeat"/"say that again" re-reads the last reply aloud — but
/// before Awili has ever spoken (a fresh conversation, or right after a
/// [`reset`](TurnController::reset)), there is nothing to re-read, and firing
/// [`TurnAction::Repeat`] would tell the [LIVE] wiring to re-synthesise an empty
/// reply. So this folds the command to [`TurnAction::KeepListening`] when no reply
/// has ended yet — floor unchanged, mic still live, no turn lost — and only yields
/// [`TurnAction::Repeat`] once there is a reply outcome to pick up from. It is the
/// same "a command with nothing to act on must not fire a degenerate action" guard
/// as [`resolve_send_draft`] ("send it" on an empty composer), one command over.
///
/// This is the consumer [`ReplyOutcome`] was recorded for: the controller keeps
/// [`last_reply_outcome`](TurnController::last_reply_outcome) precisely so a later
/// "repeat"/"resume" decision knows whether there is anything to pick up from.
/// *Either* recorded outcome is repeatable — [`Drained`](ReplyOutcome::Drained)
/// re-reads the whole reply from the top, [`Interrupted`](ReplyOutcome::Interrupted)
/// gives the ears-tuned resume a real pick-up point — so both map to
/// [`TurnAction::Repeat`]; only `None` (no reply yet) is not. Choosing *how* to
/// re-read from that outcome (from-the-top vs resume-from-pickup) stays the deferred
/// ears-tuned [LIVE] slice; this pure guard only decides whether the re-read fires
/// at all, the way [`resolve_send_draft`] decides only whether the send fires.
#[allow(dead_code)] // wired by the voice controller (ears-tuned slice)
pub fn resolve_repeat(last_reply_outcome: Option<ReplyOutcome>) -> TurnAction {
    match last_reply_outcome {
        Some(_) => TurnAction::Repeat,
        None => TurnAction::KeepListening,
    }
}

/// *How* a fired "repeat"/"resume" should re-read the last reply — the pure branch
/// [`resolve_repeat`]'s go/no-go boolean deliberately throws away. Once
/// [`resolve_repeat`] has decided the re-read *fires* (there is a reply outcome to
/// pick up from), the recorded [`ReplyOutcome`] still says *which kind* of re-read it
/// is, and that two-way branch is itself pure: a drained reply has no interruption to
/// resume from, an interrupted one does. Naming it lets the [LIVE] wiring switch on a
/// mode instead of re-inspecting
/// [`last_reply_outcome`](TurnController::last_reply_outcome) a second time after
/// already learning, from [`resolve_repeat`], that a repeat fires.
///
/// Only the *pickup point* inside [`Resume`](Self::Resume) — which sample or word the
/// resumed read-aloud restarts from — needs Owner's ears; that stays the deferred
/// [LIVE] slice. This enum is exactly the code-verifiable half of barge-in-then-resume
/// that [`ReplyOutcome::Interrupted`] was recorded to carry, split off ahead of the
/// ears-tuned wiring the same way [`BargeInDetector`] is split off ahead of the audio
/// plumbing that will tune its `sustain_windows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RepeatMode {
    /// The reply drained on its own ([`ReplyOutcome::Drained`]): there is no
    /// interruption to pick up from, so "repeat" re-reads the whole reply from the
    /// top.
    FromTop,
    /// The reply was cut mid-playback ([`ReplyOutcome::Interrupted`]):
    /// "repeat"/"resume" picks up from where the barge-in stopped it. The exact
    /// pickup point is the ears-tuned [LIVE] detail; this only names that a resume —
    /// not a from-the-top re-read — is what the outcome calls for.
    Resume,
}

/// Classify *how* a fired "repeat"/"resume" should re-read, from the last reply's
/// recorded [`ReplyOutcome`]. This is the natural refinement of [`resolve_repeat`]:
/// where that decides *whether* the re-read fires at all (`Some` outcome →
/// [`TurnAction::Repeat`], `None` → keep listening), this decides *which kind* once it
/// does — [`ReplyOutcome::Drained`] → [`RepeatMode::FromTop`],
/// [`ReplyOutcome::Interrupted`] → [`RepeatMode::Resume`].
///
/// `None` (no reply has ended yet) has no mode — the caller should not have fired a
/// repeat, since [`resolve_repeat`] would have folded it to
/// [`KeepListening`](TurnAction::KeepListening) — so it returns `None`, keeping this
/// in exact lockstep with that go/no-go: `repeat_mode(o).is_some()` holds for the same
/// `o` as `resolve_repeat(o) == TurnAction::Repeat`. A caller reads it straight off the
/// controller, symmetric with the guard: `repeat_mode(ctl.last_reply_outcome())`.
#[allow(dead_code)] // wired by the voice controller (ears-tuned slice)
pub fn repeat_mode(last_reply_outcome: Option<ReplyOutcome>) -> Option<RepeatMode> {
    match last_reply_outcome {
        Some(ReplyOutcome::Drained) => Some(RepeatMode::FromTop),
        Some(ReplyOutcome::Interrupted) => Some(RepeatMode::Resume),
        None => None,
    }
}

/// The decision half of *barge-in* (mem 3049, voice priority #1): while a
/// read-aloud reply is playing, sustained user speech on the still-open
/// conversation mic should cancel the TTS and hand the turn back — without a stop
/// word or a button press. The audio plumbing (keeping the conversation mic open
/// *through* playback and routing its [`VadEvent`]s here, then calling
/// [`SpeechQueue::stop`] on a `true`) and the *value* of `sustain_windows` need
/// Owner's ears to tune against his mic picking up the system voice; the state
/// machine that turns those events into a single, debounced interrupt does not.
/// So it is built and proven here ahead of that wiring — the same eyes/ears-free
/// kernel split used for the terminal-copy `cell_at_pixel`/`classify_copy_key`
/// pieces. No production caller yet (hence `dead_code`); the conversation-mode
/// wiring is the ears-dependent follow-up.
///
/// What the kernel pins down:
/// * **Only during playback.** Speech while not `speaking` is ordinary hands-free
///   listening, never a barge-in: `feed` returns `false` and stays disarmed.
/// * **Sustained, not a blip.** An interrupt fires only after `sustain_windows`
///   *consecutive* in-speech windows, so a single echo of the system's own TTS,
///   a cough, or a one-window "mhm" backchannel cannot interrupt. Any real
///   silence resets the run — the [`Vad`](awili_voice::Vad) hangover already
///   bridges intra-word dips, so a [`VadEvent::Silence`]/[`VadEvent::SilenceEndpoint`]
///   reaching here is a genuine gap.
/// * **Once per playback.** After it fires it latches, so it does not re-emit an
///   interrupt every window while the async TTS stop is still in flight; the next
///   `speaking == false` feed re-arms it for the following reply.
#[derive(Debug)]
#[allow(dead_code)]
pub struct BargeInDetector {
    /// Consecutive in-speech windows required to treat playback-time speech as a
    /// deliberate interruption. Tunable by ear (echo of the system voice, brief
    /// backchannels). Clamped to `>= 1`.
    sustain_windows: u32,
    /// Consecutive in-speech windows seen so far during the current playback.
    run: u32,
    /// Latched once an interrupt has fired for this playback, cleared only when a
    /// `speaking == false` feed re-arms it.
    fired: bool,
}

#[allow(dead_code)]
impl BargeInDetector {
    /// `sustain_windows` is the consecutive in-speech [`Vad`](awili_voice::Vad)
    /// windows required before playback-time speech counts as a barge-in (clamped
    /// to `>= 1`; 0 would interrupt on the first noisy window).
    pub fn new(sustain_windows: u32) -> Self {
        Self {
            sustain_windows: sustain_windows.max(1),
            run: 0,
            fired: false,
        }
    }

    /// Feed one [`Vad::feed`](awili_voice::Vad::feed) result together with whether
    /// a read-aloud is currently `speaking`. Returns `true` exactly once — on the
    /// window where sustained speech first crosses the threshold during playback —
    /// and `false` otherwise.
    pub fn feed(&mut self, speaking: bool, ev: VadEvent) -> bool {
        if !speaking {
            // Not playing: re-arm for the next reply, never barge in.
            self.run = 0;
            self.fired = false;
            return false;
        }
        match ev {
            VadEvent::SpeechStart | VadEvent::SpeechContinue => {
                self.run = self.run.saturating_add(1);
                if !self.fired && self.run >= self.sustain_windows {
                    self.fired = true;
                    return true;
                }
                false
            }
            VadEvent::Silence | VadEvent::SilenceEndpoint => {
                // A real gap (post-hangover): the run must be *consecutive*, so a
                // lone echo blip cannot accumulate toward an interrupt.
                self.run = 0;
                false
            }
        }
    }

    /// Whether an interrupt has already fired for the current playback (latched
    /// until the next `speaking == false` feed re-arms it). Lets the wiring skip a
    /// second [`SpeechQueue::stop`] while the first cancel is still in flight.
    pub fn has_fired(&self) -> bool {
        self.fired
    }
}

/// Which participant holds the conversational floor — the *pure phase* of the
/// hands-free turn loop, distinct from the per-feed [`TurnEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Floor {
    /// The user has (or is being offered) the floor: audio is wake-gated and
    /// routed to the [`HandsFreeSession`], which captures a whole utterance.
    User,
    /// Awili holds the floor: a read-aloud reply is playing. Audio is watched
    /// *only* for a barge-in; the wake gate is deliberately not fed, so Awili's
    /// own voice bleeding into the mic can never re-trigger it.
    Assistant,
}

/// How the most recently *ended* read-aloud reply left Awili's floor — the one
/// bit of history the controller keeps so a later "repeat"/"resume" decision
/// knows whether there is anything to pick up from.
///
/// This is the pure, code-verifiable half of *barge-in-then-resume* (mem 3049,
/// voice priority #1): the controller records **why** the assistant floor ended,
/// but the actual audio resume (re-reading from where playback stopped, or
/// re-synthesising the whole reply) is deliberately deferred to the ears-tuned
/// [`TurnAction::Repeat`] wiring — it needs Owner's ears, not a CI test.
///
/// The state has a clean invariant against the floor: it is `Some` only while
/// the user holds the floor *after a reply ended*. It is `None` before any reply
/// and, crucially, `None` while a reply is in flight ([`begin_reply`] clears the
/// prior outcome), so a consumer never mistakes a stale interruption for the
/// live reply's fate. A fresh conversation ([`reset`]) clears it too.
///
/// [`begin_reply`]: TurnController::begin_reply
/// [`reset`]: TurnController::reset
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ReplyOutcome {
    /// The read-aloud finished on its own ([`reply_finished`]) — nothing pending;
    /// a "repeat" would re-read the whole reply from the top.
    ///
    /// [`reply_finished`]: TurnController::reply_finished
    Drained,
    /// Sustained user speech interrupted the read-aloud ([`TurnEvent::BargeIn`]) —
    /// the reply was cut mid-playback, so a "resume"/"repeat" has a real
    /// pick-up point for the ears-tuned wiring to act on.
    Interrupted,
}

/// What one [`TurnController::feed`] did — the *most significant* transition,
/// mirroring the collapse-to-one-event convention of the sub-machines it drives
/// ([`HandsFreeEvent`], [`Vad::feed`](awili_voice::Vad::feed)). For the resting
/// question "who holds the floor right now?" query
/// [`TurnController::assistant_speaking`] instead.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum TurnEvent {
    /// User floor, asleep: no wake word, nothing captured this feed.
    Idle,
    /// The wake word just fired: the user's turn opened and capture began.
    Activated,
    /// User floor, capturing, but no utterance endpointed this feed.
    Listening,
    /// A complete user utterance is ready to transcribe and submit. The floor
    /// stays with the user (the session remains armed for a follow-up) until the
    /// caller starts the reply's read-aloud via [`begin_reply`](TurnController::begin_reply).
    Utterance(Vec<f32>),
    /// Assistant floor: the reply is playing and no barge-in this feed.
    Speaking,
    /// Sustained user speech interrupted the reply. The caller should stop the
    /// read-aloud ([`SpeechQueue::stop`]); the floor has already returned to the
    /// user (wake-gated — the barging audio is not itself captured, an
    /// ears-tuned follow-up decides whether to auto-open capture).
    BargeIn,
}

/// The app-level outcome of resolving a *captured, transcribed* user utterance
/// through [`resolve_turn`], together with any controller-floor state change the
/// decision implies (already applied by [`TurnController::resolve`] before it
/// returns). Where [`TurnEvent`] is the per-feed audio phase, `TurnAction` is what
/// the loop does once a whole utterance has been Whisper'd back to text — the
/// [`TurnDirective`] lifted into the controller so a hands-free "stop" or "new
/// conversation" is not just classified but *acted on the floor*.
///
/// The controller only owns the state that is pure and code-verifiable — closing
/// the capture mic on Stop, resetting to a cold user floor on NewConversation. The
/// user-visible effect of each action (halting the [`SpeechQueue`], submitting or
/// clearing the composer draft, re-reading the last reply, opening a fresh
/// conversation) is the caller's [LIVE] job, exactly as with [`TurnDirective`].
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum TurnAction {
    /// Dictation: submit this text as a user turn. The floor stays with the user
    /// until the caller starts the reply's read-aloud via
    /// [`begin_reply`](TurnController::begin_reply).
    Submit(String),
    /// "stop"/"cancel"/"exit"/"quit": leave conversation mode. The capture mic has
    /// already been closed (cold, wake-gated), so
    /// [`user_mic_live`](TurnController::user_mic_live) is now `false`; the caller
    /// also halts any read-aloud in flight.
    Stop,
    /// "send it": submit whatever is already in the composer draft. No controller
    /// state change (the draft lives in the app).
    SendDraft,
    /// "scratch that": discard the composer draft. No controller state change.
    Scratch,
    /// "repeat"/"say that again": re-read the last reply aloud. No controller state
    /// change.
    Repeat,
    /// "new conversation": the controller has been [`reset`](TurnController::reset)
    /// to a cold, wake-gated user floor with no leftover reply state; the caller
    /// opens a fresh conversation.
    NewConversation,
    /// "approve"/"allow it": answer a pending Work approval gate with *allow*. No
    /// controller floor change — whether a `Work` approval card is actually open is
    /// app state the controller cannot see, so (exactly like [`SendDraft`](Self::SendDraft)
    /// deferring the empty-draft check) the [LIVE] caller consults its
    /// `work.panel.approval` and calls `resolve_work_approval(ApprovalDecision::Approve)`
    /// only when a card is up — that call already no-ops when none is, so an
    /// "approve" with nothing pending is harmless.
    Approve,
    /// "deny"/"reject": answer a pending Work approval gate with *deny*. The Deny
    /// counterpart of [`Approve`](Self::Approve); same no-controller-state, [LIVE]-gated
    /// wiring (`resolve_work_approval(ApprovalDecision::Deny)`).
    Deny,
    /// Nothing usable was said (silence, or annotations that strip to nothing);
    /// keep listening. No state change.
    KeepListening,
}

/// The single authoritative resting-state phase of the hands-free turn loop, for
/// a caller driving voice UI (a status indicator, a mic glyph). It composes the
/// two floor booleans [`assistant_speaking`](TurnController::assistant_speaking)
/// and [`user_mic_live`](TurnController::user_mic_live) into the three states that
/// can actually occur, so the impossible fourth combination — a live capture mic
/// *while* Awili is speaking, which the mutually-exclusive floors forbid — cannot
/// be represented at all. Unlike the per-feed [`TurnEvent`], which is the
/// momentary audio phase of one chunk, `TurnPhase` is the loop's resting state
/// *between* feeds: the one value a UI polls to render "listening / speaking /
/// asleep".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TurnPhase {
    /// User's floor, mic cold: wake-gated, waiting for the wake word (freshly
    /// built, after a reply drained or was barged, after a [`Stop`](TurnAction::Stop)
    /// or [`NewConversation`](TurnAction::NewConversation), or once the session's
    /// idle timeout closed capture). Awili is neither listening nor speaking.
    Dormant,
    /// User's floor, mic live: a wake-gated capture session is open and audio is
    /// being segmented into an utterance.
    Listening,
    /// Awili's floor: a read-aloud reply is playing, watched only for a barge-in.
    Speaking,
}

/// The pure hands-free *turn* loop: it owns the floor between the user and Awili,
/// composing the two already-proven kernels that each only cover half a turn
/// (mem 3049, voice priority #1). [`HandsFreeSession`] captures a user utterance
/// once the wake word arms it but knows nothing about playback; [`BargeInDetector`]
/// decides when playback-time speech is a deliberate interruption but knows
/// nothing about wake-gated listening. Neither prevents the failure mode that
/// only appears once they run in the *same* session: while Awili is speaking, its
/// own read-aloud bleeding into the still-open mic would either re-fire the wake
/// gate or be captured as a user utterance. [`TurnController`] is the missing
/// composition — exactly the split [`HandsFreeSession`] itself drew over
/// [`WakeGate`](awili_voice::WakeGate) + [`UtteranceSegmenter`](awili_voice::UtteranceSegmenter),
/// one level up.
///
/// The rule is the floor: audio reaches the wake gate *only* on the user's floor,
/// and the barge-in detector *only* on Awili's. Transitions between the two are
/// driven by the caller — [`begin_reply`](Self::begin_reply) when the reply's
/// TTS starts, [`reply_finished`](Self::reply_finished) when it drains — because
/// those events are async (the gateway turn, the audio queue) and live outside
/// this kernel, exactly as [`HandsFreeSession`] takes the spotter's confidence as
/// an input rather than computing it.
///
/// Still pure and deterministic against the shared sample clock: the barge VAD is
/// re-minted fresh each time Awili takes the floor (so one reply's trailing state
/// never leaks into the next), and after a reply the user's session is cold —
/// the next turn is wake-gated again. "Continue the conversation without another
/// wake word" is the deliberately-excluded ears-tuned follow-up, the same
/// boundary [`BargeInDetector`] and [`WakeGate`](awili_voice::WakeGate) draw.
#[derive(Debug)]
#[allow(dead_code)]
pub struct TurnController {
    floor: Floor,
    /// User-floor capture: wake gate + per-session utterance segmenter.
    session: HandsFreeSession,
    /// Assistant-floor interruption kernel.
    barge: BargeInDetector,
    /// Barge VAD tuning, stored (not a live VAD) so a fresh, stateless VAD is
    /// minted each time Awili takes the floor — same fresh-per-session discipline
    /// [`HandsFreeSession`] uses for its segmenter.
    barge_vad_threshold: f32,
    barge_vad_silence_windows: usize,
    /// The current reply's barge VAD; only fed on the assistant floor.
    barge_vad: Vad,
    /// How the last *ended* reply left the assistant floor — `Interrupted` on a
    /// barge-in, `Drained` on a natural finish, `None` before any reply or while
    /// one is in flight. The pure half of barge-in-then-resume (see
    /// [`ReplyOutcome`]).
    last_reply_outcome: Option<ReplyOutcome>,
}

#[allow(dead_code)]
impl TurnController {
    /// Build a controller starting on the user's floor (wake-gated). Only the
    /// barge VAD's `threshold` and `silence_windows` are read; any accumulated
    /// state on the passed VAD is ignored, since each reply gets its own fresh
    /// VAD.
    pub fn new(session: HandsFreeSession, barge: BargeInDetector, barge_vad: &Vad) -> Self {
        Self {
            floor: Floor::User,
            session,
            barge,
            barge_vad_threshold: barge_vad.threshold,
            barge_vad_silence_windows: barge_vad.silence_windows,
            barge_vad: Vad::new(barge_vad.threshold, barge_vad.silence_windows),
            last_reply_outcome: None,
        }
    }

    /// Feed one audio chunk together with the keyword-spotter's `confidence` for
    /// it. On the **user's floor** the chunk drives the wake-gated
    /// [`HandsFreeSession`] and `confidence` can open a turn. On **Awili's floor**
    /// `confidence` is ignored entirely — the chunk drives only the barge VAD, so
    /// the read-aloud can never wake or feed itself — and sustained speech yields
    /// a single [`TurnEvent::BargeIn`] that hands the floor back.
    pub fn feed(&mut self, confidence: f32, samples: &[f32]) -> TurnEvent {
        match self.floor {
            Floor::User => match self.session.feed(confidence, samples) {
                HandsFreeEvent::Idle => TurnEvent::Idle,
                HandsFreeEvent::Activated => TurnEvent::Activated,
                HandsFreeEvent::Listening => TurnEvent::Listening,
                HandsFreeEvent::Utterance(u) => TurnEvent::Utterance(u),
                // The session's idle timeout closed the mic; still the user's
                // floor, just cold until the next wake word.
                HandsFreeEvent::Deactivated => TurnEvent::Idle,
            },
            Floor::Assistant => {
                let ev = self.barge_vad.feed(samples);
                if self.barge.feed(true, ev) {
                    // Interrupt: the floor returns to the user (already disarmed
                    // by begin_reply, so cold/wake-gated). The caller stops TTS.
                    self.floor = Floor::User;
                    // Record that this reply was cut mid-playback so a later
                    // resume/repeat knows it has a pick-up point (ears-tuned
                    // wiring reads this; the state itself is pure).
                    self.last_reply_outcome = Some(ReplyOutcome::Interrupted);
                    TurnEvent::BargeIn
                } else {
                    TurnEvent::Speaking
                }
            }
        }
    }

    /// Awili takes the floor: the reply's read-aloud is starting. Ends the user's
    /// capture session (so it re-arms clean for the next turn), mints a fresh
    /// barge VAD, and re-arms the barge detector for this reply. Idempotent if
    /// already on the assistant floor (re-mints — treat it as "this is a new
    /// reply").
    pub fn begin_reply(&mut self) {
        self.session.end_session();
        self.barge_vad = Vad::new(self.barge_vad_threshold, self.barge_vad_silence_windows);
        // A `speaking == false` feed re-arms the detector (clears any latched
        // fire/run from a prior reply that ended in a barge-in).
        self.barge.feed(false, VadEvent::Silence);
        // A reply is now in flight: the prior reply's outcome is consumed, so no
        // stale interruption can be mistaken for this reply's fate.
        self.last_reply_outcome = None;
        self.floor = Floor::Assistant;
    }

    /// The read-aloud drained on its own (no barge-in): the floor returns to the
    /// user, wake-gated for the next turn. No-op if the user already holds the
    /// floor (e.g. a barge-in already handed it back).
    pub fn reply_finished(&mut self) {
        if self.floor == Floor::Assistant {
            self.floor = Floor::User;
            // Natural finish: nothing to resume. Guarded by the floor check so a
            // spurious call on the user floor cannot fabricate a Drained outcome
            // (e.g. after a barge-in already handed the floor back).
            self.last_reply_outcome = Some(ReplyOutcome::Drained);
        }
    }

    /// Resolve a *captured* user utterance — the text of a
    /// [`TurnEvent::Utterance`] after Whisper has transcribed it — into the app
    /// action to take, applying the controller-floor change the decision implies.
    /// This lifts the pure [`resolve_turn`] decision into the turn loop: the
    /// classifier still gets precedence over dictation (hands-free control never
    /// round-trips the model), but now a control command also *moves the floor*
    /// where that is code-verifiable.
    ///
    /// A `Utterance` is only ever produced on the user's floor (the assistant
    /// floor yields [`TurnEvent::Speaking`]/[`TurnEvent::BargeIn`], never a
    /// transcript), so `resolve` runs on the user floor. Its state effects:
    /// * **Stop** (also the legacy "exit"/"quit" stop words) closes the capture
    ///   session — the mic goes cold and wake-gated, so
    ///   [`user_mic_live`](Self::user_mic_live) becomes `false` — modelling "leave
    ///   conversation mode". The caller also halts any read-aloud.
    /// * **NewConversation** [`reset`](Self::reset)s the controller to a pristine,
    ///   cold user floor.
    /// * **SendDraft / Scratch / Repeat** touch only app state (the composer
    ///   draft, the read-aloud), so the controller is left untouched.
    /// * **Submit / KeepListening** leave the floor with the user, armed for the
    ///   reply or the next utterance.
    pub fn resolve(&mut self, transcript: &str) -> TurnAction {
        match resolve_turn(transcript) {
            TurnDirective::Submit(text) => TurnAction::Submit(text),
            TurnDirective::Ignore => TurnAction::KeepListening,
            TurnDirective::Command(cmd) => match cmd {
                VoiceCommand::Stop => {
                    // Leaving conversation mode: close the mic so the next turn is
                    // wake-gated again (the read-aloud stop is the caller's).
                    self.session.end_session();
                    TurnAction::Stop
                }
                VoiceCommand::Send => TurnAction::SendDraft,
                VoiceCommand::Scratch => TurnAction::Scratch,
                VoiceCommand::Repeat => TurnAction::Repeat,
                VoiceCommand::NewConversation => {
                    self.reset();
                    TurnAction::NewConversation
                }
                // Approval-gate answers carry no controller floor change — the
                // pending Work card is app state the controller cannot see, so the
                // decision passes straight through and the [LIVE] caller answers
                // (or no-ops) against its own `work.panel.approval`.
                VoiceCommand::Approve => TurnAction::Approve,
                VoiceCommand::Deny => TurnAction::Deny,
            },
        }
    }

    /// Thread a captured utterance from audio all the way to an action in one
    /// step: transcribe the samples of a [`TurnEvent::Utterance`] with
    /// `transcribe`, then [`resolve`](Self::resolve) the resulting text into a
    /// [`TurnAction`], applying whatever floor change the decision implies. In
    /// the real loop `transcribe` is a Whisper pass — [`ready_model`] /
    /// [`wait_for_model`] hand back the model — kept behind the closure so this
    /// seam stays pure and CI-testable with no whisper binary.
    ///
    /// This is the missing join between [`feed`](Self::feed), which returns the
    /// raw [`TurnEvent::Utterance`] samples once the user's turn endpoints, and
    /// [`resolve`](Self::resolve), which needs the *transcribed text*. The real
    /// driver is the loop
    /// `match self.feed(conf, chunk) { TurnEvent::Utterance(u) => self.resolve_utterance(&u, whisper), .. }`.
    ///
    /// A transcription that yields nothing usable — `None` when the model is not
    /// yet ready or the pass fails, or text that strips to empty — is treated as
    /// silence: [`TurnAction::KeepListening`], floor unchanged, so a dropped or
    /// empty Whisper pass never loses the mic or the user's turn (the SF3 lesson
    /// that an unready model must not silently drop the utterance, carried one
    /// level up from [`wait_for_model`]).
    pub fn resolve_utterance(
        &mut self,
        samples: &[f32],
        transcribe: impl FnOnce(&[f32]) -> Option<String>,
    ) -> TurnAction {
        match transcribe(samples) {
            // `resolve` already folds empty/annotation-only text to
            // KeepListening, so `Some("")` and `Some("[BLANK_AUDIO]")` are safe.
            Some(text) => self.resolve(&text),
            None => TurnAction::KeepListening,
        }
    }

    /// One-call hands-free loop step: feed a chunk, and — only when that chunk
    /// endpoints a complete user utterance — transcribe and resolve it in the
    /// same call, returning both the per-feed [`TurnEvent`] and the resulting
    /// [`TurnAction`]. This folds the two-step
    /// `match self.feed(conf, chunk) { TurnEvent::Utterance(u) => self.resolve_utterance(&u, whisper), .. }`
    /// the loop would otherwise hand-write into a single entry point, so the
    /// caller drives the whole turn with one call per audio chunk:
    /// `let (event, action) = ctl.drive(conf, chunk, whisper);`.
    ///
    /// The returned `Option<TurnAction>` is `Some` on **exactly** the feeds that
    /// produced a [`TurnEvent::Utterance`] — every other event
    /// ([`Idle`](TurnEvent::Idle), [`Activated`](TurnEvent::Activated),
    /// [`Listening`](TurnEvent::Listening), [`Speaking`](TurnEvent::Speaking),
    /// [`BargeIn`](TurnEvent::BargeIn)) pairs with `None`, and `transcribe` is
    /// **not called** on those feeds (no Whisper pass is spent on a chunk that
    /// carries no finished utterance). The `TurnEvent` is still surfaced verbatim
    /// on the utterance feed too, so a caller that also drives UI off the audio
    /// phase (a "listening…" indicator, a barge-in stop) sees every event whether
    /// or not an action came with it.
    ///
    /// Floor effects are exactly those of [`resolve_utterance`](Self::resolve_utterance)
    /// / [`resolve`](Self::resolve) — a control command moves the floor, dictation
    /// keeps it, an unready/empty transcription folds to
    /// [`KeepListening`](TurnAction::KeepListening) and never drops the turn.
    pub fn drive(
        &mut self,
        confidence: f32,
        samples: &[f32],
        transcribe: impl FnOnce(&[f32]) -> Option<String>,
    ) -> (TurnEvent, Option<TurnAction>) {
        match self.feed(confidence, samples) {
            TurnEvent::Utterance(u) => {
                let action = self.resolve_utterance(&u, transcribe);
                (TurnEvent::Utterance(u), Some(action))
            }
            event => (event, None),
        }
    }

    /// One-call loop step that *also* threads the resting-phase edge: run
    /// [`drive`](Self::drive), then feed the controller's new
    /// [`phase`](Self::phase) through the caller's [`PhaseTracker`], returning the
    /// per-feed [`TurnEvent`], the resolved [`TurnAction`] (if this chunk
    /// endpointed an utterance), and the [`PhaseChange`] the step produced (if the
    /// resting phase actually moved). This folds the
    /// `let (event, action) = ctl.drive(..); let change = tracker.observe(ctl.phase());`
    /// a UI-driving loop would otherwise hand-write into one call:
    /// `let (event, action, change) = ctl.drive_tracked(&mut tracker, conf, chunk, whisper);`.
    ///
    /// Crucially the phase is sampled *after* `drive` has resolved the turn, so any
    /// floor effect of the resolved action is already reflected in the edge: a
    /// transcribed [`Stop`](TurnAction::Stop)/[`NewConversation`](TurnAction::NewConversation)
    /// that closes the mic surfaces its `-> Dormant` edge in the *same* call that
    /// returns the action, and dictation that keeps the mic live yields `None` (the
    /// phase held steady). The tracker stays a caller-owned, seedable kernel — this
    /// only borrows it — so the same decoupling [`PhaseTracker`] draws is preserved.
    pub fn drive_tracked(
        &mut self,
        tracker: &mut PhaseTracker,
        confidence: f32,
        samples: &[f32],
        transcribe: impl FnOnce(&[f32]) -> Option<String>,
    ) -> (TurnEvent, Option<TurnAction>, Option<PhaseChange>) {
        let (event, action) = self.drive(confidence, samples, transcribe);
        let change = tracker.observe(self.phase());
        (event, action, change)
    }

    /// Reset to a cold, wake-gated user floor — as if freshly built. Ends any open
    /// capture session and re-mints the barge VAD/detector so no prior-reply run
    /// leaks into the next exchange (same fresh-per-reply discipline as
    /// [`begin_reply`](Self::begin_reply)). Used by [`resolve`](Self::resolve) on a
    /// [`VoiceCommand::NewConversation`], and directly by the caller when the user
    /// opens a new conversation by hand.
    pub fn reset(&mut self) {
        self.session.end_session();
        self.barge_vad = Vad::new(self.barge_vad_threshold, self.barge_vad_silence_windows);
        self.barge.feed(false, VadEvent::Silence);
        self.floor = Floor::User;
        // A pristine conversation has no reply behind it to resume.
        self.last_reply_outcome = None;
    }

    /// How the last *ended* reply left Awili's floor, or `None` if no reply has
    /// ended into the current user floor (before any reply, or while one is in
    /// flight). A "repeat"/"resume" wiring reads this to tell a mid-playback
    /// interruption ([`ReplyOutcome::Interrupted`], a real pick-up point) from a
    /// reply that already drained ([`ReplyOutcome::Drained`], re-read from the
    /// top). The pure, ears-free half of barge-in-then-resume; the audio resume
    /// itself stays with the [`TurnAction::Repeat`] wiring.
    pub fn last_reply_outcome(&self) -> Option<ReplyOutcome> {
        self.last_reply_outcome
    }

    /// Whether Awili currently holds the floor (a reply is playing). Authoritative
    /// resting-state query, distinct from the per-feed [`TurnEvent`]s.
    pub fn assistant_speaking(&self) -> bool {
        self.floor == Floor::Assistant
    }

    /// Whether the user's capture mic is live right now (a wake-gated session is
    /// open). Always `false` on Awili's floor.
    pub fn user_mic_live(&self) -> bool {
        self.floor == Floor::User && self.session.is_active()
    }

    /// The loop's single authoritative resting-state [`TurnPhase`]: `Speaking` on
    /// Awili's floor, else `Listening` while the user's capture mic is live, else
    /// `Dormant`. This is the composition of
    /// [`assistant_speaking`](Self::assistant_speaking) and
    /// [`user_mic_live`](Self::user_mic_live) a UI caller wants as one source of
    /// truth. Because the two floors are mutually exclusive, `Speaking` is checked
    /// first and always wins over the (necessarily cold) mic, so the impossible
    /// "live mic while Awili speaks" state can never surface through this query.
    pub fn phase(&self) -> TurnPhase {
        if self.assistant_speaking() {
            TurnPhase::Speaking
        } else if self.user_mic_live() {
            TurnPhase::Listening
        } else {
            TurnPhase::Dormant
        }
    }
}

/// A single edge in the resting-[`TurnPhase`] stream: the loop moved `from` one
/// phase `to` another. This is what a UI driver reacts to — a status glyph
/// animates on the *change* ("just woke", "Awili started speaking", "went to
/// sleep"), not on every polled frame that reports the same phase. The three
/// convenience predicates name the arrivals a caller usually branches on;
/// `from` is kept for the callers (a chime, a log line) that care where the
/// loop came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub struct PhaseChange {
    /// The phase the loop was resting in before this edge.
    pub from: TurnPhase,
    /// The phase it has just entered.
    pub to: TurnPhase,
}

#[allow(dead_code)]
impl PhaseChange {
    /// The mic just opened for the user (arrived at [`Listening`](TurnPhase::Listening)):
    /// a wake word armed a fresh capture session.
    pub fn now_listening(&self) -> bool {
        self.to == TurnPhase::Listening
    }

    /// Awili just took the floor to read a reply (arrived at
    /// [`Speaking`](TurnPhase::Speaking)).
    pub fn now_speaking(&self) -> bool {
        self.to == TurnPhase::Speaking
    }

    /// The loop just went idle (arrived at [`Dormant`](TurnPhase::Dormant)): a reply
    /// drained or was barged, a [`Stop`](TurnAction::Stop)/[`NewConversation`](TurnAction::NewConversation)
    /// closed the mic, or the session's idle timeout did.
    pub fn now_dormant(&self) -> bool {
        self.to == TurnPhase::Dormant
    }
}

/// Turns the [`phase`](TurnController::phase) stream into edges. A UI driver polls
/// `phase()` every loop step but only wants to *act* when it changes; without this
/// it must hold the previous phase and diff by hand at every call site. `observe`
/// owns that one bit of state: feed it the current phase each step and it returns
/// `Some(`[`PhaseChange`]`)` on exactly the steps the phase actually moved, `None`
/// while it holds steady.
///
/// It is deliberately decoupled from [`TurnController`] — it observes the phase
/// *values*, not the controller — so it stays a pure, independently testable
/// kernel (the same split the rest of this file draws) and a caller can seed its
/// starting phase to whatever the UI first rendered. The usual driver line is
/// `if let Some(change) = tracker.observe(ctl.phase()) { /* animate */ }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub struct PhaseTracker {
    last: TurnPhase,
}

#[allow(dead_code)]
impl PhaseTracker {
    /// Start observing from `initial` — the phase the UI is already showing, so the
    /// first `observe` of that same phase reports no change. A fresh controller
    /// rests in [`Dormant`](TurnPhase::Dormant); [`PhaseTracker::new`] with that
    /// value matches a UI that opens asleep.
    pub fn new(initial: TurnPhase) -> Self {
        Self { last: initial }
    }

    /// The last phase observed — what the UI is currently rendering.
    pub fn phase(&self) -> TurnPhase {
        self.last
    }

    /// Feed the current [`TurnPhase`]. Returns `Some(`[`PhaseChange`]`)` — from the
    /// previously observed phase to `phase` — when `phase` differs from the last
    /// one seen, and records it; returns `None` (recording nothing new) when the
    /// phase has not moved since the previous call, so repeated polling of a steady
    /// phase yields exactly one edge per real transition.
    pub fn observe(&mut self, phase: TurnPhase) -> Option<PhaseChange> {
        if phase == self.last {
            None
        } else {
            let change = PhaseChange {
                from: self.last,
                to: phase,
            };
            self.last = phase;
            Some(change)
        }
    }
}

/// Pull a ready model out of the slot (cloning the cheap `Arc`) without holding
/// the lock across the slow transcription that follows.
pub fn ready_model(slot: &ModelSlot) -> Option<Arc<Whisper>> {
    match &*slot.lock().unwrap() {
        ModelState::Ready(w) => Some(w.clone()),
        _ => None,
    }
}

/// Block until the model is `Ready` (returning it) or terminally not coming
/// (`Failed`, or back to `Idle` — load never started). Polls while `Loading`.
///
/// Used by the final-transcription worker (review SF3): on first use, the user
/// can release dictation while the multi-hundred-MB download is still running.
/// The recorded utterance is *retained* and transcribed the moment loading
/// finishes, instead of being silently discarded because the slot wasn't `Ready`
/// at the instant of release. Session scoping (blocker 1) still applies at
/// apply-time, so a transcript that waited out a long download is dropped if the
/// user has since started a new utterance or submitted.
///
/// RC2-APP (F0275): the wait is bounded ([`MODEL_LOAD_DEADLINE`]), and an
/// `Idle` slot is given [`MODEL_IDLE_GRACE`] for its load to start (the
/// load starts on its own thread, so a wait can see the slot before it
/// does) instead of dropping the utterance at once.
pub fn wait_for_model(slot: &ModelSlot) -> Option<Arc<Whisper>> {
    wait_for_model_within(slot, MODEL_IDLE_GRACE, MODEL_LOAD_DEADLINE)
}

/// How long a wait gives an `Idle` model slot for its load to start.
pub const MODEL_IDLE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
/// The longest a transcription waits for the voice model (a first-use
/// download of several hundred MB included).
pub const MODEL_LOAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// [`wait_for_model`] with explicit bounds: `idle_grace` for a load to
/// start, `deadline` overall. `None` when the load failed, never started,
/// or did not finish in time.
pub fn wait_for_model_within(
    slot: &ModelSlot,
    idle_grace: std::time::Duration,
    deadline: std::time::Duration,
) -> Option<Arc<Whisper>> {
    let started = std::time::Instant::now();
    loop {
        let waited = started.elapsed();
        match &*lock_slot(slot) {
            ModelState::Ready(w) => return Some(w.clone()),
            ModelState::Failed(_) => return None,
            ModelState::Idle if waited >= idle_grace => return None,
            ModelState::Idle | ModelState::Loading(_) => {}
        }
        if waited >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// RC2-APP (F0274): the model slot, even after a panic elsewhere while it
/// was held.
fn lock_slot(slot: &ModelSlot) -> std::sync::MutexGuard<'_, ModelState> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// RC2-APP (F0274): while a load runs, the slot says `Loading`; if the load
/// ends without saying how (a panic in the download or in the model load),
/// this guard says it failed — the slot never stays `Loading`, so the mic
/// can retry and a waiting transcription ends — and clears the lock's
/// poison.
pub(crate) struct LoadingGuard<'a>(pub(crate) &'a ModelSlot);

/// The message a load that ended without an outcome leaves in the slot.
pub const LOAD_STOPPED: &str = "the voice model load stopped unexpectedly";

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        {
            let mut state = lock_slot(self.0);
            if matches!(&*state, ModelState::Loading(_)) {
                *state = ModelState::Failed(LOAD_STOPPED.to_string());
            }
        }
        self.0.clear_poison();
    }
}

/// Blocking model load for a worker thread: download (with progress reported
/// through `status`) then load, writing the outcome into `slot`. Idempotent — a
/// second caller that finds the slot already loading/ready returns at once.
pub fn load_model_blocking(spec: &ModelSpec, slot: &ModelSlot, mut status: impl FnMut(String)) {
    {
        let mut s = slot.lock().unwrap();
        if !s.can_load() {
            return; // already loading or ready
        }
        *s = ModelState::Loading(0.0);
    }
    // RC2-APP (F0274): never left `Loading` by a panic below.
    let _loading = LoadingGuard(slot);
    status("Preparing voice model…".to_string());
    let path = match ensure_model(spec, |p: Progress| {
        // Both feedback channels (slice-5 §4): the slot fraction drives the
        // mic/voice button label, the status string the composer-row line.
        note_download_progress(slot, p);
        if let Some(frac) = p.fraction() {
            status(format!(
                "Downloading voice model… {}%",
                (frac * 100.0) as u32
            ));
        } else {
            status("Downloading voice model…".to_string());
        }
    }) {
        Ok(p) => p,
        Err(e) => {
            *slot.lock().unwrap() = ModelState::Failed(e.to_string());
            status(format!("Voice model unavailable: {e}"));
            return;
        }
    };
    status("Loading voice model…".to_string());
    match Whisper::load(&path.to_string_lossy()) {
        Ok(w) => {
            *slot.lock().unwrap() = ModelState::Ready(Arc::new(w));
            status_clear(&mut status);
        }
        Err(e) => {
            *slot.lock().unwrap() = ModelState::Failed(e.to_string());
            status(format!("Voice model failed to load: {e}"));
        }
    }
}

/// Whisper needs a minimum amount of audio (~1 s at 16 kHz) before a transcript is
/// meaningful; below this a partial would just be noise.
pub const MIN_SAMPLES_FOR_PARTIAL: usize = 16_000;

/// Sentinel status meaning "clear the line". `load_model_blocking` emits it on
/// success so the UI drops the loading message.
pub const STATUS_CLEAR: &str = "\u{0}";

fn status_clear(status: &mut impl FnMut(String)) {
    status(STATUS_CLEAR.to_string());
}

// ---- RC2-R4 (F0956): barge-in while Awili reads a reply ---------------------

/// Consecutive 20 ms speech windows that count as the owner talking over the
/// read-aloud (200 ms): a cough or a one-word "mhm" does not.
pub const BARGE_SUSTAIN_WINDOWS: u32 = 10;
/// Audio kept from before the barge-in fired (300 ms), so the start of "stop"
/// is in what Whisper hears.
pub const BARGE_PREROLL_SAMPLES: usize = 15 * awili_voice::vad::WINDOW_SAMPLES;
/// The longest stretch of talk-over transcribed at once (4 s at 16 kHz).
pub const BARGE_MAX_SAMPLES: usize = 64_000;

/// RC2-R4 (F0956): Awili's floor while it reads a reply in conversation mode.
/// A [`TurnController`] on the assistant floor runs the [`BargeInDetector`]
/// over the mic; when the owner talks over the reply long enough, the catcher
/// keeps that speech (with a little audio from just before) until a silence
/// endpoint or [`BARGE_MAX_SAMPLES`], hands it back for transcription, and
/// takes the floor again for the rest of the reply. What the speech MEANS is
/// decided from its words by [`playback_barge_action`] — the energy alone
/// cannot tell the owner from Awili's own voice in the speakers. Pure and
/// clock-free: the caller feeds samples, nothing here sleeps or reads time.
#[derive(Debug)]
pub struct BargeCatcher {
    ctl: TurnController,
    preroll: std::collections::VecDeque<f32>,
    vad_threshold: f32,
    vad_silence_windows: usize,
    /// The talk-over being kept, and its own endpoint VAD.
    capture: Option<(Vec<f32>, Vad)>,
}

impl BargeCatcher {
    /// A catcher with conversation mode's VAD tuning, already on Awili's floor.
    pub fn new(vad: &Vad) -> Self {
        // The user floor's wake gate is never fed a confidence above 0: the
        // catcher only ever listens while Awili holds the floor.
        let session = HandsFreeSession::new(awili_voice::WakeGate::new(1.0, 0, 1), vad);
        let mut ctl =
            TurnController::new(session, BargeInDetector::new(BARGE_SUSTAIN_WINDOWS), vad);
        ctl.begin_reply();
        Self {
            ctl,
            preroll: std::collections::VecDeque::with_capacity(BARGE_PREROLL_SAMPLES),
            vad_threshold: vad.threshold,
            vad_silence_windows: vad.silence_windows,
            capture: None,
        }
    }

    /// Feed one mic window (best one [`awili_voice::vad::WINDOW_SAMPLES`]
    /// long). Returns a finished talk-over to transcribe, else `None`.
    pub fn feed(&mut self, window: &[f32]) -> Option<Vec<f32>> {
        if let Some((acc, vad)) = &mut self.capture {
            acc.extend_from_slice(window);
            let ended = vad.feed(window) == VadEvent::SilenceEndpoint;
            if ended || acc.len() >= BARGE_MAX_SAMPLES {
                let (audio, _) = self.capture.take().expect("capturing");
                // Back on Awili's floor for the rest of the reply.
                self.ctl.begin_reply();
                self.preroll.clear();
                return Some(audio);
            }
            return None;
        }
        self.preroll.extend(window.iter().copied());
        while self.preroll.len() > BARGE_PREROLL_SAMPLES {
            self.preroll.pop_front();
        }
        if self.ctl.feed(0.0, window) == TurnEvent::BargeIn {
            let mut vad = Vad::new(self.vad_threshold, self.vad_silence_windows);
            let acc: Vec<f32> = self.preroll.drain(..).collect();
            vad.feed(&acc);
            self.capture = Some((acc, vad));
        }
        None
    }
}

/// RC2-R4M2: a microphone the conversation loop or the talk-over listener
/// holds: the real capture or, in tests, a stand-in whose talk-over the test
/// scripts (no device, no permission prompt, no speech model).
pub enum Mic {
    Real(MicCapture),
    #[cfg(test)]
    Test {
        samples: awili_voice::capture::SharedSamples,
        said: Arc<Mutex<Vec<String>>>,
    },
}

impl Mic {
    pub fn shared_buffer(&self) -> awili_voice::capture::SharedSamples {
        match self {
            Mic::Real(m) => m.shared_buffer(),
            #[cfg(test)]
            Mic::Test { samples, .. } => samples.clone(),
        }
    }

    pub fn stop(self) {
        match self {
            Mic::Real(m) => {
                let _ = m.stop();
            }
            #[cfg(test)]
            Mic::Test { .. } => {}
        }
    }

    #[cfg(test)]
    fn test_said(&self) -> Option<Arc<Mutex<Vec<String>>>> {
        match self {
            Mic::Real(_) => None,
            Mic::Test { said, .. } => Some(said.clone()),
        }
    }
}

/// RC2-R4M2: tests only: the stand-in mics. `said` is what the owner says
/// over a read-aloud (only a running talk-over listener hears it); `opened`
/// counts the mics opened.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct TestMics {
    pub said: Arc<Mutex<Vec<String>>>,
    pub opened: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl TestMics {
    pub fn open(&self) -> Mic {
        self.opened
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Mic::Test {
            samples: Default::default(),
            said: self.said.clone(),
        }
    }
}

/// RC2-R4 (F0956): the barge-in listener's mic and worker while a reply is read
/// aloud in conversation mode. Held on the UI thread (the mic stream is not
/// `Send`); [`BargeRun::stop`] ends both.
pub struct BargeRun {
    mic: Mic,
    stop: Arc<AtomicBool>,
}

impl BargeRun {
    pub fn stop(self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.mic.stop();
    }
}

/// How often the barge-in worker takes the mic's new samples.
const BARGE_POLL: std::time::Duration = std::time::Duration::from_millis(40);

/// RC2-R4 (F0956): start listening for talk-over on `mic` while a reply is
/// read aloud. A worker thread feeds the mic in 20 ms windows to a
/// [`BargeCatcher`]; each talk-over it keeps is transcribed (only when the
/// model is already loaded) and handed to `on_text`. Nothing is submitted
/// from here: the UI decides with [`playback_barge_action`].
pub fn start_barge_run(
    mic: Mic,
    vad: Vad,
    slot: ModelSlot,
    mut on_text: impl FnMut(String) + Send + 'static,
) -> BargeRun {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let buffer = mic.shared_buffer();
    #[cfg(test)]
    let said = mic.test_said();
    std::thread::spawn(move || {
        use std::sync::atomic::Ordering;
        let mut catcher = BargeCatcher::new(&vad);
        while !flag.load(Ordering::SeqCst) {
            std::thread::sleep(BARGE_POLL);
            // RC2-R4M2: a stand-in mic hears what its test scripts.
            #[cfg(test)]
            if let Some(said) = &said {
                let heard = std::mem::take(&mut *said.lock().unwrap_or_else(|p| p.into_inner()));
                for text in heard {
                    if !flag.load(Ordering::SeqCst) {
                        on_text(text);
                    }
                }
            }
            let fresh = std::mem::take(&mut *buffer.lock().unwrap_or_else(|p| p.into_inner()));
            for window in fresh.chunks(awili_voice::vad::WINDOW_SAMPLES) {
                let Some(audio) = catcher.feed(window) else {
                    continue;
                };
                let text = ready_model(&slot).and_then(|w| w.transcribe(&audio).ok());
                if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                    if !flag.load(Ordering::SeqCst) {
                        on_text(text);
                    }
                }
            }
        }
    });
    BargeRun { mic, stop }
}

/// What a talk-over during the read-aloud asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BargeAction {
    /// Awili's own voice coming back through the mic, or talk that is not a
    /// stop: the reply goes on.
    Ignore,
    /// The owner said stop: stop reading and stop the reply.
    StopReply,
}

/// Lower-case words of `text` (letters, digits and apostrophes kept).
fn barge_words(text: &str) -> Vec<String> {
    strip_voice_annotations(text)
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
        .filter(|w| !w.is_empty())
        .map(|w| w.replace('\u{2019}', "'"))
        .collect()
}

/// RC2-R4 (F0956): decide a talk-over from its transcript and the sentences
/// Awili has been reading aloud in this reply (`spoken`). Awili's own voice
/// comes back through the mic as runs of its own words, so every run of two
/// or more words that also runs in what it said is taken away, and a
/// one-word transcript that is one of its words is its echo too (even when
/// its sentence says "stop"). What is left is a stop when the voice commands
/// read it as one ("stop", "stop talking", "that's enough", "Awili, stop",
/// "cancel", "quit"…); anything else is ignored.
pub fn playback_barge_action(transcript: &str, spoken: &[String]) -> BargeAction {
    let said: Vec<String> = spoken.iter().flat_map(|s| barge_words(s)).collect();
    let heard = barge_words(transcript);
    if heard.is_empty() {
        return BargeAction::Ignore;
    }
    let runs_in_said = |run: &[String]| said.windows(run.len()).any(|w| w == run);
    let mut echo = vec![false; heard.len()];
    if heard.len() == 1 {
        echo[0] = said.contains(&heard[0]);
    }
    for i in 0..heard.len() {
        for j in (i + 2)..=heard.len() {
            if runs_in_said(&heard[i..j]) {
                echo[i..j].iter_mut().for_each(|e| *e = true);
            }
        }
    }
    let left: Vec<&str> = heard
        .iter()
        .zip(&echo)
        .filter(|(_, e)| !**e)
        .map(|(w, _)| w.as_str())
        .collect();
    if left.is_empty() {
        return BargeAction::Ignore;
    }
    match resolve_turn(&left.join(" ")) {
        TurnDirective::Command(VoiceCommand::Stop) => BargeAction::StopReply,
        _ => BargeAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_inherits_auto_speak_from_config() {
        let cfg = VoiceConfig {
            auto_speak: true,
            ..Default::default()
        };
        let v = Voice::new(cfg);
        assert!(v.auto_speak);
        assert!(!v.dictating);
        assert!(v.model.lock().unwrap().can_load());
    }

    #[test]
    fn ready_model_is_none_until_loaded() {
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Idle));
        assert!(ready_model(&slot).is_none());
        *slot.lock().unwrap() = ModelState::Failed("x".into());
        assert!(ready_model(&slot).is_none());
    }

    #[test]
    fn can_load_permits_retry_after_failure_but_not_a_load_in_flight() {
        // can_load is the guard load_model_blocking checks before starting a
        // (re)load and the predicate the mic-toggle retry hangs on. Only the
        // Idle => true arm is otherwise exercised (voice_inherits_auto_speak…),
        // leaving two load-bearing arms unpinned:
        //   * Failed => true — after a failed download the user must be able to
        //     retry by toggling the mic again (the message from failure() is
        //     re-surfaced in main.rs). Drop `| Failed(_)` and retry dies silently.
        //   * Loading => false — a second worker that finds a download already in
        //     flight returns at once (load_model_blocking idempotency) rather than
        //     clobbering it back to Loading(0.0). Add `| Loading(_)` and a stray
        //     click restarts the multi-hundred-MB download mid-stream.
        assert!(ModelState::Idle.can_load(), "never attempted: loadable");
        assert!(
            ModelState::Failed("network".into()).can_load(),
            "retry after a failed load"
        );
        assert!(
            !ModelState::Loading(0.4).can_load(),
            "a load already in flight is not restartable"
        );
    }

    #[test]
    fn failure_surfaces_the_prior_error_message_only_when_failed() {
        // failure() is what main.rs reads to re-surface a prior model-load error
        // as the user toggles the mic to retry ("retrying voice model (previous
        // error: {msg})"). It had no direct coverage, leaving two arms unpinned:
        //   * Failed(e) => Some(e) — must return the ACTUAL stored message, not a
        //     constant; a mutation to Some("") or to None silently swallows the
        //     download error on retry (the very failure the banner exists to fix).
        //   * _ => None — every non-failed state (Idle before a load, Loading
        //     mid-download, Ready) must report no error, or the retry banner would
        //     fire spuriously mid-load. Idle and Loading isolate this arm without a
        //     Whisper Arc (Ready is not constructible in-test).
        assert_eq!(
            ModelState::Failed("network down".into()).failure(),
            Some("network down"),
            "a failed load must re-surface its exact message"
        );
        assert_eq!(
            ModelState::Idle.failure(),
            None,
            "never attempted: no error"
        );
        assert_eq!(
            ModelState::Loading(0.5).failure(),
            None,
            "a load in flight is not a failure"
        );
    }

    // ---- blocker 1: session scoping --------------------------------------

    #[test]
    fn stale_session_results_are_dropped() {
        // The blocker scenario: session A is released while inference runs;
        // session B starts before A's results land. A's partial and final are
        // stamped with A's id and the gate (the exact check the UI apply
        // closures perform) must reject them, while B's go through.
        use awili_core::EditBuffer;
        use awili_voice::Dictation;

        let mut sessions = SessionCounter::default();
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();

        let a = sessions.begin();
        assert!(sessions.is_current(a));
        // A's first partial lands while A is still current.
        if sessions.is_current(a) {
            d.set_partial(&mut buf, "session a partial");
        }
        // A released; B begins before A's in-flight work returns.
        let b = sessions.begin();
        assert!(!sessions.is_current(a), "B's start must retire A");
        // B's ghost replaces the preview; a STALE partial from A must not.
        if sessions.is_current(b) {
            d.set_partial(&mut buf, "session b");
        }
        if sessions.is_current(a) {
            d.set_partial(&mut buf, "session a late partial");
        }
        assert_eq!(buf.text(), "session b");
        // A's final arrives after B started: dropped. B's final commits.
        if sessions.is_current(a) {
            d.commit(&mut buf, "session a final");
        }
        assert_eq!(buf.text(), "session b");
        if sessions.is_current(b) {
            d.commit(&mut buf, "session b final");
        }
        assert_eq!(buf.text(), "session b final ");
    }

    #[test]
    fn submit_invalidates_outstanding_final() {
        // The second half of the blocker: the user submits while the final
        // transcription is still running. take()/submit invalidates the session,
        // so the stale final must not insert into the cleared composer.
        use awili_core::EditBuffer;
        use awili_voice::Dictation;

        let mut sessions = SessionCounter::default();
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();

        let a = sessions.begin();
        d.set_partial(&mut buf, "draft words");
        // Submit: the draft (preview included) is taken; sessions invalidated.
        let sent = std::mem::take(&mut buf);
        let _ = sent;
        d = Dictation::new();
        sessions.invalidate();
        // The in-flight final for A returns afterwards — and is dropped.
        if sessions.is_current(a) {
            d.commit(&mut buf, "stale final");
        }
        assert_eq!(buf.text(), "");
        assert!(!d.is_active());
    }

    #[test]
    fn session_counter_basics() {
        let mut s = SessionCounter::default();
        let a = s.begin();
        assert_eq!(s.current(), a);
        assert!(s.is_current(a));
        let b = s.begin();
        assert!(s.is_current(b) && !s.is_current(a));
        s.invalidate();
        assert!(!s.is_current(b), "invalidate retires the live session too");
    }

    // ---- SF3: the final transcription waits for a loading model ----------

    #[test]
    fn wait_for_model_returns_none_on_failure_without_blocking() {
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Failed("boom".into())));
        assert!(wait_for_model(&slot).is_none());
        // Idle (load never started) must also return rather than spin forever.
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Idle));
        assert!(wait_for_model(&slot).is_none());
    }

    // ---- slice 5: model download progress (§4) ----------------------------

    #[test]
    fn download_progress_drives_loading_state_transitions() {
        // Mocked ensure_model progress stream: the slot must walk
        // Loading(0.0) → Loading(0.43) → Loading(1.0), readable at render time.
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Idle));
        assert_eq!(slot.lock().unwrap().progress(), None);

        *slot.lock().unwrap() = ModelState::Loading(0.0); // load_model_blocking's first act
        assert_eq!(slot.lock().unwrap().progress(), Some(0.0));

        note_download_progress(
            &slot,
            Progress {
                received: 43,
                total: Some(100),
            },
        );
        assert_eq!(slot.lock().unwrap().progress(), Some(0.43));

        note_download_progress(
            &slot,
            Progress {
                received: 100,
                total: Some(100),
            },
        );
        assert_eq!(slot.lock().unwrap().progress(), Some(1.0));
    }

    #[test]
    fn unknown_total_keeps_the_previous_fraction() {
        // A chunked response reports no Content-Length: the bar must not snap
        // back to 0% mid-download.
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Loading(0.5)));
        note_download_progress(
            &slot,
            Progress {
                received: 999,
                total: None,
            },
        );
        assert_eq!(slot.lock().unwrap().progress(), Some(0.5));
    }

    #[test]
    fn progress_ticks_do_not_resurrect_a_finished_load() {
        // A straggling progress event after Failed/Ready must not flip the
        // state back to Loading.
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Failed("net".into())));
        note_download_progress(
            &slot,
            Progress {
                received: 1,
                total: Some(2),
            },
        );
        assert!(matches!(&*slot.lock().unwrap(), ModelState::Failed(_)));
        assert_eq!(slot.lock().unwrap().progress(), None);
    }

    // ---- slice 5: conversation-mode turn actions (§3) ---------------------

    #[test]
    fn stop_words_exit_conversation_mode() {
        for t in [
            "stop", "Stop.", " STOP ", "Cancel", "cancel!", "Exit.", "quit", "Quit!",
        ] {
            assert_eq!(voice_turn_action(t), VoiceTurnAction::Exit, "{t:?}");
        }
    }

    #[test]
    fn normal_transcripts_submit_as_spoken() {
        assert_eq!(
            voice_turn_action(" What's the weather? "),
            VoiceTurnAction::Submit("What's the weather?".to_string())
        );
        // A stop word inside a sentence is NOT a stop command.
        assert_eq!(
            voice_turn_action("stop the build and rerun it"),
            VoiceTurnAction::Submit("stop the build and rerun it".to_string())
        );
    }

    #[test]
    fn whitespace_separated_fragments_are_not_collapsed_into_a_stop_word() {
        // Normalization keeps internal whitespace (the `|| c.is_whitespace()`
        // disjunct of the alphanumeric filter), so a mis-segmented utterance —
        // e.g. whisper rendering "exit" as "ex it" — stays two tokens and is
        // submitted as spoken. Dropping the whitespace disjunct would merge the
        // fragments into "exit"/"stop" and silently drop the user out of
        // conversation mode. Unlike the punctuation cases (which are edge-only
        // trimmed by the same match on `normalized`), only preserved *internal*
        // whitespace keeps these apart from a bare stop word.
        assert_eq!(
            voice_turn_action("ex it"),
            VoiceTurnAction::Submit("ex it".to_string())
        );
        assert_eq!(
            voice_turn_action("st op"),
            VoiceTurnAction::Submit("st op".to_string())
        );
    }

    /// RC2-UX (F0956 app half): the stop phrases the voice crate knows leave
    /// conversation mode through the live path (`on_voice_transcript` acts on
    /// `voice_turn_action`), not only the four legacy words; a sentence that
    /// merely contains one is still sent.
    #[test]
    fn rc2ux_every_stop_phrase_leaves_conversation_mode() {
        for t in [
            "Stop talking.",
            "shut up",
            "That's enough!",
            "Awili, stop please.",
            "Stop. Stop. Stop.",
            "be quiet",
            "hush",
        ] {
            assert_eq!(voice_turn_action(t), VoiceTurnAction::Exit, "{t:?}");
        }
        assert_eq!(
            voice_turn_action("please don't stop talking about gardens"),
            VoiceTurnAction::Submit("please don't stop talking about gardens".to_string())
        );
    }

    #[test]
    fn empty_or_punctuation_only_transcripts_are_ignored() {
        assert_eq!(voice_turn_action(""), VoiceTurnAction::Ignore);
        assert_eq!(voice_turn_action("   "), VoiceTurnAction::Ignore);
        assert_eq!(voice_turn_action("…"), VoiceTurnAction::Ignore);
    }

    #[test]
    fn whisper_silence_annotations_are_stripped_to_nothing() {
        // Pure non-speech annotations whisper emits on near-silence: an
        // all-annotation transcript must NOT submit — it collapses to Ignore.
        for t in [
            "[BLANK_AUDIO]",
            "[ Silence ]",
            "[silence]",
            "(silence)",
            "[MUSIC]",
            "( upbeat music )",
            " [BLANK_AUDIO] ",
            "[BLANK_AUDIO][BLANK_AUDIO]",
            "[BLANK_AUDIO] (typing)",
        ] {
            assert_eq!(strip_voice_annotations(t), "", "{t:?}");
            assert_eq!(voice_turn_action(t), VoiceTurnAction::Ignore, "{t:?}");
        }
    }

    #[test]
    fn annotations_are_stripped_but_surrounding_speech_survives() {
        // A real utterance with a stray annotation keeps the words.
        assert_eq!(strip_voice_annotations("hello [BLANK_AUDIO]"), "hello");
        assert_eq!(
            strip_voice_annotations("[ Silence ] what's the weather"),
            "what's the weather"
        );
        assert_eq!(
            voice_turn_action("[BLANK_AUDIO] what's the weather?"),
            VoiceTurnAction::Submit("what's the weather?".to_string())
        );
        // A stop word with a trailing annotation still exits.
        assert_eq!(
            voice_turn_action("stop [BLANK_AUDIO]"),
            VoiceTurnAction::Exit
        );
    }

    #[test]
    fn stripping_never_eats_brackets_a_speaker_would_not_utter() {
        // Plain speech with no delimiters is untouched (whitespace collapsed).
        assert_eq!(
            strip_voice_annotations("just plain words"),
            "just plain words"
        );
        assert_eq!(
            strip_voice_annotations("multiple    spaces  here"),
            "multiple spaces here"
        );
        // A nested/mixed span is removed wholesale up to its own closer.
        assert_eq!(strip_voice_annotations("[a (b) c] kept"), "kept");
        // An unterminated opener (whisper truncation) drops to end-of-string.
        assert_eq!(strip_voice_annotations("good [truncated"), "good");
    }

    // ---- resolve_turn: unified command-vs-dictation decision --------------

    #[test]
    fn resolve_turn_routes_control_phrases_to_commands() {
        // Each verbal-control family resolves to its command, not dictation.
        assert_eq!(
            resolve_turn("stop"),
            TurnDirective::Command(VoiceCommand::Stop)
        );
        assert_eq!(
            resolve_turn("send it"),
            TurnDirective::Command(VoiceCommand::Send)
        );
        assert_eq!(
            resolve_turn("scratch that"),
            TurnDirective::Command(VoiceCommand::Scratch)
        );
        assert_eq!(
            resolve_turn("repeat"),
            TurnDirective::Command(VoiceCommand::Repeat)
        );
        assert_eq!(
            resolve_turn("new conversation"),
            TurnDirective::Command(VoiceCommand::NewConversation)
        );
        // Address/politeness fillers and whisper punctuation don't break routing.
        assert_eq!(
            resolve_turn("Awili, send it please."),
            TurnDirective::Command(VoiceCommand::Send)
        );
    }

    #[test]
    fn resolve_turn_legacy_exit_words_fold_into_stop() {
        // "exit"/"quit" aren't in the command classifier's vocabulary, but the
        // slice-3 stop words mean stop-the-conversation — folded into Stop so
        // no behaviour is lost by giving the classifier precedence.
        for t in ["exit", "Exit.", "quit", "Quit!"] {
            assert_eq!(
                resolve_turn(t),
                TurnDirective::Command(VoiceCommand::Stop),
                "{t:?}"
            );
        }
    }

    #[test]
    fn resolve_turn_dictation_containing_a_command_word_is_submitted() {
        // The whole-utterance guard in the classifier means a real sentence that
        // merely contains a control verb is dictation — precedence can't eat it.
        for t in [
            "please don't stop helping me",
            "send the report to my accountant",
            "let's start over on the tax section",
            "stop the build and rerun it",
        ] {
            assert_eq!(
                resolve_turn(t),
                TurnDirective::Submit(t.to_string()),
                "{t:?}"
            );
        }
        // A plain question submits with trimmed text.
        assert_eq!(
            resolve_turn(" What's the weather? "),
            TurnDirective::Submit("What's the weather?".to_string())
        );
    }

    #[test]
    fn resolve_turn_ignores_silence_and_pure_annotations() {
        for t in ["", "   ", "…", "[BLANK_AUDIO]", "[ Silence ]", "(typing)"] {
            assert_eq!(resolve_turn(t), TurnDirective::Ignore, "{t:?}");
        }
    }

    #[test]
    fn resolve_turn_bare_address_word_falls_through_to_dictation() {
        // A pure wake-address with no command core ("awili", "hey awili") is not a
        // command (the classifier's filler-strip leaves an empty core → None),
        // so it flows to the dictation path exactly as voice_turn_action would.
        // Documents the boundary rather than silently ignoring it: the classifier
        // owns commands, not address-word suppression.
        assert_eq!(
            resolve_turn("awili"),
            TurnDirective::Submit("awili".to_string())
        );
        assert_eq!(
            resolve_turn("hey awili"),
            TurnDirective::Submit("hey awili".to_string())
        );
    }

    #[test]
    fn resolve_turn_strips_annotations_before_classifying() {
        // A control phrase carrying a stray whisper annotation still resolves to
        // its command (annotations are stripped before the classifier sees it).
        assert_eq!(
            resolve_turn("stop [BLANK_AUDIO]"),
            TurnDirective::Command(VoiceCommand::Stop)
        );
        // ...and surrounding real speech with an annotation still submits.
        assert_eq!(
            resolve_turn("[BLANK_AUDIO] what's the weather?"),
            TurnDirective::Submit("what's the weather?".to_string())
        );
    }

    #[test]
    fn endpoint_to_submit_chain_produces_the_submit_action() {
        // The conversation wiring end-to-end minus audio hardware and whisper:
        // VAD endpoint → segmenter yields the utterance → (transcription
        // mocked) → the turn action is Submit with the transcribed text. This
        // is exactly the decision path on_voice_transcript executes before
        // calling on_submit.
        use awili_voice::{vad::WINDOW_SAMPLES, UtteranceSegmenter, Vad};
        let mut seg = UtteranceSegmenter::new(Vad::new(0.01, 5));
        assert!(
            seg.feed(&vec![0.1; 4 * WINDOW_SAMPLES]).is_none(),
            "speaking"
        );
        let utterance = seg
            .feed(&vec![0.0; 5 * WINDOW_SAMPLES])
            .expect("silence endpoint fires");
        assert!(utterance.len() >= 9 * WINDOW_SAMPLES);
        // Whisper stands in: the utterance "transcribes" to a question.
        let transcript = "hello awili";
        assert_eq!(
            voice_turn_action(transcript),
            VoiceTurnAction::Submit("hello awili".to_string())
        );
    }

    // ---- codex review SF2: typed draft vs voice auto-submit ----------------

    #[test]
    fn typed_draft_blocks_the_voice_auto_submit() {
        // A half-typed thought must not be spliced into a spoken utterance and
        // auto-submitted.
        assert!(draft_blocks_voice_submit("half a typed thou"));
        assert!(draft_blocks_voice_submit("  x"));
        // The normal hands-free case: an empty (or whitespace-only) composer.
        assert!(!draft_blocks_voice_submit(""));
        assert!(!draft_blocks_voice_submit("   \n"));
    }

    #[test]
    fn blocked_submit_leaves_the_typed_draft_untouched() {
        // The buffer-level scenario SF2 guards against: with a typed draft and
        // no ghost, Dictation::commit splices at the caret — which is exactly
        // why the gate must fire BEFORE the commit, leaving the draft alone.
        use awili_core::EditBuffer;
        use awili_voice::Dictation;
        let mut buf = EditBuffer::new();
        buf.set_text("draft in progress");
        buf.set_caret(5);
        assert!(draft_blocks_voice_submit(buf.text()));
        // Gate fired → no commit, no take: the draft is exactly as typed.
        assert_eq!(buf.text(), "draft in progress");
        // Counterfactual (what the unguarded path did): the spliced mixture
        // that would have been auto-submitted.
        let mut d = Dictation::new();
        d.commit(&mut buf, "what's the weather");
        assert_eq!(buf.text(), "draftwhat's the weather  in progress");
    }

    // ---- voice priority #1: "send it" empty-draft guard -------------------

    #[test]
    fn send_it_on_an_empty_composer_keeps_listening_not_a_blank_turn() {
        // "send it" with nothing in the composer must not post a blank user
        // turn; it folds to KeepListening (floor unchanged, mic still live).
        assert_eq!(resolve_send_draft(""), TurnAction::KeepListening);
        assert_eq!(resolve_send_draft("   \n\t"), TurnAction::KeepListening);
    }

    #[test]
    fn send_it_with_a_drafted_thought_sends_it() {
        assert_eq!(
            resolve_send_draft("email the accountant"),
            TurnAction::SendDraft
        );
        // Non-whitespace content, however short, is a real thought.
        assert_eq!(resolve_send_draft("  x"), TurnAction::SendDraft);
    }

    #[test]
    fn send_and_dictation_guards_share_a_content_rule_but_invert_intent() {
        // The polarity footgun the two named resolvers exist to prevent: they
        // agree on WHAT counts as content (so neither strands a whitespace-only
        // draft) yet mean opposite things by it — a non-empty draft SENDS on
        // "send it" and BLOCKS a dictation auto-submit.
        for d in ["", "   ", "hello", "  x"] {
            let has_content = !d.trim().is_empty();
            assert_eq!(draft_blocks_voice_submit(d), has_content);
            assert_eq!(resolve_send_draft(d) == TurnAction::SendDraft, has_content);
        }
    }

    // ---- voice priority #1: "repeat" nothing-to-repeat guard --------------

    #[test]
    fn repeat_before_any_reply_keeps_listening_not_a_degenerate_reread() {
        // "repeat" in a fresh conversation (no reply has ended, so
        // last_reply_outcome is None) must not tell the wiring to re-read an
        // empty reply; it folds to KeepListening (floor unchanged, mic live).
        assert_eq!(resolve_repeat(None), TurnAction::KeepListening);
    }

    #[test]
    fn repeat_after_a_drained_reply_re_reads() {
        // A reply that finished on its own is re-readable from the top.
        assert_eq!(
            resolve_repeat(Some(ReplyOutcome::Drained)),
            TurnAction::Repeat
        );
    }

    #[test]
    fn repeat_after_a_barged_reply_re_reads() {
        // An interrupted reply is also repeatable — the ears-tuned resume has a
        // pick-up point; this guard only decides the re-read fires at all.
        assert_eq!(
            resolve_repeat(Some(ReplyOutcome::Interrupted)),
            TurnAction::Repeat
        );
    }

    #[test]
    fn repeat_fires_iff_there_is_a_reply_outcome_to_pick_up_from() {
        // The whole guard: any recorded outcome is repeatable, only None is not —
        // exactly the has-something-to-act-on rule resolve_send_draft applies to
        // the composer draft, keyed here on the controller's own history.
        for (outcome, repeatable) in [
            (None, false),
            (Some(ReplyOutcome::Drained), true),
            (Some(ReplyOutcome::Interrupted), true),
        ] {
            assert_eq!(resolve_repeat(outcome) == TurnAction::Repeat, repeatable);
        }
    }

    // ---- voice priority #1: "repeat" re-read mode (from-top vs resume) -----

    #[test]
    fn repeat_mode_of_a_drained_reply_is_from_the_top() {
        // A reply that drained on its own has no interruption to pick up from, so
        // the fired re-read starts over from the top.
        assert_eq!(
            repeat_mode(Some(ReplyOutcome::Drained)),
            Some(RepeatMode::FromTop)
        );
    }

    #[test]
    fn repeat_mode_of_a_barged_reply_is_a_resume() {
        // An interrupted reply calls for a resume — the ears-tuned pickup point is
        // deferred, but the branch itself (resume, not from-top) is pure and pinned.
        assert_eq!(
            repeat_mode(Some(ReplyOutcome::Interrupted)),
            Some(RepeatMode::Resume)
        );
    }

    #[test]
    fn repeat_mode_of_no_reply_is_none() {
        // No reply has ended: there is no mode because a repeat should not have
        // fired at all — resolve_repeat(None) already folds to KeepListening.
        assert_eq!(repeat_mode(None), None);
    }

    #[test]
    fn repeat_mode_is_some_in_exact_lockstep_with_resolve_repeat_firing() {
        // The refinement invariant: repeat_mode yields a mode for precisely the
        // outcomes resolve_repeat turns into a Repeat, and no mode for the one it
        // folds to KeepListening. The two never disagree on whether a re-read fires.
        for outcome in [
            None,
            Some(ReplyOutcome::Drained),
            Some(ReplyOutcome::Interrupted),
        ] {
            assert_eq!(
                repeat_mode(outcome).is_some(),
                resolve_repeat(outcome) == TurnAction::Repeat,
                "repeat_mode and resolve_repeat must agree that a re-read fires"
            );
        }
    }

    #[test]
    fn repeat_mode_reads_a_live_controllers_recorded_outcome() {
        // End to end off the controller's own history, the way the [LIVE] wiring
        // consults it: repeat_mode(ctl.last_reply_outcome()).
        let mut c = turn_controller();
        // Fresh conversation: nothing to repeat, no mode.
        assert_eq!(repeat_mode(c.last_reply_outcome()), None);
        // A reply that drains on its own -> re-read from the top.
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.0, 1)), TurnEvent::Speaking);
        c.reply_finished();
        assert_eq!(
            repeat_mode(c.last_reply_outcome()),
            Some(RepeatMode::FromTop)
        );
        // The next reply is barged mid-playback -> resume from the pickup point.
        c.begin_reply();
        c.feed(0.0, &turn_windows(0.1, 1));
        c.feed(0.0, &turn_windows(0.1, 1));
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::BargeIn);
        assert_eq!(
            repeat_mode(c.last_reply_outcome()),
            Some(RepeatMode::Resume)
        );
    }

    // ---- voice priority #1: barge-in decision kernel ----------------------

    fn speech_run(d: &mut BargeInDetector, speaking: bool, n: usize) -> bool {
        // Feed `n` consecutive in-speech windows; the first is a SpeechStart, the
        // rest SpeechContinue (what Vad::feed emits across a held utterance).
        // Returns whether the kernel fired at any point in the run.
        let mut fired = false;
        for i in 0..n {
            let ev = if i == 0 {
                VadEvent::SpeechStart
            } else {
                VadEvent::SpeechContinue
            };
            fired |= d.feed(speaking, ev);
        }
        fired
    }

    #[test]
    fn barge_in_fires_only_after_sustained_speech_during_playback() {
        let mut d = BargeInDetector::new(3);
        // The first two in-speech windows are not enough — could be an echo blip.
        assert!(!d.feed(true, VadEvent::SpeechStart));
        assert!(!d.feed(true, VadEvent::SpeechContinue));
        // The third consecutive window crosses the threshold: barge in.
        assert!(
            d.feed(true, VadEvent::SpeechContinue),
            "sustained speech interrupts"
        );
        assert!(d.has_fired());
    }

    #[test]
    fn no_barge_in_while_not_speaking() {
        // Speech while no read-aloud is playing is ordinary hands-free listening,
        // handled by the segmenter — never an interrupt, however long it runs.
        let mut d = BargeInDetector::new(2);
        assert!(!speech_run(&mut d, false, 50));
        assert!(!d.has_fired());
    }

    #[test]
    fn a_lone_speech_blip_does_not_barge_in() {
        // The echo/cough guard: a single in-speech window followed by silence
        // resets the run, so isolated blips never accumulate to an interrupt.
        let mut d = BargeInDetector::new(3);
        for _ in 0..20 {
            assert!(!d.feed(true, VadEvent::SpeechStart));
            assert!(!d.feed(true, VadEvent::Silence));
        }
        assert!(!d.has_fired());
    }

    #[test]
    fn silence_resets_the_run() {
        // Two in-speech windows, then a real gap, must not combine with one more
        // window to reach a 3-window threshold — the run has to be consecutive.
        let mut d = BargeInDetector::new(3);
        assert!(!d.feed(true, VadEvent::SpeechStart));
        assert!(!d.feed(true, VadEvent::SpeechContinue));
        assert!(!d.feed(true, VadEvent::SilenceEndpoint), "the gap resets");
        assert!(!d.feed(true, VadEvent::SpeechStart), "count restarts at 1");
        assert!(!d.feed(true, VadEvent::SpeechContinue), "now 2");
        assert!(
            d.feed(true, VadEvent::SpeechContinue),
            "3 consecutive: fires"
        );
    }

    #[test]
    fn barge_in_fires_once_then_latches() {
        // After firing, the kernel must not re-emit an interrupt every window
        // while the app's async TTS stop is still in flight.
        let mut d = BargeInDetector::new(2);
        assert!(!d.feed(true, VadEvent::SpeechStart));
        assert!(
            d.feed(true, VadEvent::SpeechContinue),
            "fires on the 2nd window"
        );
        // Still speaking (cancel not yet applied): no repeat fire.
        for _ in 0..10 {
            assert!(
                !d.feed(true, VadEvent::SpeechContinue),
                "latched: fires only once"
            );
        }
        assert!(d.has_fired());
    }

    #[test]
    fn ending_playback_rearms_for_the_next_reply() {
        let mut d = BargeInDetector::new(2);
        assert!(speech_run(&mut d, true, 2), "first reply: barge in");
        assert!(d.has_fired());
        // Playback ends (the cancel landed, or the reply finished on its own).
        assert!(!d.feed(false, VadEvent::Silence), "not speaking re-arms");
        assert!(!d.has_fired(), "re-armed for the next reply");
        // A new reply can be barged into again.
        assert!(speech_run(&mut d, true, 2), "second reply: barge in again");
    }

    #[test]
    fn sustain_windows_clamped_to_at_least_one() {
        // 0 would interrupt on the first noisy window; clamp guards that.
        let mut d = BargeInDetector::new(0);
        assert!(
            d.feed(true, VadEvent::SpeechStart),
            "one window with the clamp"
        );
    }

    /// RC2-APP (F0274): a load that panics leaves the slot `Failed` (so the
    /// mic can retry and a waiting transcription ends), not `Loading`, and
    /// the slot is not left poisoned.
    #[test]
    fn rc2app_a_panicking_load_does_not_stay_loading() {
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Loading(0.0)));
        let held = slot.clone();
        let _ = std::thread::spawn(move || {
            let _loading = LoadingGuard(&held);
            let _locked = held.lock().unwrap();
            panic!("a panic in the model load, with the slot held");
        })
        .join();
        assert!(!slot.is_poisoned(), "the poison is cleared");
        let state = slot.lock().unwrap();
        assert_eq!(state.failure(), Some(LOAD_STOPPED));
        assert!(state.can_load(), "the mic can try again");
    }

    /// RC2-APP (F0275): a wait that starts before the load does (the slot
    /// still `Idle`) waits for it instead of dropping the utterance.
    #[test]
    fn rc2app_the_wait_outlasts_an_early_idle() {
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Idle));
        let loader_slot = slot.clone();
        let loader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            *loader_slot.lock().unwrap() = ModelState::Loading(0.0);
            std::thread::sleep(std::time::Duration::from_millis(300));
            *loader_slot.lock().unwrap() = ModelState::Failed("done loading".into());
        });
        let t0 = std::time::Instant::now();
        assert!(wait_for_model(&slot).is_none());
        loader.join().unwrap();
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(500),
            "the wait saw the load through: {:?}",
            t0.elapsed()
        );
    }

    /// RC2-APP (F0275): the wait has a deadline.
    #[test]
    fn rc2app_the_wait_has_a_deadline() {
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Loading(0.1)));
        let t0 = std::time::Instant::now();
        let got = wait_for_model_within(
            &slot,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(300),
        );
        assert!(got.is_none());
        assert!(t0.elapsed() < std::time::Duration::from_secs(3));
        assert!(MODEL_LOAD_DEADLINE >= std::time::Duration::from_secs(10 * 60));
    }

    #[test]
    fn wait_for_model_outlasts_loading() {
        // SF3 regression: the slot is Loading when the worker starts waiting (the
        // first-use download still in flight); the wait must survive that state
        // and resolve once the loader finishes — instead of the old single
        // ready_model() probe that dropped the utterance.
        let slot: ModelSlot = Arc::new(Mutex::new(ModelState::Loading(0.0)));
        let loader_slot = slot.clone();
        let loader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            // The real loader writes Ready(model); a Failed terminal is enough to
            // prove the Loading state is waited out (no whisper binary needed).
            *loader_slot.lock().unwrap() = ModelState::Failed("done loading".into());
        });
        let t0 = std::time::Instant::now();
        let got = wait_for_model(&slot);
        loader.join().unwrap();
        assert!(got.is_none());
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(300),
            "wait_for_model returned before the load finished"
        );
    }

    // ---- voice priority #1: hands-free turn controller (floor loop) --------

    // n windows of constant amplitude; RMS == |amplitude| so tests reason about
    // the VAD threshold directly (same helper convention as the barge/handsfree
    // kernels). Local so this block is self-contained.
    fn turn_windows(amplitude: f32, n: usize) -> Vec<f32> {
        use awili_voice::vad::WINDOW_SAMPLES;
        vec![amplitude; n * WINDOW_SAMPLES]
    }

    fn turn_controller() -> TurnController {
        use awili_voice::WakeGate;
        // Wake threshold 0.5, refractory 100 samples, idle-timeout spanning
        // several chunks; barge VAD threshold 0.01 / 5-window endpoint; a
        // 3-window barge sustain (same round numbers the sub-kernels' tests use).
        let session = HandsFreeSession::new(WakeGate::new(0.5, 100, 8_000), &Vad::new(0.01, 5));
        TurnController::new(session, BargeInDetector::new(3), &Vad::new(0.01, 5))
    }

    #[test]
    fn starts_on_the_user_floor_and_wakes() {
        let mut c = turn_controller();
        assert!(!c.assistant_speaking());
        assert_eq!(c.feed(0.4, &turn_windows(0.0, 1)), TurnEvent::Idle);
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
        assert!(c.user_mic_live());
    }

    #[test]
    fn captures_a_user_utterance_then_stays_on_the_user_floor() {
        let mut c = turn_controller();
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 2)), TurnEvent::Listening);
        // Fifth consecutive silent window endpoints the utterance.
        match c.feed(0.0, &turn_windows(0.0, 5)) {
            TurnEvent::Utterance(u) => assert!(u.iter().any(|&x| x > 0.05), "speech captured"),
            other => panic!("expected an utterance, got {other:?}"),
        }
        // The floor stays with the user until the caller starts the reply.
        assert!(!c.assistant_speaking());
        assert!(c.user_mic_live(), "session remains armed for a follow-up");
    }

    #[test]
    fn awili_read_aloud_cannot_rewake_the_gate_on_its_own_floor() {
        let mut c = turn_controller();
        c.begin_reply();
        assert!(c.assistant_speaking());
        // Even a maximal wake confidence is ignored on the assistant floor: the
        // read-aloud bleeding into the mic (silence-to-low here) never activates.
        for _ in 0..10 {
            assert_eq!(c.feed(0.99, &turn_windows(0.0, 1)), TurnEvent::Speaking);
        }
        assert!(c.assistant_speaking(), "still Awili's floor");
        assert!(!c.user_mic_live());
    }

    #[test]
    fn sustained_speech_during_playback_barges_in_and_returns_the_floor() {
        let mut c = turn_controller(); // barge sustain = 3
        c.begin_reply();
        // Two in-speech windows are not enough (echo/cough guard).
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        // The third consecutive in-speech window is a deliberate interruption.
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::BargeIn);
        // The floor is back with the user (wake-gated: the barging audio is not
        // itself captured — mic is cold until the next wake word).
        assert!(!c.assistant_speaking());
        assert!(!c.user_mic_live());
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
    }

    #[test]
    fn a_reply_that_drains_returns_to_wake_gated_listening() {
        let mut c = turn_controller();
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.0, 1)), TurnEvent::Speaking);
        c.reply_finished();
        assert!(!c.assistant_speaking());
        // A weak score does not wake; a real wake word does.
        assert_eq!(c.feed(0.4, &turn_windows(0.0, 1)), TurnEvent::Idle);
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
    }

    #[test]
    fn each_reply_gets_a_fresh_barge_vad_no_run_leak() {
        // A first reply accrues two in-speech windows (below the 3-sustain), then
        // drains. The next reply must start the barge run from zero, so its own
        // two in-speech windows likewise do not fire — proving no leak.
        let mut c = turn_controller();
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        c.reply_finished();
        // Second reply.
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        assert!(
            c.assistant_speaking(),
            "two windows never barge a fresh reply"
        );
    }

    #[test]
    fn begin_reply_re_arms_after_a_prior_barge_in() {
        // A reply that ended in a barge-in leaves the detector latched-fired; the
        // next begin_reply must clear it so that reply can itself be interrupted.
        let mut c = turn_controller();
        c.begin_reply();
        c.feed(0.0, &turn_windows(0.1, 1));
        c.feed(0.0, &turn_windows(0.1, 1));
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::BargeIn);
        // New turn, new reply.
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::Speaking);
        assert_eq!(
            c.feed(0.0, &turn_windows(0.1, 1)),
            TurnEvent::BargeIn,
            "the re-armed detector fires again for the new reply"
        );
    }

    #[test]
    fn reply_outcome_is_none_before_and_during_a_reply() {
        let mut c = turn_controller();
        // Nothing has played yet.
        assert_eq!(c.last_reply_outcome(), None);
        // While a reply is in flight the outcome stays None — it describes an
        // *ended* reply, not the live one.
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.0, 1)), TurnEvent::Speaking);
        assert_eq!(
            c.last_reply_outcome(),
            None,
            "a reply still playing has no ended-outcome"
        );
    }

    #[test]
    fn reply_outcome_records_drained_then_interrupted() {
        let mut c = turn_controller();
        // A reply that finishes on its own records Drained.
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.0, 1)), TurnEvent::Speaking);
        c.reply_finished();
        assert_eq!(c.last_reply_outcome(), Some(ReplyOutcome::Drained));
        // The next reply clears the prior outcome while it plays, then a barge-in
        // records Interrupted — the latest reply's fate, never the stale Drained.
        c.begin_reply();
        assert_eq!(
            c.last_reply_outcome(),
            None,
            "begin_reply consumes the prior outcome"
        );
        c.feed(0.0, &turn_windows(0.1, 1));
        c.feed(0.0, &turn_windows(0.1, 1));
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::BargeIn);
        assert_eq!(c.last_reply_outcome(), Some(ReplyOutcome::Interrupted));
    }

    #[test]
    fn reply_finished_on_the_user_floor_cannot_fabricate_a_drained_outcome() {
        // After a barge-in the floor is already the user's and the outcome is
        // Interrupted. A spurious reply_finished (the caller's TTS drain firing
        // late) must not overwrite it with Drained — the guard is the floor check.
        let mut c = turn_controller();
        c.begin_reply();
        c.feed(0.0, &turn_windows(0.1, 1));
        c.feed(0.0, &turn_windows(0.1, 1));
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::BargeIn);
        assert_eq!(c.last_reply_outcome(), Some(ReplyOutcome::Interrupted));
        c.reply_finished(); // no-op: already the user's floor
        assert_eq!(
            c.last_reply_outcome(),
            Some(ReplyOutcome::Interrupted),
            "a late drain on the user floor leaves the barge-in outcome intact"
        );
    }

    #[test]
    fn reset_clears_the_reply_outcome() {
        let mut c = turn_controller();
        c.begin_reply();
        assert_eq!(c.feed(0.0, &turn_windows(0.0, 1)), TurnEvent::Speaking);
        c.reply_finished();
        assert_eq!(c.last_reply_outcome(), Some(ReplyOutcome::Drained));
        // A fresh conversation has no reply behind it to resume.
        c.reset();
        assert_eq!(c.last_reply_outcome(), None);
    }

    #[test]
    fn phase_tracks_the_floor_across_a_whole_turn() {
        let mut c = turn_controller();
        // Freshly built: user floor, mic cold.
        assert_eq!(c.phase(), TurnPhase::Dormant);
        // Wake word fires -> capturing.
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
        assert_eq!(c.phase(), TurnPhase::Listening);
        // Awili takes the floor to read the reply.
        c.begin_reply();
        assert_eq!(c.phase(), TurnPhase::Speaking);
        // Reply drains on its own: back to a cold, wake-gated user floor.
        c.reply_finished();
        assert_eq!(c.phase(), TurnPhase::Dormant);
    }

    #[test]
    fn phase_speaking_wins_over_the_mic_never_a_fourth_state() {
        // On Awili's floor the mic is always cold, so phase() must report Speaking
        // and user_mic_live() false together — the impossible "live mic while
        // speaking" combination cannot appear.
        let mut c = turn_controller();
        c.begin_reply();
        assert_eq!(c.phase(), TurnPhase::Speaking);
        assert!(!c.user_mic_live());
    }

    #[test]
    fn phase_is_dormant_after_a_barge_in() {
        let mut c = turn_controller(); // barge sustain = 3
        c.begin_reply();
        c.feed(0.0, &turn_windows(0.1, 1));
        c.feed(0.0, &turn_windows(0.1, 1));
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 1)), TurnEvent::BargeIn);
        // Floor returned to the user, but wake-gated: mic cold, so Dormant.
        assert_eq!(c.phase(), TurnPhase::Dormant);
    }

    #[test]
    fn phase_is_dormant_after_a_stop_command() {
        let mut c = capturing_controller();
        assert_eq!(c.phase(), TurnPhase::Listening);
        assert_eq!(c.resolve("stop"), TurnAction::Stop);
        // Stop closed the capture session: mic cold on the user floor.
        assert_eq!(c.phase(), TurnPhase::Dormant);
    }

    // ---- voice priority #1: react to phase transitions, not every polled frame ----

    #[test]
    fn phase_tracker_fires_once_per_transition_and_is_quiet_while_steady() {
        // A UI polls phase() every loop step; the tracker must emit an edge only on
        // the steps the phase actually moves, so a status glyph animates once per
        // transition rather than every frame.
        let mut t = PhaseTracker::new(TurnPhase::Dormant);
        // Same phase as seeded: no edge.
        assert_eq!(t.observe(TurnPhase::Dormant), None);
        // Moved.
        assert_eq!(
            t.observe(TurnPhase::Listening),
            Some(PhaseChange {
                from: TurnPhase::Dormant,
                to: TurnPhase::Listening,
            })
        );
        // Polled again, unchanged: quiet, and the recorded phase is unchanged.
        assert_eq!(t.observe(TurnPhase::Listening), None);
        assert_eq!(t.observe(TurnPhase::Listening), None);
        assert_eq!(t.phase(), TurnPhase::Listening);
    }

    #[test]
    fn phase_tracker_follows_a_whole_turn_off_the_live_controller() {
        // Drive a real controller through a full turn and thread each phase() poll
        // through the tracker: it must yield exactly the Dormant->Listening->
        // Speaking->Dormant edge sequence, one edge per real move.
        let mut c = turn_controller();
        let mut t = PhaseTracker::new(c.phase());
        let mut edges = Vec::new();
        let mut record = |c: &TurnController, t: &mut PhaseTracker| {
            if let Some(change) = t.observe(c.phase()) {
                edges.push(change);
            }
        };
        // Freshly built: Dormant, seeded — no edge yet.
        record(&c, &mut t);
        // Wake word fires -> Listening.
        c.feed(0.9, &turn_windows(0.0, 1));
        record(&c, &mut t);
        // Awili takes the floor -> Speaking.
        c.begin_reply();
        record(&c, &mut t);
        // Reply drains -> Dormant.
        c.reply_finished();
        record(&c, &mut t);

        assert_eq!(
            edges,
            vec![
                PhaseChange {
                    from: TurnPhase::Dormant,
                    to: TurnPhase::Listening,
                },
                PhaseChange {
                    from: TurnPhase::Listening,
                    to: TurnPhase::Speaking,
                },
                PhaseChange {
                    from: TurnPhase::Speaking,
                    to: TurnPhase::Dormant,
                },
            ]
        );
        // The floor is always handed back cold, so a reply never returns straight
        // to a live mic: Speaking's outbound edge is always to Dormant, never
        // Listening.
        assert!(
            edges
                .iter()
                .all(|e| e.from != TurnPhase::Speaking || e.to == TurnPhase::Dormant),
            "returning the floor leaves the mic cold: Speaking only ever exits to Dormant"
        );
    }

    #[test]
    fn phase_change_predicates_name_each_arrival() {
        let woke = PhaseChange {
            from: TurnPhase::Dormant,
            to: TurnPhase::Listening,
        };
        assert!(woke.now_listening() && !woke.now_speaking() && !woke.now_dormant());

        let replying = PhaseChange {
            from: TurnPhase::Listening,
            to: TurnPhase::Speaking,
        };
        assert!(replying.now_speaking() && !replying.now_listening() && !replying.now_dormant());

        let asleep = PhaseChange {
            from: TurnPhase::Speaking,
            to: TurnPhase::Dormant,
        };
        assert!(asleep.now_dormant() && !asleep.now_listening() && !asleep.now_speaking());
    }

    // ---- voice priority #1: resolve a transcribed utterance into an action ----

    // Drive the controller to a live user-floor capture (wake word fired, session
    // armed) so `resolve` runs in exactly the state a real transcript arrives in.
    fn capturing_controller() -> TurnController {
        let mut c = turn_controller();
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
        assert!(c.user_mic_live(), "mic live after wake");
        c
    }

    #[test]
    fn resolve_dictation_submits_and_keeps_the_mic() {
        let mut c = capturing_controller();
        assert_eq!(
            c.resolve("what's the weather in tokyo"),
            TurnAction::Submit("what's the weather in tokyo".to_string())
        );
        // Dictation does not move the floor: still the user's, awaiting the reply.
        assert!(!c.assistant_speaking());
        assert!(c.user_mic_live());
    }

    #[test]
    fn resolve_stop_closes_the_capture_mic() {
        let mut c = capturing_controller();
        assert_eq!(c.resolve("stop"), TurnAction::Stop);
        // "stop" leaves conversation mode: the mic is cold and wake-gated again.
        assert!(!c.user_mic_live(), "capture session closed");
        assert!(!c.assistant_speaking());
        // The legacy slice-3 stop words fold into the same action.
        for word in ["cancel", "exit", "quit"] {
            let mut c = capturing_controller();
            assert_eq!(c.resolve(word), TurnAction::Stop, "{word:?}");
            assert!(!c.user_mic_live(), "{word:?} closed the mic");
        }
    }

    #[test]
    fn resolve_new_conversation_resets_to_a_cold_user_floor() {
        let mut c = capturing_controller();
        assert_eq!(c.resolve("new conversation"), TurnAction::NewConversation);
        assert!(!c.assistant_speaking(), "cold user floor");
        assert!(!c.user_mic_live(), "capture session ended");
        // Pristine: a weak score does not wake, then a real wake word re-arms the
        // fresh exchange (the low window re-establishes the gate's rising edge, as
        // the drain test does).
        assert_eq!(c.feed(0.4, &turn_windows(0.0, 1)), TurnEvent::Idle);
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
    }

    #[test]
    fn resolve_draft_commands_leave_the_controller_untouched() {
        // Send/Scratch/Repeat act on app state (composer draft, read-aloud), not
        // the floor: the controller keeps the user's live capture session.
        for (phrase, want) in [
            ("send it", TurnAction::SendDraft),
            ("scratch that", TurnAction::Scratch),
            ("repeat", TurnAction::Repeat),
        ] {
            let mut c = capturing_controller();
            assert_eq!(c.resolve(phrase), want, "{phrase:?}");
            assert!(c.user_mic_live(), "{phrase:?} left the mic live");
            assert!(
                !c.assistant_speaking(),
                "{phrase:?} stayed on the user floor"
            );
        }
    }

    #[test]
    fn resolve_approval_commands_answer_the_gate_and_leave_the_controller_untouched() {
        // "approve"/"deny" answer a pending Work approval card — app state the
        // controller cannot see — so like Send/Scratch/Repeat they carry no floor
        // change and keep the user's live capture. Today (without these arms) the
        // same words fall through to dictation and post a junk user turn while the
        // card stays up; the point of the arms is that they do NOT.
        for (phrase, want) in [
            ("approve", TurnAction::Approve),
            ("approve it", TurnAction::Approve),
            ("allow it", TurnAction::Approve),
            ("deny", TurnAction::Deny),
            ("reject", TurnAction::Deny),
            ("don't allow", TurnAction::Deny),
        ] {
            let mut c = capturing_controller();
            assert_eq!(c.resolve(phrase), want, "{phrase:?}");
            assert!(c.user_mic_live(), "{phrase:?} left the mic live");
            assert!(
                !c.assistant_speaking(),
                "{phrase:?} stayed on the user floor"
            );
        }
    }

    #[test]
    fn resolve_bare_yes_no_stay_dictation_not_approval() {
        // The tight-vocabulary guarantee at the turn-loop level: a spoken "yes"/"no"
        // is a Submit, never an approval answer, so ordinary conversation is never
        // eaten by the gate.
        for word in ["yes", "no", "sure", "okay"] {
            let mut c = capturing_controller();
            assert_eq!(
                c.resolve(word),
                TurnAction::Submit(word.to_string()),
                "{word:?}"
            );
        }
    }

    #[test]
    fn resolve_silence_keeps_listening() {
        let mut c = capturing_controller();
        for t in ["", "   ", "[BLANK_AUDIO]", "(typing)"] {
            assert_eq!(c.resolve(t), TurnAction::KeepListening, "{t:?}");
            assert!(c.user_mic_live(), "{t:?} kept listening");
        }
    }

    #[test]
    fn resolve_utterance_transcribes_captured_samples_then_submits() {
        // The end-to-end seam: drive feed to a real endpointed utterance, then
        // hand exactly those samples to resolve_utterance. The transcriber sees
        // the captured audio (non-empty speech) and its text flows into resolve.
        let mut c = turn_controller();
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
        assert_eq!(c.feed(0.0, &turn_windows(0.1, 2)), TurnEvent::Listening);
        let samples = match c.feed(0.0, &turn_windows(0.0, 5)) {
            TurnEvent::Utterance(u) => u,
            other => panic!("expected an utterance, got {other:?}"),
        };
        let action = c.resolve_utterance(&samples, |s| {
            assert!(s.iter().any(|&x| x > 0.05), "transcriber got the speech");
            Some("what's the weather in tokyo".to_string())
        });
        assert_eq!(
            action,
            TurnAction::Submit("what's the weather in tokyo".to_string())
        );
        // Dictation keeps the user's live capture for the reply.
        assert!(c.user_mic_live());
        assert!(!c.assistant_speaking());
    }

    #[test]
    fn resolve_utterance_control_command_moves_the_floor() {
        // A transcribed control word takes precedence over dictation AND its
        // code-verifiable floor effect is applied through the seam (not just
        // returned): "stop" closes the mic here exactly as bare resolve() does.
        // (Samples are irrelevant to these command tests — the closure is a stub
        // returning fixed text; only test 1 above threads the real captured audio.)
        let mut c = capturing_controller();
        assert_eq!(
            c.resolve_utterance(&[], |_| Some("stop".to_string())),
            TurnAction::Stop
        );
        assert!(
            !c.user_mic_live(),
            "stop closed the capture mic via the seam"
        );
    }

    #[test]
    fn resolve_utterance_failed_or_empty_transcription_keeps_the_mic() {
        // SF3 lesson one level up: an unready/failed Whisper pass (None) — or one
        // that strips to nothing — must never drop the turn. Keep listening, mic
        // stays live, floor unchanged.
        let mut c = capturing_controller();
        assert_eq!(
            c.resolve_utterance(&turn_windows(0.1, 1), |_| None),
            TurnAction::KeepListening,
            "None transcription keeps listening"
        );
        assert!(c.user_mic_live(), "failed transcription kept the mic live");
        // Text that resolve_turn strips to empty folds the same way.
        assert_eq!(
            c.resolve_utterance(&[], |_| Some("[BLANK_AUDIO]".to_string())),
            TurnAction::KeepListening,
            "annotation-only transcript keeps listening"
        );
        assert!(c.user_mic_live());
    }

    #[test]
    fn resolve_stop_from_playback_is_never_reached_but_reset_is_safe() {
        // reset() is also the by-hand "new conversation" button: it must return a
        // pristine user floor even when Awili currently holds it (a reply playing).
        let mut c = turn_controller();
        c.begin_reply();
        assert!(c.assistant_speaking());
        c.reset();
        assert!(!c.assistant_speaking(), "reset hands the floor back");
        assert!(!c.user_mic_live(), "and lands cold/wake-gated");
        assert_eq!(c.feed(0.9, &turn_windows(0.0, 1)), TurnEvent::Activated);
    }

    #[test]
    fn drive_runs_a_whole_turn_in_one_call_per_chunk() {
        // The one-call loop from cold to a finished, resolved utterance. Every
        // pre-utterance chunk yields its TurnEvent with no action and never calls
        // the transcriber; the endpointing chunk yields Utterance + the resolved
        // Submit in the same call.
        let mut c = turn_controller();
        let mut transcribes = 0;
        // A closure factory so each call gets a fresh FnOnce that also counts.
        let mut drive = |c: &mut TurnController, conf: f32, s: &[f32], text: Option<&str>| {
            let t = text.map(str::to_string);
            let (ev, act) = c.drive(conf, s, |captured| {
                transcribes += 1;
                assert!(captured.iter().any(|&x| x > 0.05), "transcriber got speech");
                t
            });
            (ev, act)
        };

        // Wake, then speech, then a silent tail that endpoints the utterance.
        assert_eq!(
            drive(&mut c, 0.9, &turn_windows(0.0, 1), None),
            (TurnEvent::Activated, None)
        );
        assert_eq!(
            drive(&mut c, 0.0, &turn_windows(0.1, 2), None),
            (TurnEvent::Listening, None)
        );
        let (ev, act) = drive(
            &mut c,
            0.0,
            &turn_windows(0.0, 5),
            Some("what's the weather in tokyo"),
        );
        assert!(matches!(ev, TurnEvent::Utterance(_)), "endpointed: {ev:?}");
        assert_eq!(
            act,
            Some(TurnAction::Submit(
                "what's the weather in tokyo".to_string()
            ))
        );
        assert_eq!(
            transcribes, 1,
            "transcribe ran once — only on the utterance feed"
        );
        // Dictation kept the user's live capture for the reply.
        assert!(c.user_mic_live());
        assert!(!c.assistant_speaking());
    }

    #[test]
    fn drive_never_transcribes_a_chunk_without_a_finished_utterance() {
        // Idle/Activated/Listening and the whole assistant floor (Speaking,
        // BargeIn) all pair with None and must not spend a Whisper pass — the
        // transcriber panics if drive ever calls it here.
        let mut c = turn_controller();
        let no_transcribe = |c: &mut TurnController, conf: f32, s: &[f32]| {
            c.drive(conf, s, |_| -> Option<String> {
                panic!("transcribe must not run without an endpointed utterance")
            })
        };

        assert_eq!(
            no_transcribe(&mut c, 0.4, &turn_windows(0.0, 1)),
            (TurnEvent::Idle, None)
        );
        assert_eq!(
            no_transcribe(&mut c, 0.9, &turn_windows(0.0, 1)),
            (TurnEvent::Activated, None)
        );
        assert_eq!(
            no_transcribe(&mut c, 0.0, &turn_windows(0.1, 1)),
            (TurnEvent::Listening, None)
        );
        // Assistant floor: read-aloud (Speaking) then a sustained barge-in.
        c.begin_reply();
        assert_eq!(
            no_transcribe(&mut c, 0.99, &turn_windows(0.0, 1)),
            (TurnEvent::Speaking, None)
        );
        assert_eq!(
            no_transcribe(&mut c, 0.0, &turn_windows(0.1, 1)),
            (TurnEvent::Speaking, None)
        );
        assert_eq!(
            no_transcribe(&mut c, 0.0, &turn_windows(0.1, 1)),
            (TurnEvent::Speaking, None)
        );
        assert_eq!(
            no_transcribe(&mut c, 0.0, &turn_windows(0.1, 1)),
            (TurnEvent::BargeIn, None)
        );
    }

    #[test]
    fn drive_control_command_moves_the_floor_and_failed_pass_keeps_the_mic() {
        // The seam's floor effects flow through drive unchanged: a transcribed
        // "stop" closes the mic; a later None/empty pass on a fresh utterance
        // keeps listening without dropping the turn.
        let mut c = capturing_controller();
        // Endpoint an utterance, transcribed as the control word "stop".
        // Wake already happened in capturing_controller. drive takes one chunk,
        // so feed speech first (does not endpoint yet) then the silent tail.
        let (ev, act) = c.drive(0.0, &turn_windows(0.1, 2), |_| Some("stop".to_string()));
        assert_eq!((ev, act), (TurnEvent::Listening, None));
        let (ev, act) = c.drive(0.0, &turn_windows(0.0, 5), |_| Some("stop".to_string()));
        assert!(matches!(ev, TurnEvent::Utterance(_)));
        assert_eq!(act, Some(TurnAction::Stop));
        assert!(!c.user_mic_live(), "stop closed the mic through drive");

        // A failed Whisper pass (None) on a fresh endpointed utterance folds to
        // KeepListening through drive too — the turn is never dropped (SF3). Wake
        // a new session first, since "stop" just closed the mic.
        let mut c = capturing_controller();
        assert_eq!(
            c.drive(0.0, &turn_windows(0.1, 2), |_| None),
            (TurnEvent::Listening, None)
        );
        let (ev, act) = c.drive(0.0, &turn_windows(0.0, 5), |_| None);
        assert!(matches!(ev, TurnEvent::Utterance(_)));
        assert_eq!(act, Some(TurnAction::KeepListening));
        assert!(
            c.user_mic_live(),
            "a failed pass kept the mic live through drive"
        );
    }

    #[test]
    fn drive_tracked_yields_the_phase_edge_alongside_the_action() {
        // One call per chunk drives the turn AND reports the resting-phase edge, so
        // a UI animates listening/speaking/asleep off the same loop step it acts on.
        // Seed the tracker at the fresh controller's Dormant, matching a UI asleep.
        let mut c = turn_controller();
        let mut t = PhaseTracker::new(c.phase());

        // Wake fires: Activated, no action yet, and a Dormant->Listening edge.
        let (ev, act, change) = c.drive_tracked(&mut t, 0.9, &turn_windows(0.0, 1), |_| {
            panic!("no utterance yet")
        });
        assert_eq!((ev, act), (TurnEvent::Activated, None));
        assert_eq!(
            change,
            Some(PhaseChange {
                from: TurnPhase::Dormant,
                to: TurnPhase::Listening,
            })
        );

        // Speech mid-utterance: Listening, no action, and the phase held steady
        // (still Listening) so no edge fires — the glyph does not re-animate.
        let (ev, act, change) = c.drive_tracked(&mut t, 0.0, &turn_windows(0.1, 2), |_| {
            panic!("not endpointed yet")
        });
        assert_eq!((ev, act), (TurnEvent::Listening, None));
        assert_eq!(change, None, "phase steady at Listening: no edge");

        // Silent tail endpoints the utterance; dictation keeps the mic, so the
        // action is Submit and the phase is *still* Listening — again no edge.
        let (ev, act, change) = c.drive_tracked(&mut t, 0.0, &turn_windows(0.0, 5), |_| {
            Some("send this to my accountant".to_string())
        });
        assert!(matches!(ev, TurnEvent::Utterance(_)));
        assert_eq!(
            act,
            Some(TurnAction::Submit("send this to my accountant".to_string()))
        );
        assert_eq!(
            change, None,
            "dictation kept the mic live: phase held at Listening"
        );
    }

    #[test]
    fn drive_tracked_surfaces_a_stop_commands_dormant_edge_in_the_same_call() {
        // The whole point of sampling phase *after* drive resolves: a control
        // command's floor effect is already applied, so its edge comes back in the
        // same call that returns the action — not one poll later.
        let mut c = capturing_controller();
        // capturing_controller already woke the mic: seed the tracker there so the
        // Listening->Dormant edge is the only one this test can produce.
        let mut t = PhaseTracker::new(c.phase());
        assert_eq!(t.phase(), TurnPhase::Listening);

        // Speech then a silent tail carrying the control word "stop".
        let (_, act, change) = c.drive_tracked(&mut t, 0.0, &turn_windows(0.1, 2), |_| {
            panic!("not endpointed yet")
        });
        assert_eq!(act, None);
        assert_eq!(change, None, "still capturing: phase steady at Listening");

        let (ev, act, change) = c.drive_tracked(&mut t, 0.0, &turn_windows(0.0, 5), |_| {
            Some("stop".to_string())
        });
        assert!(matches!(ev, TurnEvent::Utterance(_)));
        assert_eq!(act, Some(TurnAction::Stop));
        assert_eq!(
            change,
            Some(PhaseChange {
                from: TurnPhase::Listening,
                to: TurnPhase::Dormant,
            }),
            "stop closed the mic: its Dormant edge rides back with the action"
        );
        assert!(!c.user_mic_live());
    }

    // ---- RC2-R4 (F0956): barge-in while a reply is read aloud ----------

    fn said(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// Awili's own voice coming back through the mic never stops the reply,
    /// even when its sentence says "stop"; the owner's stop does, alone or
    /// over Awili's words; other talk does nothing.
    #[test]
    fn rc2r4_awilis_own_voice_never_stops_the_reply() {
        let spoken = said(&[
            "Stop the service first.",
            "Then drain the node and stop k3s on it.",
        ]);
        for echo in [
            "Stop.",
            "stop the service first",
            "Then drain the node and stop K3S on it.",
            "[BLANK_AUDIO] drain the node and stop",
            "and stop k3s",
            "",
        ] {
            assert_eq!(
                playback_barge_action(echo, &spoken),
                BargeAction::Ignore,
                "{echo:?} is Awili's own voice"
            );
        }
        for stop in [
            "stop talking",
            "Stop, stop.",
            "That's enough.",
            "Awili, stop.",
            "cancel",
            "quit",
            "stop talking the service first",
        ] {
            assert_eq!(
                playback_barge_action(stop, &spoken),
                BargeAction::StopReply,
                "{stop:?} is the owner saying stop"
            );
        }
        // With nothing of Awili's in it, a bare "stop" is the owner's.
        assert_eq!(
            playback_barge_action("Stop.", &said(&["The cluster has three nodes."])),
            BargeAction::StopReply
        );
        for other in ["uh huh", "please don't stop helping me", "what about hostb"] {
            assert_eq!(
                playback_barge_action(other, &spoken),
                BargeAction::Ignore,
                "{other:?}"
            );
        }
    }

    /// The catcher keeps only sustained talk-over (with the audio from just
    /// before it fired), hands it back at the silence endpoint, and listens
    /// again for the rest of the reply; short blips never fire.
    #[test]
    fn rc2r4_the_catcher_keeps_sustained_talk_over_only() {
        use awili_voice::vad::WINDOW_SAMPLES;
        let vad = Vad::new(0.01, 5);
        let mut c = BargeCatcher::new(&vad);
        let feed = |amp: f32, n: usize, c: &mut BargeCatcher| {
            let mut out = None;
            for w in turn_windows(amp, n).chunks(WINDOW_SAMPLES) {
                if let Some(u) = c.feed(w) {
                    assert!(out.is_none(), "one talk-over at a time");
                    out = Some(u);
                }
            }
            out
        };
        // Blips (80 ms) with gaps: with the VAD's 5-window hangover they stay
        // under the sustain run, so nothing fires.
        for _ in 0..20 {
            assert!(feed(0.2, 4, &mut c).is_none());
            assert!(feed(0.0, 6, &mut c).is_none());
        }
        // Sustained talk-over, then silence: one utterance with the pre-roll.
        assert!(feed(0.2, 30, &mut c).is_none());
        let u = feed(0.0, 6, &mut c).expect("the talk-over ends at the silence endpoint");
        assert!(u.len() >= 30 * WINDOW_SAMPLES, "{} samples", u.len());
        // All 30 loud windows are in it: the 10 before it fired come from the
        // pre-roll.
        let loud = u.iter().filter(|s| **s > 0.1).count();
        assert_eq!(loud, 30 * WINDOW_SAMPLES, "the pre-roll kept the start");
        // Back on Awili's floor: it catches the next one too.
        assert!(feed(0.2, 30, &mut c).is_none());
        assert!(feed(0.0, 6, &mut c).is_some());
        // Endless talk-over is cut at the cap.
        let long = feed(0.2, BARGE_MAX_SAMPLES / WINDOW_SAMPLES + 40, &mut c).expect("capped");
        assert!(
            long.len() <= BARGE_MAX_SAMPLES + WINDOW_SAMPLES,
            "{}",
            long.len()
        );
    }
}

#[cfg(test)]
#[path = "voice_barge_app_tests.rs"]
mod barge_app_tests;
