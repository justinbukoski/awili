//! Record checks (RC2-OPSF4): evidence the harness reads itself, after a
//! probe's answer text has passed. A model can write that a sub-agent or a
//! Codex review ran; only the records those tools leave behind show it.
//!
//! Every probe with a check carries `{nonce}` in its prompt; the runner puts
//! a fresh value there for each probe, so a record counts only when it holds
//! this probe's nonce (an older run's records never do). Records are read
//! where the tools write them:
//!
//! - Claude Code session transcripts, `<claude home>/projects/<project>/<session>.jsonl`,
//!   with each sub-agent's transcript in `<session>/subagents/agent-*.jsonl`
//!   (older versions keep sub-agent records in the session file, marked
//!   `isSidechain` with an `agentId`);
//! - Codex session records, `<codex home>/sessions/**/rollout-*.jsonl`.
//!
//! An executor whose records are not there (a run through another tool)
//! fails the check, saying so: an unverifiable claim is not a pass.

use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde_json::Value;

use crate::evaluate::{evaluate, Outcome};
use crate::manifest::{Expect, RecordCheck};

/// Where Claude Code and Codex keep their records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRoots {
    /// Claude Code's config directory (holds `projects/`).
    pub claude_home: PathBuf,
    /// Codex's home directory (holds `sessions/`).
    pub codex_home: PathBuf,
}

impl RecordRoots {
    /// The tools' own settings: `CLAUDE_CONFIG_DIR` (else `~/.claude`) and
    /// `CODEX_HOME` (else `~/.codex`).
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let pick = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(fallback))
        };
        Self {
            claude_home: pick("CLAUDE_CONFIG_DIR", ".claude"),
            codex_home: pick("CODEX_HOME", ".codex"),
        }
    }
}

/// Check `check` for the probe run marked `nonce` (the marker the runner
/// put in its prompt), whose answer was `answer`. The records are the
/// evidence (RC2-OPSJ2): most checks never read the answer, and the two
/// that do ([`RecordCheck::ReportedHits`], [`RecordCheck::CodexVerdict`])
/// hold it to what the records show. Records are found by the marker
/// alone, so a replay of a recorded run (a fixed marker,
/// `AWILI_SELFTEST_NONCE`) checks the same way. `Err` says, in plain
/// words, what is missing.
pub fn verify(
    check: &RecordCheck,
    roots: &RecordRoots,
    nonce: &str,
    answer: &str,
) -> Result<(), String> {
    if nonce.is_empty() {
        return Err("no run marker, so no record can be tied to this run".into());
    }
    match check {
        RecordCheck::Subagents { count, reply } => subagents(roots, nonce, *count, reply),
        RecordCheck::CodexRun { last_line } => codex_run(roots, nonce, last_line),
        RecordCheck::ToolHits { calls } => tool_hits(roots, nonce, calls),
        RecordCheck::ReportedHits { calls } => reported_hits(roots, nonce, calls, answer),
        RecordCheck::CodexVerdict { verdicts } => codex_verdict(roots, nonce, verdicts, answer),
    }
}

/// The JSON records of a JSONL file (unreadable lines are skipped).
fn records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Files directly in `dir` whose name satisfies `keep`, sorted.
fn files_in(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| p.file_name().is_some_and(|n| keep(&n.to_string_lossy())))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// The text of a message `content`: a string, or the text of its blocks
/// (`text` blocks, and the content of `tool_result` blocks).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text.to_owned()
                } else if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    block.get("content").map(content_text).unwrap_or_default()
                } else {
                    String::new()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn kind(record: &Value) -> &str {
    record.get("type").and_then(Value::as_str).unwrap_or("")
}

fn timestamp(record: &Value) -> Option<DateTime<FixedOffset>> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
}

fn flag(record: &Value, name: &str) -> bool {
    record.get(name).and_then(Value::as_bool) == Some(true)
}

