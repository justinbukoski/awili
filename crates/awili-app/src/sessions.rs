//! Sessions panel — browse past Hermes agent sessions.
//!
//! Fetches from `GET /api/sessions` and shows session history with metadata
//! (model, tokens, duration, status).

use awili_gateway::rest::{HermesSession, RestClient};

/// Which slice of history the Sessions browser shows. Local conversations are
/// filtered by their [`awili_core::ConvKind`]; `Hermes` shows remote agent
/// sessions; `All` shows everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionsView {
    #[default]
    All,
    Work,
    Hermes,
}

impl SessionsView {
    pub fn label(self) -> &'static str {
        match self {
            SessionsView::All => "All",
            SessionsView::Work => "Work",
            SessionsView::Hermes => "Hermes",
        }
    }

    /// The tabs, in display order.
    pub fn all() -> [SessionsView; 3] {
        [SessionsView::All, SessionsView::Work, SessionsView::Hermes]
    }
}

/// The sessions panel state.
#[derive(Clone, Debug)]
pub struct SessionsState {
    pub sessions: Vec<HermesSession>,
    pub selected_id: Option<String>,
    /// The fetched transcript for `selected_id` (I1): `(session_id, rows)`.
    pub transcript: Option<(String, Vec<(String, String)>)>,
    pub transcript_loading: bool,
    pub(crate) transcript_request: Option<String>,
    pub(crate) transcript_generation: u64,
    pub transcript_error: Option<String>,
    pub filter: String,
    /// Which history slice the browser shows (tab selection).
    pub view: SessionsView,
    pub loading: bool,
    pub error: Option<String>,
}

// The fetch/selection surface lands with the sessions panel (Phase 5).
#[allow(dead_code)]
impl SessionsState {
    pub fn new() -> Self {
        Self {
            transcript: None,
            transcript_loading: false,
            transcript_request: None,
            transcript_generation: 0,
            transcript_error: None,
            sessions: Vec::new(),
            selected_id: None,
            filter: String::new(),
            view: SessionsView::All,
            loading: false,
            error: None,
        }
    }

    pub fn fetch(&mut self, client: &RestClient) {
        self.loading = true;
        self.error = None;
        match client.list_sessions() {
            Ok(sessions) => {
                self.sessions = sessions;
                self.loading = false;
            }
            Err(e) => {
                self.error = Some(format!("{e}"));
                self.loading = false;
            }
        }
    }

