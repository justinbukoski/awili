//! S3: one typed category for every run failure (Work, Chat, Code, Council
//! member), the action that fixes it, and the short run id that ties an error
//! card, Inspect reply and the log line of one run together.
//!
//! Pure: [`classify`] reads the failure text the existing sources already
//! produce (provider events, `SeamError`, CLI exit text, app notices); no
//! source changes its own wording.
use awili_providers::ProviderErrorKind;
use awili_seam::{SeamError, StopCause};

/// What kind of failure ended a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunErrorCategory {
    /// The model's service could not be reached or stopped answering.
    Connection,
    /// The provider answered with a failure (overloaded, server error, protocol).
    Provider,
    /// The helper or provider is signed out or its key was refused.
    SignedOut,
    /// Rate limit or out of quota/credits.
    RateLimitOrQuota,
    /// A rule or approval blocked the action.
    Policy,
    /// Something on this Mac is missing (helper program, folder, setup).
    LocalSetup,
    /// The owner (or the host) stopped the run.
    Cancelled,
    Unknown,
}

/// The one primary action a failure card offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunErrorAction {
    SignIn,
    Retry,
    ChooseFolder,
    OpenSettings,
    TryAnotherModel,
    None,
}

impl RunErrorAction {
    /// The button text; `None` for no action.
    pub(crate) fn label(self) -> Option<&'static str> {
        match self {
            Self::SignIn => Some("Sign in"),
            Self::Retry => Some("Retry"),
            Self::ChooseFolder => Some("Choose folder"),
            Self::OpenSettings => Some("Open settings"),
            Self::TryAnotherModel => Some("Try another model"),
            Self::None => None,
        }
    }
}

impl RunErrorCategory {
    /// Plain card title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Connection => "Couldn\u{2019}t reach the model",
            Self::Provider => "The model had a problem",
            Self::SignedOut => "Signed out",
            Self::RateLimitOrQuota => "Limit reached",
            Self::Policy => "Blocked by a rule",
            Self::LocalSetup => "Setup needed",
            Self::Cancelled => "Stopped",
            Self::Unknown => "Something went wrong",
        }
    }

    /// Whether the owner can fix it from this Mac.
    pub(crate) fn user_can_fix(self) -> bool {
        matches!(
            self,
            Self::Connection
                | Self::SignedOut
                | Self::RateLimitOrQuota
                | Self::Policy
                | Self::LocalSetup
        )
    }
}

/// A classified failure: category plus the primary action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunError {
    pub(crate) category: RunErrorCategory,
    pub(crate) action: RunErrorAction,
}

impl RunError {
    fn new(category: RunErrorCategory, action: RunErrorAction) -> Self {
        Self { category, action }
    }
}

fn has(lower: &str, words: &[&str]) -> bool {
    words.iter().any(|w| lower.contains(w))
}

