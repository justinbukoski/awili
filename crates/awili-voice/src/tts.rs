//! Read-aloud: a [`TtsBackend`] trait with two implementations (slice-3 spec §3).
//!
//! * [`SayTts`] — the macOS `say` command. Zero dependencies, works day one; the
//!   fallback when no HTTP endpoint is configured.
//! * [`HttpTts`] — a configurable fleet-TTS endpoint. POSTs the sentence, gets
//!   synthesized audio back, and plays it with the macOS `afplay` tool.
//!
//! Both backends *shell out* (`say` / `afplay`) rather than linking an audio
//! output library, so the whole module compiles on Linux (the slice DoD) while the
//! actual playback is mac-only at runtime — smoke-tested live by Foreman. Playback
//! is sentence-chunked and stoppable: [`speak_all`] walks chunks in order and
//! checks a shared [`CancelToken`] between (and during) each one, so the composer's
//! stop button cuts off mid-speech.
//!
//! The unit tests here cover the pieces that can be wrong without audio hardware:
//! the cancel-token semantics, the HTTP request builder, and that [`speak_all`]
//! stops feeding chunks the instant it is cancelled.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::config::VoiceConfig;

/// How long the HTTP TTS backend will wait to connect. A dead endpoint must not
/// pin the speech worker forever (review SF5).
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Overall deadline for one synthesis request (send + body). Bounds how long a
/// stale worker can outlive a Stop on an unresponsive endpoint (review SF5):
/// blocking reqwest cannot be interrupted mid-call, but it *can* be bounded.
const HTTP_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);

/// A shared "stop now" flag. Cloning shares the same underlying bool, so the UI
/// thread can cancel an utterance a worker thread is mid-way through speaking.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Reset to the un-cancelled state, reused for the next utterance.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Why read-aloud failed.
#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("spawning {tool}: {source}")]
    Spawn {
        tool: &'static str,
        source: std::io::Error,
    },
    #[error("{tool} exited with {status}")]
    Status { tool: &'static str, status: String },
    #[error("tts http request: {0}")]
    Http(String),
    #[error("tts audio io: {0}")]
    Io(#[from] std::io::Error),
}

/// A speech backend. `speak` blocks until the chunk has been spoken or `cancel`
/// fires, whichever comes first; it must return promptly once cancelled (killing
/// any child process it spawned).
pub trait TtsBackend: Send + Sync {
    fn speak(&self, text: &str, cancel: &CancelToken) -> Result<(), TtsError>;
    /// A short name for diagnostics / the composer's status.
    fn name(&self) -> &'static str;
}

/// Speak a sequence of chunks in order, stopping the instant `cancel` fires —
/// between chunks and (via the backend) during one. A cancelled run returns
/// `Ok(())`: a user-pressed stop is not an error.
pub fn speak_all<I>(
    backend: &dyn TtsBackend,
    chunks: I,
    cancel: &CancelToken,
) -> Result<(), TtsError>
where
    I: IntoIterator<Item = String>,
{
    for chunk in chunks {
        if cancel.is_cancelled() {
            break;
        }
        if chunk.trim().is_empty() {
            continue;
        }
        backend.speak(&chunk, cancel)?;
    }
    Ok(())
}

/// A running read-aloud worker: a background thread that speaks enqueued
/// sentences in order through the configured backend, stoppable at any time.
///
/// This is the streaming half of read-aloud — the composer feeds it sentences as
/// the [`SentenceChunker`](crate::SentenceChunker) closes them mid-generation (or
/// all at once for the per-reply speaker button). [`stop`](Self::stop) cuts off
/// immediately (cancels the in-flight chunk and abandons the queue);
/// [`finish`](Self::finish) lets the queue drain naturally at end of reply. A
/// `done` channel fires once the worker exits so the UI can clear its speaking
/// state.
pub struct SpeechQueue {
    tx: Option<std::sync::mpsc::Sender<String>>,
    cancel: CancelToken,
}

impl SpeechQueue {
    /// Spawn the worker for `cfg`'s backend. Returns the queue plus a `done`
    /// receiver that yields once the worker thread exits (queue drained or
    /// stopped).
    pub fn start(cfg: &VoiceConfig) -> (SpeechQueue, async_channel::Receiver<()>) {
        Self::start_with_backend(backend_for(cfg))
    }

    /// Spawn the worker for an explicit backend; [`start`](Self::start) picks
    /// the backend from config.
    pub fn start_with_backend(
        backend: Box<dyn TtsBackend>,
    ) -> (SpeechQueue, async_channel::Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let (done_tx, done_rx) = async_channel::bounded::<()>(1);
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            run_queue(backend.as_ref(), &rx, &worker_cancel);
            let _ = done_tx.send_blocking(());
        });
        (
            SpeechQueue {
                tx: Some(tx),
                cancel,
            },
            done_rx,
        )
    }

    /// Queue a sentence to speak after the current ones. No-op once stopped.
    pub fn enqueue(&self, sentence: String) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(sentence);
        }
    }

    /// Signal end-of-reply: stop accepting new sentences and let the worker drain
    /// the queue, then exit (firing `done`).
    pub fn finish(&mut self) {
        self.tx = None;
    }

    /// Stop immediately: cancel the in-flight chunk and abandon the queue. The
    /// worker exits (firing `done`) without speaking anything further.
    pub fn stop(&mut self) {
        self.cancel.cancel();
        self.tx = None;
    }
}