    pub fn selected(&self) -> Option<&HermesSession> {
        let id = self.selected_id.as_deref()?;
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn filtered(&self) -> Vec<&HermesSession> {
        if self.filter.is_empty() {
            return self.sessions.iter().collect();
        }
        let q = self.filter.to_lowercase();
        self.sessions
            .iter()
            .filter(|s| {
                s.title.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    || s.model.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    || s.source
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .collect()
    }

    /// Total token usage across all loaded sessions.
    pub fn total_tokens(&self) -> (u64, u64) {
        let input: u64 = self.sessions.iter().filter_map(|s| s.input_tokens).sum();
        let output: u64 = self.sessions.iter().filter_map(|s| s.output_tokens).sum();
        (input, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, title: &str, model: &str) -> HermesSession {
        HermesSession {
            id: id.to_string(),
            source: Some("desktop".to_string()),
            model: Some(model.to_string()),
            title: Some(title.to_string()),
            started_at: Some(1000.0),
            ended_at: Some(2000.0),
            end_reason: None,
            message_count: Some(5),
            tool_call_count: Some(2),
            input_tokens: Some(1000),
            output_tokens: Some(500),
            preview: None,
            last_active: None,
        }
    }

    #[test]
    fn filter_by_title() {
        let mut state = SessionsState::new();
        state.sessions = vec![
            make_session("1", "Debug CSS", "gpt-4"),
            make_session("2", "Write tests", "claude"),
        ];
        state.filter = "debug".to_string();
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "1");
    }

    #[test]
    fn filter_by_model() {
        let mut state = SessionsState::new();
        state.sessions = vec![
            make_session("1", "Task A", "gpt-4"),
            make_session("2", "Task B", "claude"),
        ];
        state.filter = "claude".to_string();
        assert_eq!(state.filtered().len(), 1);
    }

    #[test]
    fn filter_by_source_is_case_insensitive() {
        // `filtered()` also matches the session `source` (which client the
        // session originated from) and lowercases the query before comparing.
        // Neither is exercised by filter_by_title/filter_by_model: both pass an
        // already-lowercase query that hits only title or model. A regression
        // dropping the `source` arm (sessions.rs:98) or the query
        // `.to_lowercase()` (line 84) would silently break the Sessions
        // browser's source filter — this guards both in one shot.
        let mut state = SessionsState::new();
        let mut desktop = make_session("1", "Task A", "gpt-4");
        desktop.source = Some("desktop".to_string());
        let mut mobile = make_session("2", "Task B", "claude");
        mobile.source = Some("mobile".to_string());
        state.sessions = vec![desktop, mobile];
        // Mixed-case query matches only session 2's source — no title/model hit.
        state.filter = "MOBILE".to_string();
        let filtered = state.filtered();
        assert_eq!(
            filtered.len(),
            1,
            "source arm + query lowercasing must match"
        );
        assert_eq!(filtered[0].id, "2");
    }

    #[test]
    fn filter_by_model_is_case_insensitive() {
        // `filtered()` lowercases each session's `model` before comparing
        // (sessions.rs:96). filter_by_model exercises the model arm but with an
        // already-lowercase model ("claude") vs a lowercase query, so removing
        // the model field's `.to_lowercase()` still matches there; likewise the
        // source test's models are all lowercase. So no test drives a
        // capitalized model name against a lowercase query — a regression
        // dropping the model-field lowercasing would silently break the
        // Sessions browser model filter for any model with uppercase letters
        // (e.g. "Claude-Opus"). This pins that per-field lowercasing.
        let mut state = SessionsState::new();
        state.sessions = vec![
            make_session("1", "Task A", "Claude-Opus"),
            make_session("2", "Task B", "gpt-4"),
        ];
        // Lowercase query hits only session 1's model — not its title/source.
        state.filter = "opus".to_string();
        let filtered = state.filtered();
        assert_eq!(
            filtered.len(),
            1,
            "model field must be lowercased before matching"
        );
        assert_eq!(filtered[0].id, "1");
    }

    #[test]
    fn tabs_render_in_fixed_order_with_fixed_labels() {
        // The Sessions browser tab bar is built by iterating `SessionsView::all()`
        // and rendering each tab's `label()`. Neither the tab display order (the
        // `all()` array) nor the per-variant label text (`label()`'s 4-arm match)
        // has any other test coverage — both had ZERO. So a reorder of the
        // `all()` array (e.g. swapping Chat/Work) or a `label()` typo would
        // silently change the visible tab bar and survive the whole suite.
        // Pinning the full ordered label list catches both an array reorder AND
        // a label typo in one observable assertion.
        let labels: Vec<&'static str> = SessionsView::all().iter().map(|v| v.label()).collect();
        assert_eq!(labels, ["All", "Work", "Hermes"]);
    }

    #[test]
    fn total_tokens() {
        let mut state = SessionsState::new();
        state.sessions = vec![make_session("1", "A", "m"), make_session("2", "B", "m")];
        let (input, output) = state.total_tokens();
        assert_eq!(input, 2000);
        assert_eq!(output, 1000);
    }

    #[test]
    fn selected_session() {
        let mut state = SessionsState::new();
        state.sessions = vec![make_session("abc", "Test", "m")];
        state.selected_id = Some("abc".to_string());
        assert!(state.selected().is_some());
        state.selected_id = Some("nope".to_string());
        assert!(state.selected().is_none());
    }
}

/// Parse the dashboard's `GET /api/sessions/{id}/messages` payload into
/// `(role, text)` rows — leniently: a bare array or a `{messages:[…]}`
/// envelope; text from `content` (string), `text`, or an array of
/// `{type:"text", text}` parts. Unknown shapes yield an empty row list, never
/// an error — the transcript view says "no messages" rather than crashing.
pub fn parse_session_messages(v: &serde_json::Value) -> Vec<(String, String)> {
    let items = match v {
        serde_json::Value::Array(a) => a.as_slice(),
        serde_json::Value::Object(o) => match o.get("messages") {
            Some(serde_json::Value::Array(a)) => a.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    items
        .iter()
        .filter_map(|m| {
            let role = m
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("message")
                .to_string();
            let text = match m.get("content") {
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(serde_json::Value::Array(parts)) => Some(
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => m.get("text").and_then(|t| t.as_str()).map(str::to_string),
            }?;
            if text.trim().is_empty() {
                return None;
            }
            Some((role, text))
        })
        .collect()
}

#[cfg(test)]
mod transcript_tests {
    use super::*;

    #[test]
    fn parses_the_dashboard_envelope_with_string_content() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"session_id":"s1","messages":[
                {"id":1,"role":"user","content":"hi"},
                {"id":2,"role":"assistant","content":"hello"},
                {"id":3,"role":"assistant","content":""}
            ]}"#,
        )
        .unwrap();
        let rows = parse_session_messages(&v);
        assert_eq!(rows.len(), 2, "empty content rows are dropped");
        assert_eq!(rows[0], ("user".into(), "hi".into()));
    }

    #[test]
    fn parses_bare_arrays_and_part_lists() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[{"role":"user","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]"#,
        )
        .unwrap();
        let rows = parse_session_messages(&v);
        assert_eq!(rows, vec![("user".to_string(), "a\nb".to_string())]);
        assert!(parse_session_messages(&serde_json::json!(42)).is_empty());
    }
}