/// `w` as a whole word (no letter or digit on either side).
fn word(lower: &str, w: &str) -> bool {
    lower.match_indices(w).any(|(i, _)| {
        !lower[..i]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            && !lower[i + w.len()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
    })
}

/// An HTTP status `code` written as one ("HTTP 503", "status: 502", "error
/// 401", "(429)"): a whole number right after such a lead-in. A bare number
/// in a path, size or line number is not a status.
fn http_status(lower: &str, code: &str) -> bool {
    lower.match_indices(code).any(|(i, _)| {
        let (before, after) = (&lower[..i], &lower[i + code.len()..]);
        if before
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            || after.chars().next().is_some_and(char::is_alphanumeric)
        {
            return false;
        }
        let lead = before.trim_end_matches([' ', ':']);
        [
            "http", "http/1.1", "http/2", "status", "error", "code", "response", "(",
        ]
        .iter()
        .any(|c| lead.ends_with(c))
    })
}

/// Map failure text from any existing source into a [`RunError`].
pub(crate) fn classify(text: &str) -> RunError {
    use RunErrorAction as A;
    use RunErrorCategory as C;
    let l = text.to_ascii_lowercase();
    // S1b: the run never started because its instructions could not be written.
    if l.contains("could not prepare this run's instructions") {
        return RunError::new(C::LocalSetup, A::Retry);
    }
    // rc.4 S7: the opt-in sandbox could not be set up, so the turn did not start
    // (never silently run unsandboxed). Settings is where it is turned off.
    if l.contains("couldn't start the sandbox") {
        return RunError::new(C::LocalSetup, A::OpenSettings);
    }
    // A rule or the owner said no. A filesystem "permission denied" is not a rule.
    if has(
        &l,
        &[
            "blocked by rule",
            "blocked by a rule",
            "blocked by policy",
            "denied by the owner",
            "owner denied",
            "you denied",
            "approval was denied",
            "approval required",
            "needs your approval",
            "not allowed by",
            "outside the project folder",
            "refused by rule",
        ],
    ) {
        return RunError::new(C::Policy, A::None);
    }
    if has(
        &l,
        &[
            "stopped by user",
            "turn stopped by host",
            "cancelled",
            "canceled",
        ],
    ) {
        return RunError::new(C::Cancelled, A::None);
    }
    if crate::submit_guard::is_missing_folder_notice(text)
        || has(&l, &["need a working folder", "no longer there"])
    {
        return RunError::new(C::LocalSetup, A::ChooseFolder);
    }
    if crate::agent_signin::is_signed_out_notice(text)
        || has(&l, &["unauthorized", "not logged in", "signed out"])
        || http_status(&l, "401")
    {
        return RunError::new(C::SignedOut, A::SignIn);
    }
    if crate::work::provider_setup_hint(text).is_some()
        || crate::work::is_engine_setup_notice(text)
        || has(
            &l,
            &[
                "could not start",
                "failed to spawn",
                "no such file",
                "command not found",
                "not configured",
                "needs its key",
                "missing key",
            ],
        )
    {
        return RunError::new(C::LocalSetup, A::OpenSettings);
    }
    if has(
        &l,
        &[
            "quota",
            "billing",
            "credit balance",
            "insufficient credit",
            "out of credit",
            "usage limit",
            "rate_limit",
            "rate limit",
            "rate-limit",
            "too many requests",
        ],
    ) || http_status(&l, "429")
    {
        return RunError::new(C::RateLimitOrQuota, A::TryAnotherModel);
    }
    if has(&l, &["overloaded"]) || http_status(&l, "529") {
        return RunError::new(C::Provider, A::Retry);
    }
    if has(
        &l,
        &[
            "timed out",
            "timeout",
            "deadline",
            "connection refused",
            "could not connect",
            "error sending request",
            "unreachable",
            "transport:",
            "request failed",
        ],
    ) || word(&l, "dns")
    {
        return RunError::new(C::Connection, A::Retry);
    }
    if has(
        &l,
        &[
            "internal server error",
            "bad gateway",
            "provider protocol",
            "turn failed",
            "context:",
            "harness:",
            "service unavailable",
        ],
    ) || ["500", "502", "503"].iter().any(|c| http_status(&l, c))
    {
        return RunError::new(C::Provider, A::TryAnotherModel);
    }
    RunError::new(C::Unknown, A::Retry)
}

// Typed entry points for callers that hold the error value; the app itself
// receives provider and seam failures as text, which `classify` reads.
#[cfg_attr(not(test), allow(dead_code))]
/// Map a provider-layer kind (when the caller already has one).
pub(crate) fn classify_provider_kind(kind: ProviderErrorKind, msg: &str) -> RunError {
    match kind {
        ProviderErrorKind::Quota | ProviderErrorKind::RateLimit => RunError::new(
            RunErrorCategory::RateLimitOrQuota,
            RunErrorAction::TryAnotherModel,
        ),
        ProviderErrorKind::Overloaded => {
            RunError::new(RunErrorCategory::Provider, RunErrorAction::Retry)
        }
        ProviderErrorKind::Other => classify(msg),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Map a typed seam failure.
pub(crate) fn classify_seam(err: &SeamError) -> RunError {
    use RunErrorAction as A;
    use RunErrorCategory as C;
    match err {
        SeamError::Transport(m) => {
            let inner = classify(m);
            if inner.category == C::Unknown {
                RunError::new(C::Connection, A::Retry)
            } else {
                inner
            }
        }
        SeamError::Protocol(_) => RunError::new(C::Provider, A::TryAnotherModel),
        SeamError::Storage(_) => RunError::new(C::LocalSetup, A::None),
        SeamError::Harness(_) => RunError::new(C::Unknown, A::Retry),
        SeamError::Cancelled(StopCause::Denied) => RunError::new(C::Policy, A::None),
        SeamError::Cancelled(_) => RunError::new(C::Cancelled, A::None),
        SeamError::Context(_) => RunError::new(C::Provider, A::TryAnotherModel),
        SeamError::TurnFailed { reason, .. } => classify(reason),
        #[allow(unreachable_patterns)]
        _ => classify(&err.to_string()),
    }
}

/// A fresh short run id (8 hex characters).
pub(crate) fn new_run_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// The run id carried by a longer per-turn token (its first 8 characters).
pub(crate) fn run_id_of_token(token: &str) -> String {
    token.chars().take(8).collect()
}

/// The engine log line that opens a run; the same id appears in its error
/// card and in Inspect reply.
pub(crate) fn log_line(run_id: &str, kind: &str, conv_id: &str) -> String {
    format!("awili: run {run_id} started ({kind}) conv={conv_id}")
}

/// Write [`log_line`] to the app log (stderr). The engine's own framed log
/// is written only by the engine worker, so the id is not appended there.
pub(crate) fn log_run_start(run_id: &str, kind: &str, conv_id: &str) {
    eprintln!("{}", log_line(run_id, kind, conv_id));
}

/// Run ids by (conversation, reply timestamp) so Inspect reply can show the
/// id of the run that produced a reply, saved with the conversation
/// (`run_ids:<conv>`, any conversation kind) and deleted with it.
///
/// A saved row is keyed by the reply's STORED timestamp; a restored
/// conversation gets fresh in-memory timestamps, so [`Self::restore`] pairs
/// the stored rows with the rebuilt messages in order (as the reply footers do).
#[derive(Debug, Default)]
pub(crate) struct RunIds {
    live: std::collections::HashMap<(String, u64), String>,
    saved: std::collections::HashMap<String, std::collections::BTreeMap<u64, String>>,
}

impl RunIds {
    /// The `app_state` key of `conv`'s saved run ids.
    pub(crate) fn key(conv: &str) -> String {
        format!("run_ids:{conv}")
    }

    /// Remember the run that produced the reply at `ts`; returns the JSON to
    /// persist under [`Self::key`] when something changed.
    pub(crate) fn set(&mut self, conv: &str, ts: u64, run_id: &str) -> Option<String> {
        if run_id.is_empty() {
            return None;
        }
        self.live.insert((conv.to_string(), ts), run_id.to_string());
        let saved = self.saved.entry(conv.to_string()).or_default();
        if saved.insert(ts, run_id.to_string()).as_deref() == Some(run_id) {
            return None;
        }
        serde_json::to_string(saved).ok()
    }

    pub(crate) fn get(&self, conv: &str, ts: u64) -> Option<&str> {
        self.live.get(&(conv.to_string(), ts)).map(String::as_str)
    }

    /// Put `raw` (a saved row) back on a restored conversation: stored
    /// message `i` that `restore_conversation` kept pairs with `convo`'s
    /// message `i`.
    pub(crate) fn restore(
        &mut self,
        conv: &str,
        raw: &str,
        msgs: &[awili_persist::StoredMessage],
        convo: &awili_core::ConversationState,
    ) {
        let Ok(saved) = serde_json::from_str::<std::collections::BTreeMap<u64, String>>(raw) else {
            return;
        };
        let kept = msgs
            .iter()
            .filter(|m| crate::role_from_str(&m.role).is_some());
        for (stored, message) in kept.zip(convo.messages()) {
            let stored_ts = u64::try_from(stored.ts).unwrap_or_default();
            if let Some(id) = saved.get(&stored_ts) {
                self.live.insert((conv.to_string(), message.ts), id.clone());
            }
        }
        self.saved.insert(conv.to_string(), saved);
    }

    /// The conversation was deleted.
    pub(crate) fn forget(&mut self, conv: &str) {
        self.live.retain(|(c, _), _| c != conv);
        self.saved.remove(conv);
    }
}

#[cfg(test)]
#[path = "run_error_tests.rs"]
mod run_error_tests;