/// Whether a session record is a user turn: a prompt sent to the session
/// itself, not a sub-agent's (`isSidechain`), not one Claude Code added
/// (`isMeta`, a compaction summary), and not tool results (which Claude
/// Code records as user records too).
fn is_prompt(record: &Value) -> bool {
    kind(record) == "user"
        && !flag(record, "isSidechain")
        && !flag(record, "isMeta")
        && !flag(record, "isCompactSummary")
        && match record.pointer("/message/content") {
            Some(Value::String(_)) => true,
            Some(Value::Array(blocks)) => !blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")),
            _ => false,
        }
}

/// A Claude Code session with a user turn that carries the nonce: its
/// records, where that turn starts and ends in them, and when it was sent.
/// Only what happened in that turn is the probe's: work done before the
/// marked prompt (RC2-OPSG4) or after the next user turn (RC2-OPSH3) never
/// counts for it.
struct Marked {
    file: PathBuf,
    records: Vec<Value>,
    turn: usize,
    /// The next user turn (or the end of the records).
    end: usize,
    at: Option<DateTime<FixedOffset>>,
}

impl Marked {
    /// The session file's records in the marked turn, after its prompt.
    fn window(&self) -> &[Value] {
        &self.records[self.turn + 1..self.end]
    }

    /// Whether something that started at `start` started in the marked turn:
    /// at or after its prompt, and before the next user turn. Without the
    /// times to tell, it did not.
    fn started_in_turn(&self, start: DateTime<FixedOffset>) -> bool {
        self.at.is_some_and(|at| start >= at) && self.before_next_turn(Some(start))
    }

    /// Whether something dated `at` came before the next user turn (always,
    /// when there is no next turn). Without the times to tell, it did not
    /// (RC2-OPSI1): a reply after the next prompt is not the probe's.
    fn before_next_turn(&self, at: Option<DateTime<FixedOffset>>) -> bool {
        match self.records.get(self.end) {
            None => true,
            Some(next) => at.zip(timestamp(next)).is_some_and(|(at, next)| at < next),
        }
    }
}

/// Claude Code sessions with a user turn that carries the nonce.
fn claude_sessions(roots: &RecordRoots, nonce: &str) -> Vec<Marked> {
    let projects = roots.claude_home.join("projects");
    let mut sessions = vec![];
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&projects)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        for file in files_in(&dir, |n| n.ends_with(".jsonl")) {
            let records = records(&file);
            let turn = records.iter().position(|r| {
                is_prompt(r)
                    && r.pointer("/message/content")
                        .is_some_and(|c| content_text(c).contains(nonce))
            });
            if let Some(turn) = turn {
                let end = records[turn + 1..]
                    .iter()
                    .position(is_prompt)
                    .map_or(records.len(), |i| turn + 1 + i);
                let at = timestamp(&records[turn]);
                sessions.push(Marked {
                    file,
                    records,
                    turn,
                    end,
                    at,
                });
            }
        }
    }
    sessions
}

fn no_claude_session(roots: &RecordRoots) -> String {
    format!(
        "no Claude Code session record carries this run's marker (looked in {})",
        roots.claude_home.join("projects").display()
    )
}

/// One sub-agent's run: when it started and ended, and its last reply.
struct Agent {
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    reply: String,
}

fn agent(records: &[&Value]) -> Option<Agent> {
    let times: Vec<DateTime<FixedOffset>> = records
        .iter()
        .filter_map(|r| r.get("timestamp").and_then(Value::as_str))
        .filter_map(|t| DateTime::parse_from_rfc3339(t).ok())
        .collect();
    let reply = records
        .iter()
        .rev()
        .find(|r| kind(r) == "assistant")
        .and_then(|r| r.pointer("/message/content"))
        .map(content_text)
        .unwrap_or_default();
    Some(Agent {
        start: *times.iter().min()?,
        end: *times.iter().max()?,
        reply,
    })
}