/// After this many sentences in a row fail, the backend is taken as down and
/// the rest of the reply is dropped. A single failure skips only its own
/// sentence (review F0973).
const MAX_FAILURES_IN_A_ROW: usize = 3;

/// The speech worker's loop: speak each queued sentence in order until the
/// queue is finished or stopped. A sentence that fails is logged and skipped;
/// the next one is still spoken (review F0973), unless
/// [`MAX_FAILURES_IN_A_ROW`] sentences have failed in a row.
fn run_queue(
    backend: &dyn TtsBackend,
    rx: &std::sync::mpsc::Receiver<String>,
    cancel: &CancelToken,
) {
    let mut failures_in_a_row = 0;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        // Err: the sender dropped and the queue is drained.
        let Ok(sentence) = rx.recv() else {
            break;
        };
        if cancel.is_cancelled() {
            break;
        }
        match backend.speak(&sentence, cancel) {
            Ok(()) => failures_in_a_row = 0,
            Err(e) => {
                failures_in_a_row += 1;
                eprintln!(
                    "awili: read-aloud ({}) skipped a sentence: {e}",
                    backend.name()
                );
                if failures_in_a_row >= MAX_FAILURES_IN_A_ROW {
                    eprintln!(
                        "awili: read-aloud ({}) stopped after {failures_in_a_row} failures in a row",
                        backend.name()
                    );
                    break;
                }
            }
        }
    }
}

/// Choose the backend for a config: the HTTP endpoint when one is set, else the
/// macOS `say` fallback (slice-3 spec §3).
pub fn backend_for(cfg: &VoiceConfig) -> Box<dyn TtsBackend> {
    match cfg.tts_endpoint.as_deref().filter(|s| !s.is_empty()) {
        Some(endpoint) => Box::new(
            HttpTts::new(endpoint.to_string())
                .with_model(cfg.tts_model.clone())
                .with_voice(cfg.tts_voice.clone()),
        ),
        None => Box::new(SayTts::new()),
    }
}

/// Wait for `child`, killing it promptly if `cancel` fires. Polls so a stop is
/// honoured within ~30 ms even though the child has no cancellation channel.
fn wait_cancellable(
    mut child: Child,
    tool: &'static str,
    cancel: &CancelToken,
) -> Result<(), TtsError> {
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                return Err(TtsError::Status {
                    tool,
                    status: status.to_string(),
                });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => return Err(TtsError::Spawn { tool, source: e }),
        }
    }
}

/// The macOS `say` command backend — the zero-dependency fallback.
#[derive(Default)]
pub struct SayTts;

impl SayTts {
    pub fn new() -> Self {
        Self
    }
}

