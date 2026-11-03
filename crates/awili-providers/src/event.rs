//! The provider-agnostic conversation types and the streamed-event vocabulary
//! every [`ChatProvider`](crate::ChatProvider) emits.

use futures::stream::BoxStream;

/// Who authored a turn in the request. The base preamble is carried separately
/// on [`ChatRequest`] because the wire formats place it out-of-band (Anthropic's
/// top-level `system`, Ollama's leading `system` message). `System` (slice 18)
/// lets the app additionally prepend a per-conversation system message (an
/// applied prompt template); each adapter maps it onto its format's system
/// slot — Anthropic hoists it into the top-level field, Ollama emits a
/// `"system"`-role message in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One prior turn in the conversation handed to the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Everything a provider needs to start a streaming completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRequest {
    pub model: String,
    /// The system preamble — in slice 2 this is the memory-injected standing
    /// directives (priority >= 90) plus any base instructions.
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
}

/// Token accounting reported by the provider at the end of a stream. Fields are
/// best-effort: a provider that omits a count leaves it at 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Cached input is a subset of input_tokens; None means unreported.
    pub cached_input_tokens: Option<u32>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Saturating `u64 -> u32` for provider-reported token counts (M5). Providers
/// hand us usage as JSON `u64`; a bad upstream frame or an aggregate count above
/// `u32::MAX` must clamp, not wrap. The downstream `CostTicker` already uses
/// saturating arithmetic, so a plain `as u32` truncation here would silently
/// under-report cost and persisted usage before it ever reached that clamp.
pub(crate) fn clamp_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// One event in a streamed completion. Provider wire formats are normalized
/// here, including nonterminal agent activity and failed tool attempts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEvent {
    /// A chunk of generated text to append to the reply bubble.
    Token(String),
    /// Token accounting (usually arrives near the end).
    Usage(Usage),
    /// The context window the provider reported for this turn's model, in tokens.
    ContextWindow(u32),
    /// The stream finished cleanly. Always the last event a consumer sees, even
    /// if the upstream connection was cut (the decoder synthesises it so the UI
    /// never hangs waiting for a reply that will not come).
    Done,
    /// A transport, HTTP, or upstream error. Followed by [`ProviderEvent::Done`].
    Error(String),
    /// One line of agent ACTIVITY — a tool/command step ("using fs.read…",
    /// "✓ Bash: cargo test") — separated from [`Token`](Self::Token) so the
    /// UI can fold plumbing into a collapsible per-turn summary instead of
    /// interleaving it with the answer (design critique P2, 2026-08-23).
    Activity(String),
    /// Awili RC2-R2G7: a tool/command step BEGAN: its plain label
    /// ("Read notes.txt", "Bash: cargo test", "$ ls"). Shown like
    /// [`Activity`](Self::Activity); kept apart so a surface listing the
    /// tools a turn used (a council card's "Used:") counts real tools only,
    /// never a harness note such as a connection retry.
    ToolStarted(String),
    /// A tool attempt failed, but the harness may recover in the same turn.
    /// This is not a terminal stream error and must not abort the consumer.
    ToolAttemptFailed,
}

/// A streaming chat backend. `stream` is non-blocking: it kicks off the request
/// on a worker thread and hands back an async stream of [`ProviderEvent`]s that a
/// GPUI task drains. The returned stream always terminates with
/// [`ProviderEvent::Done`].
pub trait ChatProvider: Send + Sync {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent>;
    /// Awili RC1-N-FIX1: whether this backend's own hooks bring the owner's
    /// memory (his Claude Code / Codex hooks, by his explicit setting), so
    /// the app pins none of its own memory into the request. Default: no.
    fn memory_from_hooks(&self) -> bool {
        false
    }
}