/// The sub-agents of one session that ran in the marked turn: their own
/// transcripts (started in that turn, by time, and read only up to the next
/// user turn, so a reply after it does not count), or sidechain records in
/// that turn in the session file, grouped by agent.
fn session_agents(session: &Marked) -> Vec<Agent> {
    let mut agents = vec![];
    let dir = session.file.with_extension("").join("subagents");
    for file in files_in(&dir, |n| n.starts_with("agent-") && n.ends_with(".jsonl")) {
        let all = records(&file);
        let in_turn: Vec<&Value> = all
            .iter()
            .filter(|r| session.before_next_turn(timestamp(r)))
            .collect();
        agents.extend(agent(&in_turn).filter(|a| session.started_in_turn(a.start)));
    }
    let main = session.window();
    let mut ids: Vec<&str> = main
        .iter()
        .filter(|r| r.get("isSidechain").and_then(Value::as_bool) == Some(true))
        .filter_map(|r| r.get("agentId").and_then(Value::as_str))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let own: Vec<&Value> = main
            .iter()
            .filter(|r| r.get("agentId").and_then(Value::as_str) == Some(id))
            .collect();
        agents.extend(agent(&own));
    }
    agents
}

fn subagents(roots: &RecordRoots, nonce: &str, count: u32, reply: &str) -> Result<(), String> {
    let sessions = claude_sessions(roots, nonce);
    if sessions.is_empty() {
        return Err(no_claude_session(roots));
    }
    let wanted = Expect::ContainsAll(vec![reply.to_owned()]);
    let mut best = (0usize, 0usize);
    for session in &sessions {
        let agents = session_agents(session);
        let replied: Vec<&Agent> = agents
            .iter()
            .filter(|a| evaluate(&wanted, &a.reply) == Outcome::Pass)
            .collect();
        // The most of them running at one moment (a start inside the others).
        let together = replied
            .iter()
            .map(|a| {
                replied
                    .iter()
                    .filter(|b| b.start <= a.start && a.start <= b.end)
                    .count()
            })
            .max()
            .unwrap_or(0);
        if together >= count as usize {
            return Ok(());
        }
        best = best.max((together, agents.len()));
    }
    Err(format!(
        "the session's records show {} sub-agent(s) in this run's turn, at most {} of them replying `{reply}` at the same time; {count} are needed",
        best.1, best.0
    ))
}

fn codex_text(record: &Value, role: &str) -> Option<String> {
    let payload = record.get("payload")?;
    (kind(record) == "response_item"
        && payload.get("type").and_then(Value::as_str) == Some("message")
        && payload.get("role").and_then(Value::as_str) == Some(role))
    .then(|| payload.get("content").map(content_text).unwrap_or_default())
}

fn rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
        if path.is_dir() {
            rollouts(&path, out);
        } else if path.file_name().is_some_and(|n| {
            n.to_string_lossy().starts_with("rollout-") && n.to_string_lossy().ends_with(".jsonl")
        }) {
            out.push(path);
        }
    }
}

/// Whether a Codex record starts a new turn (Codex's own `task_started`).
fn codex_turn_start(record: &Value) -> bool {
    kind(record) == "event_msg"
        && record.pointer("/payload/type").and_then(Value::as_str) == Some("task_started")
}

/// The reply Codex gave in the turn whose prompt carries the nonce
/// (RC2-OPSH3): its last assistant message after that prompt and before the
/// next turn starts (Codex's `task_started`; in records without those, the
/// next user message). `None` when the marker is in no user message;
/// `Some(None)` when that turn has no reply.
fn codex_reply<'a>(all: &'a [Value], nonce: &str) -> Option<Option<&'a Value>> {
    let start = all
        .iter()
        .position(|r| codex_text(r, "user").is_some_and(|t| t.contains(nonce)))?;
    let has_turns = all.iter().any(codex_turn_start);
    let end = all[start + 1..]
        .iter()
        .position(|r| {
            if has_turns {
                codex_turn_start(r)
            } else {
                codex_text(r, "user").is_some()
            }
        })
        .map_or(all.len(), |i| start + 1 + i);
    Some(
        all[start + 1..end]
            .iter()
            .rev()
            .find(|r| codex_text(r, "assistant").is_some()),
    )
}