impl TtsBackend for SayTts {
    fn speak(&self, text: &str, cancel: &CancelToken) -> Result<(), TtsError> {
        let child = Command::new("say")
            .arg("--")
            .arg(text)
            .spawn()
            .map_err(|e| TtsError::Spawn {
                tool: "say",
                source: e,
            })?;
        wait_cancellable(child, "say", cancel)
    }

    fn name(&self) -> &'static str {
        "say"
    }
}

/// A fleet-TTS HTTP endpoint backend. POSTs the sentence as JSON and plays the
/// returned audio with `afplay`.
pub struct HttpTts {
    endpoint: String,
    /// `tts_model` from config (slice 11): sent as `"model"` in the request
    /// when set, so the endpoint picks the configured voice.
    model: Option<String>,
    /// `tts_voice` from config: the OpenAI `/v1/audio/speech` voice id
    /// (kokoro `af_heart` etc.).
    voice: Option<String>,
}

impl HttpTts {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: None,
            voice: None,
        }
    }

    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model.filter(|m| !m.is_empty());
        self
    }

    pub fn with_voice(mut self, voice: Option<String>) -> Self {
        self.voice = voice.filter(|v| !v.is_empty());
        self
    }

    /// Fetch synthesized audio for `text` from the endpoint (blocking). Extracted
    /// from [`speak`](Self::speak) so the request shape is unit-testable without a
    /// live server. Connect and overall deadlines are bounded (review SF5) so an
    /// unresponsive endpoint cannot keep a stale speech thread alive indefinitely
    /// after Stop.
    fn fetch_audio(&self, text: &str) -> Result<Vec<u8>, TtsError> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_TOTAL_TIMEOUT)
            .build()
            .map_err(|e| TtsError::Http(e.to_string()))?;
        let resp = build_tts_request(
            &client,
            &self.endpoint,
            text,
            self.model.as_deref(),
            self.voice.as_deref(),
        )
        .send()
        .map_err(|e| TtsError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(TtsError::Http(format!(
                "HTTP {} from {}",
                resp.status(),
                self.endpoint
            )));
        }
        let bytes = resp.bytes().map_err(|e| TtsError::Http(e.to_string()))?;
        Ok(bytes.to_vec())
    }
}

impl TtsBackend for HttpTts {
    fn speak(&self, text: &str, cancel: &CancelToken) -> Result<(), TtsError> {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let audio = self.fetch_audio(text)?;
        if cancel.is_cancelled() {
            return Ok(());
        }
        // Play through a temp file: afplay reads a path, not stdin. The guard's
        // name is process-unique (pid + monotonic sequence — review SF5: length
        // alone collided across overlapping stale/new workers), the file is
        // created exclusively and owner-only (review T31), and its Drop
        // removes the file on EVERY exit path, including a failed spawn
        // (review nit 2).
        let temp = TempAudioFile::create(&audio)?;
        let child = Command::new("afplay")
            .arg(temp.path())
            .spawn()
            .map_err(|e| TtsError::Spawn {
                tool: "afplay",
                source: e,
            })?;
        wait_cancellable(child, "afplay", cancel)
    }

    fn name(&self) -> &'static str {
        "http"
    }
}

/// Monotonic per-process sequence for temp audio filenames: two workers in the
/// same process can never pick the same name, regardless of payload size or
/// timing (review SF5).
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// How many names [`TempAudioFile::create`] tries before giving up.
const TEMP_NAME_ATTEMPTS: usize = 16;

/// Open `path` for writing only if nothing exists there yet (`O_EXCL`: an
/// existing file or symlink is refused, never followed or truncated), with
/// owner-only permissions on Unix (review T31).
fn open_new_private(path: &std::path::Path) -> Result<std::fs::File, std::io::Error> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// An RAII temporary audio file: created uniquely-named with the given bytes,
/// removed on drop — so every error, cancellation, and success path cleans up
/// (review nit 2: a failed `afplay` spawn used to leak the file).
struct TempAudioFile {
    path: PathBuf,
}