fn codex_run(roots: &RecordRoots, nonce: &str, last_line: &str) -> Result<(), String> {
    let mut files = vec![];
    rollouts(&roots.codex_home.join("sessions"), &mut files);
    files.sort();
    // The Claude Code turn that ran Codex, when its session is there: a
    // reply dated after that turn's next user prompt is not this run's
    // (RC2-OPSI1).
    let turns = claude_sessions(roots, nonce);
    let mut seen = None;
    for file in files {
        let all = records(&file);
        let Some(reply) = codex_reply(&all, nonce) else {
            continue;
        };
        let last = reply
            .and_then(|r| codex_text(r, "assistant"))
            .and_then(|t| {
                t.lines()
                    .map(str::trim)
                    .rfind(|l| !l.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let in_turn = turns.is_empty()
            || reply.is_some_and(|r| turns.iter().any(|s| s.before_next_turn(timestamp(r))));
        if last.is_empty() {
            seen = Some("the Codex session has no reply to this run's prompt".to_owned());
        } else if !in_turn {
            seen = Some(
                "the Codex reply to this run's prompt came after the next user prompt, so it is not this run's"
                    .to_owned(),
            );
        } else if last == last_line {
            return Ok(());
        } else {
            seen = Some(format!(
                "the Codex reply to this run's prompt: its last line is `{last}`, not `{last_line}`"
            ));
        }
    }
    Err(seen.unwrap_or_else(|| {
        format!(
            "no Codex session record carries this run's marker (looked in {})",
            roots.codex_home.join("sessions").display()
        )
    }))
}

/// How many hits a tool result holds: the archive-search tool answers with
/// a JSON list of hits. An error result, an empty list, or anything that is
/// not a list holds none.
fn hits(result: &Value) -> usize {
    if flag(result, "is_error") {
        return 0;
    }
    let text = result.get("content").map(content_text).unwrap_or_default();
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(|list| list.as_array().map(Vec::len))
        .unwrap_or(0)
}

/// The results of `calls` in this run's turn, in the order they came back
/// (RC2-OPSG4, RC2-OPSH3: calls and results in that turn only). `Err` when
/// no session carries the marker, no such call was made, or none came back.
fn call_results(roots: &RecordRoots, nonce: &str, calls: &[String]) -> Result<Vec<Value>, String> {
    let sessions = claude_sessions(roots, nonce);
    if sessions.is_empty() {
        return Err(no_claude_session(roots));
    }
    let mut called = false;
    let mut results = vec![];
    for session in &sessions {
        let blocks = |role: &'static str| {
            session
                .window()
                .iter()
                .filter(move |r| kind(r) == role)
                .filter_map(|r| r.pointer("/message/content").and_then(Value::as_array))
                .flatten()
        };
        let ids: Vec<&str> = blocks("assistant")
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter(|b| is_call(b, calls))
            .filter_map(|b| b.get("id").and_then(Value::as_str))
            .collect();
        called |= !ids.is_empty();
        results.extend(
            blocks("user")
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .filter(|b| {
                    b.get("tool_use_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| ids.contains(&id))
                })
                .cloned(),
        );
    }
    if !called {
        Err(format!(
            "this run's turn made no call to {}",
            calls.join(" or ")
        ))
    } else if results.is_empty() {
        Err("no result came back from those calls in this run's turn".to_owned())
    } else {
        Ok(results)
    }
}

/// The probe measures that the model can use the tool (RC2-OPSJ2): one of
/// `calls` was made in this run's turn, and its result in that turn holds
/// at least one hit. The answer text is not evidence, so it is not read.
fn tool_hits(roots: &RecordRoots, nonce: &str, calls: &[String]) -> Result<(), String> {
    let results = call_results(roots, nonce, calls)?;
    if results.iter().any(|r| hits(r) > 0) {
        Ok(())
    } else {
        Err("the search came back empty (no hits, or an error)".to_owned())
    }
}

/// [`tool_hits`], and the count the answer reports is the number of hits
/// the last of those calls returned (review of 59de542f): once limits are
/// set aside ("limit 5", "limit of 5 results", "the limit argument was set
/// to 5"; review of 5e16ca42), the answer holds that count as a standalone
/// number, and every count it claims ("50 results", "hits: 3", "returned
/// 3") is that number.
fn reported_hits(
    roots: &RecordRoots,
    nonce: &str,
    calls: &[String],
    answer: &str,
) -> Result<(), String> {
    let results = call_results(roots, nonce, calls)?;
    let count = results.last().map(hits).unwrap_or(0);
    if count == 0 {
        return Err(
            "the last search in this run's turn came back empty (no hits, or an error)".to_owned(),
        );
    }
    let text = without_limits(&answer_lines(answer).join("\n"));
    if !text
        .split_whitespace()
        .filter_map(standalone_number)
        .any(|n| n == count)
    {
        return Err(format!(
            "the search returned {count} hit(s); the answer does not give {count} as a number (a limit does not count)"
        ));
    }
    if let Some(other) = claimed_counts(&text).into_iter().find(|n| *n != count) {
        return Err(format!(
            "the answer reports {other} results; the search returned {count}"
        ));
    }
    Ok(())
}

/// A whitespace-separated word that is a number once the punctuation and
/// markdown around it are stripped (`**5**`, `(1)`, `1.`); a date, a
/// version or a fraction is not one.
fn standalone_number(word: &str) -> Option<usize> {
    let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
    (!bare.is_empty() && bare.chars().all(|c| c.is_ascii_digit()))
        .then(|| bare.parse().ok())
        .flatten()
}

/// `text` with every limit's number removed: a number after "limit", "max",
/// "top", "up to" or "at most", with the words that join them ("of",
/// "argument", "was set to", a colon). What remains is what the answer
/// says came back.
fn without_limits(text: &str) -> String {
    static LIMIT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    LIMIT
        .get_or_init(|| {
            regex::Regex::new(
                r"(?i)\b(?:limit|max(?:imum)?|top|up\s+to|at\s+most)\b(?:\s+(?:argument|parameter|of|was|is|set|to)\b)*[\s:=*_`]*\d+",
            )
            .expect("static regex")
        })
        .replace_all(text, " (limit) ")
        .into_owned()
}

/// The counts an answer claims came back: "50 results", "1 matching
/// result", "hits: 3", "returned 3". Limits are removed first
/// ([`without_limits`]).
fn claimed_counts(text: &str) -> Vec<usize> {
    static CLAIMS: std::sync::OnceLock<[regex::Regex; 3]> = std::sync::OnceLock::new();
    let claims = CLAIMS.get_or_init(|| {
        [
            r"(?i)\b(\d+)[*_`]*\s+(?:[a-z-]+\s+)?(?:results?|hits?|match(?:es)?|entries)\b",
            r"(?i)\b(?:results?|hits?|match(?:es)?)[*_`]*\s*[:=][\s*_`]*(\d+)\b",
            r"(?i)\b(?:returned|found|got|gave|yielded)\s+(?:only\s+|exactly\s+|just\s+)?[*_`]*(\d+)\b",
        ]
        .map(|p| regex::Regex::new(p).expect("static regex"))
    });
    claims
        .iter()
        .flat_map(|re| re.captures_iter(text))
        .filter_map(|c| c[1].parse::<usize>().ok())
        .collect()
}

/// Whether an answer line is Claude Code's own notice, which it prints
/// after the answer when a workspace is not trusted ("Ignoring 9
/// permissions.allow entries from .claude/settings.json: ...").
fn cli_notice(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("Ignoring ") && line.contains("permissions.allow entries")
}

/// The answer's lines, without the trailing lines that are not the answer:
/// blank lines, lines with no letters or digits (a closing code fence), and
/// Claude Code's trailing notice ([`cli_notice`]).
fn answer_lines(answer: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = answer.lines().collect();
    while lines
        .last()
        .is_some_and(|l| bare(l).is_empty() || cli_notice(l))
    {
        lines.pop();
    }
    lines
}

/// A line without the markdown or punctuation around it (`**`, a code
/// span's backticks, a list dash, a quote mark, a closing full stop).
fn bare(line: &str) -> &str {
    line.trim_matches(|c: char| !c.is_alphanumeric())
}

/// The last non-empty line of a reply, trimmed.
fn last_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// A Codex review of this run, and the answer reporting its verdict
/// (review of 59de542f). The review is the latest Codex session whose
/// prompt carries the marker, replied in this run's turn (RC2-OPSI1), and
/// whose last non-empty line is exactly one of `verdicts`. The answer's
/// only verdict line (a line starting `FINAL VERDICT`, any case) is that
/// line, and it is the answer's last line ([`answer_lines`]).
fn codex_verdict(
    roots: &RecordRoots,
    nonce: &str,
    verdicts: &[String],
    answer: &str,
) -> Result<(), String> {
    let mut files = vec![];
    rollouts(&roots.codex_home.join("sessions"), &mut files);
    files.sort();
    let turns = claude_sessions(roots, nonce);
    let mut review: Option<(Option<DateTime<FixedOffset>>, String)> = None;
    let mut seen = None;
    for file in files {
        let all = records(&file);
        let Some(reply) = codex_reply(&all, nonce) else {
            continue;
        };
        let last = reply
            .and_then(|r| codex_text(r, "assistant"))
            .map(|t| last_line(&t))
            .unwrap_or_default();
        let in_turn = turns.is_empty()
            || reply.is_some_and(|r| turns.iter().any(|s| s.before_next_turn(timestamp(r))));
        if last.is_empty() {
            seen = Some("the Codex session has no reply to this run's prompt".to_owned());
        } else if !in_turn {
            seen = Some(
                "the Codex reply to this run's prompt came after the next user prompt, so it is not this run's"
                    .to_owned(),
            );
        } else if verdicts.contains(&last) {
            let at = reply.and_then(timestamp);
            if review.as_ref().is_none_or(|(latest, _)| at >= *latest) {
                review = Some((at, last));
            }
        } else {
            seen = Some(format!(
                "the Codex reply to this run's prompt ends with `{last}`, which is not one of the allowed verdicts"
            ));
        }
    }
    let Some((_, verdict)) = review else {
        return Err(seen.unwrap_or_else(|| {
            format!(
                "no Codex session record carries this run's marker (looked in {})",
                roots.codex_home.join("sessions").display()
            )
        }));
    };
    let lines = answer_lines(answer);
    let reported = lines
        .iter()
        .filter(|l| bare(l).to_uppercase().starts_with("FINAL VERDICT"))
        .count();
    if reported != 1 {
        return Err(format!(
            "the answer has {reported} verdict lines; it must give Codex's `{verdict}` once, as its last line"
        ));
    }
    let last = lines.last().map(|l| bare(l)).unwrap_or_default();
    if last != verdict {
        return Err(format!(
            "the answer's last line is `{last}`, not Codex's verdict `{verdict}`"
        ));
    }
    Ok(())
}

/// Whether a `tool_use` block is one of `calls`: the tool's own name in the
/// record is one of them, exactly (RC2-OPSH1). A shell call never counts,
/// whatever its command line: what a shell prints can be made up, while the
/// archive-search tool's result is what the archive returned.
fn is_call(block: &Value, calls: &[String]) -> bool {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
    calls.iter().any(|call| call == name)
}