impl TempAudioFile {
    /// Create a new temp audio file holding `bytes`. The name carries the pid,
    /// a per-process sequence and a time-based nonce, and the file is created
    /// exclusively (never opening something already there, symlinks included)
    /// and readable by the owner only (review T31). A name that is taken is
    /// skipped for the next one.
    fn create(bytes: &[u8]) -> Result<Self, std::io::Error> {
        Self::create_from(bytes, Self::next_candidate)
    }

    /// The next name [`create`](Self::create) tries, in the system temp
    /// directory.
    fn next_candidate() -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!(
            "awili-tts-{}-{seq}-{nonce:08x}.wav",
            std::process::id()
        ))
    }

    /// Create the file at the first name `candidate` yields that is free, trying
    /// at most [`TEMP_NAME_ATTEMPTS`] names. Each name is opened exactly as given
    /// with [`open_new_private`], so whatever is already there (a symlink
    /// included) is skipped, never followed or removed. The name source is a
    /// parameter so a test can aim it at an exact path.
    fn create_from(
        bytes: &[u8],
        mut candidate: impl FnMut() -> PathBuf,
    ) -> Result<Self, std::io::Error> {
        let mut last_err = None;
        for _ in 0..TEMP_NAME_ATTEMPTS {
            let path = candidate();
            match open_new_private(&path) {
                Ok(mut f) => {
                    // The guard exists only once we created the file, so Drop
                    // never removes something that was there before; if the
                    // writes below fail, Drop still removes our file.
                    let guard = Self { path };
                    f.write_all(bytes)?;
                    f.flush()?;
                    return Ok(guard);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no free temp audio file name",
            )
        }))
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempAudioFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Build the TTS POST request: JSON `{"text": "..."}` — plus `"model"` when a
/// `tts_model` is configured (slice 11) — to the endpoint, asking for audio
/// back. Pure so the contract is testable without a socket.
fn build_tts_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    text: &str,
    model: Option<&str>,
    voice: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    // Both dialects in one body: `input` (+ optional `voice`) is the OpenAI
    // `/v1/audio/speech` shape the fleet's kokoro-on-lemond service speaks;
    // `text` is the legacy fleet-TTS shape. Servers ignore the key they don't
    // know. The accept header stays permissive — lemond returns audio/mpeg,
    // the legacy service audio/wav, and afplay plays either from a file.
    let mut body = serde_json::json!({ "text": text, "input": text });
    if let Some(model) = model {
        body["model"] = serde_json::Value::String(model.to_string());
    }
    if let Some(voice) = voice {
        body["voice"] = serde_json::Value::String(voice.to_string());
    }
    client
        .post(endpoint)
        .header("accept", "audio/mpeg, audio/wav, audio/*")
        .json(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A backend that records the chunks it was asked to speak, optionally
    /// cancelling itself after the first one (to model a stop landing mid-run).
    struct MockTts {
        spoken: Mutex<Vec<String>>,
        cancel_after_first: Option<CancelToken>,
    }

    impl TtsBackend for MockTts {
        fn speak(&self, text: &str, _cancel: &CancelToken) -> Result<(), TtsError> {
            self.spoken.lock().unwrap().push(text.to_string());
            if let Some(tok) = &self.cancel_after_first {
                tok.cancel();
            }
            Ok(())
        }
        fn name(&self) -> &'static str {
            "mock"
        }
    }

    #[test]
    fn finish_drops_the_sender_so_the_worker_exits_and_done_fires() {
        // Codex SF1 plumbing: `finish()` is the only thing that lets a queue
        // with no further input die — the sender drops, the worker's recv()
        // unblocks, and `done` fires (the signal that clears the speaking state
        // and re-arms the conversation-mode mic). A queue that is never
        // finished blocks its worker on recv() forever.
        // An unroutable HTTP backend: the enqueued sentence fails fast instead
        // of being spoken aloud on a dev machine.
        let cfg = VoiceConfig {
            tts_endpoint: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        };
        let (mut q, done) = SpeechQueue::start(&cfg);
        q.enqueue("never spoken".to_string());
        q.finish();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut fired = false;
        while std::time::Instant::now() < deadline {
            if done.try_recv().is_ok() {
                fired = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fired,
            "done must fire once the finished queue's worker exits"
        );
    }

    #[test]
    fn wait_cancellable_maps_child_exit_status_to_ok_or_status_err() {
        // `wait_cancellable` is what turns an `afplay`/`say` process outcome into
        // the queue's success-or-error signal (tts.rs:186): a clean exit must
        // return Ok, a non-zero exit must surface `TtsError::Status` so the
        // worker logs it and counts the failure instead of silently pretending the audio
        // played. Nothing exercised this mapping, so collapsing the branch to
        // always-Ok (dropping the `status.success()` check) would let a failed
        // playback pass as success while the whole suite stayed green. Real
        // child processes make the two exit paths deterministic — the poll loop
        // handles the not-yet-exited case, so no timing assumption is needed.
        let uncancelled = CancelToken::new();

        let ok_child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        assert!(
            wait_cancellable(ok_child, "afplay", &uncancelled).is_ok(),
            "a zero-exit child must map to Ok"
        );

        let fail_child = Command::new("sh").arg("-c").arg("exit 7").spawn().unwrap();
        match wait_cancellable(fail_child, "afplay", &uncancelled) {
            Err(TtsError::Status { tool, .. }) => assert_eq!(tool, "afplay"),
            other => panic!("a non-zero exit must map to TtsError::Status, got {other:?}"),
        }
    }

    #[test]
    fn wait_cancellable_kills_the_child_and_returns_ok_when_pre_cancelled() {
        // The cancel branch (tts.rs:188) is what makes `stop()` prompt: the
        // token is checked at the top of every loop iteration, BEFORE
        // `try_wait`, so an already-cancelled token kills the child on the very
        // first pass and returns Ok without ever waiting on — or inspecting the
        // exit status of — the process. Nothing exercised this: dropping the
        // cancel branch would leave `wait_cancellable` blocking until the child
        // exited on its own (defeating stop()) yet the whole tts suite stayed
        // green. Spawn a child that would sleep 30s then exit non-zero if left
        // alone; a pre-cancelled token must instead kill it and return Ok
        // promptly. If the branch were gone this would block ~30s and then
        // surface `TtsError::Status`, so both the result AND the promptness pin
        // the behaviour.
        let cancelled = CancelToken::new();
        cancelled.cancel();

        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30; exit 7")
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let result = wait_cancellable(child, "afplay", &cancelled);
        let elapsed = started.elapsed();

        assert!(
            result.is_ok(),
            "a pre-cancelled token must kill the child and return Ok, not wait for its exit status"
        );
        // Independently pin promptness: the child would sleep 30s, so returning
        // well under that proves it was killed on the first loop pass rather
        // than waited out. A broken impl that returned Ok slowly fails here.
        assert!(
            elapsed < Duration::from_secs(5),
            "cancel must kill the child promptly, took {elapsed:?}"
        );
    }

    #[test]
    fn cancel_token_shares_state_across_clones() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled());
        a.reset();
        assert!(!b.is_cancelled());
    }

    #[test]
    fn speak_all_feeds_every_chunk_in_order() {
        let backend = MockTts {
            spoken: Mutex::new(Vec::new()),
            cancel_after_first: None,
        };
        let cancel = CancelToken::new();
        speak_all(&backend, ["One.".to_string(), "Two.".to_string()], &cancel).unwrap();
        assert_eq!(*backend.spoken.lock().unwrap(), vec!["One.", "Two."]);
    }

    #[test]
    fn speak_all_stops_after_cancellation() {
        let cancel = CancelToken::new();
        let backend = MockTts {
            spoken: Mutex::new(Vec::new()),
            cancel_after_first: Some(cancel.clone()),
        };
        // The first chunk speaks and cancels; the loop must not feed the second.
        speak_all(
            &backend,
            [
                "First.".to_string(),
                "Second.".to_string(),
                "Third.".to_string(),
            ],
            &cancel,
        )
        .unwrap();
        assert_eq!(*backend.spoken.lock().unwrap(), vec!["First."]);
    }

    #[test]
    fn speak_all_skips_blank_chunks() {
        let backend = MockTts {
            spoken: Mutex::new(Vec::new()),
            cancel_after_first: None,
        };
        let cancel = CancelToken::new();
        speak_all(&backend, ["  ".to_string(), "Hi.".to_string()], &cancel).unwrap();
        assert_eq!(*backend.spoken.lock().unwrap(), vec!["Hi."]);
    }

    #[test]
    fn pre_cancelled_run_speaks_nothing() {
        let backend = MockTts {
            spoken: Mutex::new(Vec::new()),
            cancel_after_first: None,
        };
        let cancel = CancelToken::new();
        cancel.cancel();
        speak_all(&backend, ["x.".to_string()], &cancel).unwrap();
        assert!(backend.spoken.lock().unwrap().is_empty());
    }

    #[test]
    fn http_request_posts_json_text_to_endpoint() {
        let client = reqwest::blocking::Client::new();
        let req = build_tts_request(&client, "http://tts.local/say", "Hello.", None, None)
            .build()
            .unwrap();
        assert_eq!(req.method(), "POST");
        assert_eq!(req.url().as_str(), "http://tts.local/say");
        let body = req.body().unwrap().as_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(json["text"], "Hello.");
        // No model configured — the key must be absent, not null/empty.
        assert!(json.get("model").is_none());
    }

    #[test]
    fn http_request_carries_the_configured_tts_model() {
        let client = reqwest::blocking::Client::new();
        let req = build_tts_request(
            &client,
            "http://tts.local/say",
            "Hello.",
            Some("af_bella"),
            None,
        )
        .build()
        .unwrap();
        let body = req.body().unwrap().as_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(json["text"], "Hello.");
        assert_eq!(json["model"], "af_bella");
    }

    #[test]
    fn with_model_drops_an_empty_model_so_no_model_key_is_sent() {
        // A config `tts_model = ""` (present-but-unset) must NOT put `"model": ""`
        // in the request: build_tts_request only omits the key when the model is
        // None, so `with_model`'s `.filter(|m| !m.is_empty())` is the SOLE place
        // that normalizes an empty string to None. Keeping Some("") would ask the
        // endpoint for a voice named "" and get the wrong (or a rejected) synthesis.
        // Every existing model test passes the model straight to build_tts_request,
        // bypassing with_model, so this filter was wholly unpinned.
        let tts = HttpTts::new("http://tts.local/say").with_model(Some(String::new()));
        assert!(
            tts.model.is_none(),
            "empty tts_model must normalize to None"
        );
        let client = reqwest::blocking::Client::new();
        let req = build_tts_request(
            &client,
            "http://tts.local/say",
            "Hi.",
            tts.model.as_deref(),
            None,
        )
        .build()
        .unwrap();
        let body = req.body().unwrap().as_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert!(
            json.get("model").is_none(),
            "empty model must be omitted from the request"
        );
        // A real, non-empty model is preserved (guards against over-filtering).
        let kept = HttpTts::new("http://tts.local/say").with_model(Some("af_bella".to_string()));
        assert_eq!(kept.model.as_deref(), Some("af_bella"));
    }

    #[test]
    fn http_request_carries_input_and_voice_for_openai_endpoints() {
        // The fleet's kokoro-on-lemond service speaks the OpenAI
        // `/v1/audio/speech` dialect: `input` + `voice`. Both ride alongside
        // the legacy `text` key in one body.
        let client = reqwest::blocking::Client::new();
        let req = build_tts_request(
            &client,
            "http://tts.local/v1/audio/speech",
            "Hello.",
            Some("kokoro-v1"),
            Some("af_heart"),
        )
        .build()
        .unwrap();
        let body = req.body().unwrap().as_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(json["input"], "Hello.");
        assert_eq!(json["text"], "Hello.");
        assert_eq!(json["model"], "kokoro-v1");
        assert_eq!(json["voice"], "af_heart");
        // No voice configured → key absent, like model.
        let req = build_tts_request(&client, "http://tts.local/say", "Hi.", None, None)
            .build()
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert!(json.get("voice").is_none());
    }

    #[test]
    fn http_request_accepts_fleet_audio_types() {
        // The endpoint returns raw audio; the accept header is what negotiates WAV
        // rather than mp3/json. Nothing else pins it, so dropping or changing the
        // header would silently let the server pick a format playback can't decode.
        let client = reqwest::blocking::Client::new();
        let req = build_tts_request(&client, "http://tts.local/say", "Hello.", None, None)
            .build()
            .unwrap();
        assert_eq!(
            req.headers().get("accept").unwrap(),
            "audio/mpeg, audio/wav, audio/*",
            "lemond answers audio/mpeg; the legacy service audio/wav"
        );
    }

    #[test]
    fn temp_audio_files_are_unique_and_removed_on_drop() {
        // SF5: equal-sized payloads used to collide on the same name; the
        // monotonic sequence makes every guard distinct. Nit 2: Drop removes the
        // file, covering the failed-spawn path that used to leak it.
        let a = TempAudioFile::create(b"same-bytes").unwrap();
        let b = TempAudioFile::create(b"same-bytes").unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().exists() && b.path().exists());
        let (pa, pb) = (a.path().clone(), b.path().clone());
        drop(a);
        drop(b);
        assert!(!pa.exists() && !pb.exists());
    }

    #[test]
    fn backend_selection_follows_config() {
        let mut cfg = VoiceConfig::default();
        assert_eq!(backend_for(&cfg).name(), "say");
        cfg.tts_endpoint = Some("http://tts.local/say".to_string());
        assert_eq!(backend_for(&cfg).name(), "http");
        // An empty endpoint is treated as unset → the say fallback.
        cfg.tts_endpoint = Some(String::new());
        assert_eq!(backend_for(&cfg).name(), "say");
    }

    // --- F0973: one failed sentence does not end read-aloud ---

    /// A backend that fails the sentences listed in `fail` (or every sentence
    /// when `fail_all`), recording every sentence it was asked to speak.
    struct FlakyTts {
        asked: Arc<Mutex<Vec<String>>>,
        fail: Vec<&'static str>,
        fail_all: bool,
    }

    impl TtsBackend for FlakyTts {
        fn speak(&self, text: &str, _cancel: &CancelToken) -> Result<(), TtsError> {
            self.asked.lock().unwrap().push(text.to_string());
            if self.fail_all || self.fail.contains(&text) {
                return Err(TtsError::Http("HTTP 500 from test".into()));
            }
            Ok(())
        }
        fn name(&self) -> &'static str {
            "flaky"
        }
    }

    /// Queue `sentences` on a worker with `backend`, finish, and wait for done.
    fn speak_through_queue(backend: FlakyTts, sentences: &[&str]) {
        let (mut q, done) = SpeechQueue::start_with_backend(Box::new(backend));
        for s in sentences {
            q.enqueue(s.to_string());
        }
        q.finish();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while done.try_recv().is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker must exit once the finished queue drains"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn rc2_voice_one_failed_sentence_does_not_end_read_aloud() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let backend = FlakyTts {
            asked: asked.clone(),
            fail: vec!["Two."],
            fail_all: false,
        };
        speak_through_queue(backend, &["One.", "Two.", "Three.", "Four."]);
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["One.", "Two.", "Three.", "Four."],
            "the sentences after a failed one are still spoken"
        );
    }

    #[test]
    fn rc2_voice_a_backend_that_keeps_failing_is_given_up() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let backend = FlakyTts {
            asked: asked.clone(),
            fail: Vec::new(),
            fail_all: true,
        };
        speak_through_queue(backend, &["A.", "B.", "C.", "D.", "E."]);
        assert_eq!(
            asked.lock().unwrap().len(),
            MAX_FAILURES_IN_A_ROW,
            "a backend that is down is not tried for every sentence"
        );
    }

    #[test]
    fn rc2_voice_a_success_resets_the_failure_count() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let backend = FlakyTts {
            asked: asked.clone(),
            fail: vec!["A.", "B.", "D.", "E."],
            fail_all: false,
        };
        speak_through_queue(backend, &["A.", "B.", "C.", "D.", "E.", "F."]);
        assert_eq!(asked.lock().unwrap().len(), 6, "no three failures in a row");
    }

    // --- T31: the temp audio file is never written through a planted link ---

    /// A private scratch directory holding a victim file, for the link tests.
    #[cfg(unix)]
    fn link_fixture(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("awili-tts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        (dir, victim)
    }

    /// T31 (batch review P3): a symlink planted at the exact name `create`
    /// opens is skipped — the victim is untouched, the link is left alone, and
    /// the file lands at the next name, plain and owner-only.
    #[cfg(unix)]
    #[test]
    fn rc2_voice_temp_audio_never_writes_through_a_planted_link() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, victim) = link_fixture("t31");
        let planted = dir.join("planted.wav");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let free = dir.join("free.wav");
        let mut names = vec![planted.clone(), free.clone()].into_iter();
        let mut asked = Vec::new();

        let created = TempAudioFile::create_from(b"audio bytes", || {
            let name = names.next().expect("only two names are needed");
            asked.push(name.clone());
            name
        });

        let temp = created.unwrap();
        let victim_now = std::fs::read(&victim).unwrap();
        let planted_meta = std::fs::symlink_metadata(&planted).unwrap();
        let meta = std::fs::symlink_metadata(temp.path()).unwrap();
        let contents = std::fs::read(temp.path()).unwrap();
        let temp_path = temp.path().clone();
        drop(temp);
        let planted_after_drop = std::fs::symlink_metadata(&planted).is_ok();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            asked,
            vec![planted, free.clone()],
            "the planted name was tried first"
        );
        assert_eq!(victim_now, b"keep me", "the link target is untouched");
        assert!(
            planted_meta.file_type().is_symlink(),
            "the link is left alone"
        );
        assert_eq!(temp_path, free, "the file went to the next name");
        assert!(meta.file_type().is_file(), "a plain new file, not a link");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "owner-only");
        assert_eq!(contents, b"audio bytes");
        assert!(
            planted_after_drop,
            "dropping the file never removes the link"
        );
    }

    /// When every name tried is taken by a link, `create` gives up after
    /// [`TEMP_NAME_ATTEMPTS`] names without writing anything.
    #[cfg(unix)]
    #[test]
    fn rc2_voice_temp_audio_gives_up_when_every_name_is_a_link() {
        let (dir, victim) = link_fixture("t31c");
        let planted = dir.join("planted.wav");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let mut tries = 0;

        let created = TempAudioFile::create_from(b"audio bytes", || {
            tries += 1;
            planted.clone()
        });

        let victim_now = std::fs::read(&victim).unwrap();
        let still_link = std::fs::symlink_metadata(&planted)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        std::fs::remove_dir_all(&dir).ok();
        let err = created.err().expect("no free name");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(tries, TEMP_NAME_ATTEMPTS);
        assert_eq!(victim_now, b"keep me");
        assert!(still_link);
    }

    /// `create` takes its names from `next_candidate`: the private,
    /// process-unique scheme in the system temp directory.
    #[test]
    fn rc2_voice_temp_audio_create_uses_the_unique_names() {
        let temp = TempAudioFile::create(b"x").unwrap();
        let name = temp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(temp.path().parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            name.starts_with(&format!("awili-tts-{}-", std::process::id()))
                && name.ends_with(".wav"),
            "{name}"
        );
        let next = TempAudioFile::next_candidate();
        assert_ne!(next, *temp.path());
        assert_eq!(next.parent(), temp.path().parent());
    }

    #[cfg(unix)]
    #[test]
    fn rc2_voice_temp_audio_open_refuses_an_existing_link() {
        let dir = std::env::temp_dir().join(format!("awili-tts-t31b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let link = dir.join("link.wav");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let err = open_new_private(&link).expect_err("an existing link is refused");
        let victim_now = std::fs::read(&victim).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(victim_now, b"keep me");
    }
}
