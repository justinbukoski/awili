//! Probe manifest: the versioned data model loaded from TOML (P0a).
//!
//! A [`Manifest`] is a list of [`Probe`]s; each probe is one prompt plus an
//! [`Expect`] describing how the final assistant text is judged. The field
//! names match `tests/daily-driver/probes.toml` exactly.

use serde::{Deserialize, Serialize};

/// Capability class a probe exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    /// Long-term memory read/write/recall.
    Memory,
    /// Outline wiki documents.
    Outline,
    /// Gitea repo + API access.
    Gitea,
    /// SSH to fleet hosts.
    Ssh,
    /// Web page fetch.
    Web,
    /// Headed browser interaction.
    Browser,
    /// Code-review dispatch.
    Review,
    /// Subagent fan-out.
    Subagent,
    /// File authoring (HTML, JS, 3mf, ...).
    Author,
    /// Deploy + ops health.
    Deploy,
    /// Hygiene checks (punchlist, doc drift).
    Hygiene,
    /// User-facing UX (image input, ...).
    UserFacing,
}

/// How a probe's final assistant text is judged.
///
/// All variants are evaluated against the final assistant text and are
/// case-insensitive by default (P0-weekend-repair.md item 4); a `Regex`
/// pattern can opt out per match with an explicit `(?-i:...)` group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expect {
    /// Every string must be present (case-insensitive) as a whole word, and a
    /// string listed N times must be present N times.
    ContainsAll(Vec<String>),
    /// One regex must match (`regex` crate syntax; case-insensitive by
    /// default, explicit `(?-i:...)` can opt out).
    Regex(String),
    /// Pass when at least one nested expectation passes; a triggered
    /// [`Expect::Sentinel`] makes the whole branch fail as declared-missing.
    AnyOf(Vec<Expect>),
    /// Pass only when every nested expectation passes; the first that does
    /// not decides the outcome.
    AllOf(Vec<Expect>),
    /// The answer must NOT match this regex (same syntax and default
    /// case-insensitivity as [`Expect::Regex`]): a real error printed after
    /// "no errors", or a second verdict after the first, fails the probe.
    Forbid(String),
    /// Exact token, case-insensitive. Its presence means the model declared
    /// the capability missing: `Fail { reason: "declared-missing" }`.
    Sentinel(String),
}

/// Evidence the harness reads itself once the answer text has passed
/// (RC2-OPSF4; [`crate::records`]): the records a tool left while the probe
/// ran, found by the probe's run marker (`{nonce}` in the prompt). The text
/// alone can claim a tool ran; these records show it did, and what it
/// returned, which some checks hold the answer to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordCheck {
    /// Claude Code ran at least `count` sub-agents at the same time in the
    /// session the prompt started, each replying `reply` (a whole word).
    Subagents {
        /// Sub-agents that must have run at once.
        count: u32,
        /// What each one's last reply must contain.
        reply: String,
    },
    /// A Codex session whose prompt carries the run marker ended with
    /// exactly this last line.
    CodexRun {
        /// The Codex reply's last non-empty line.
        last_line: String,
    },
    /// In the turn the prompt started, one of `calls` was made and its
    /// result there holds at least one hit (RC2-OPSJ2; named
    /// `QuotedFromTool` before). The probe measures that the model can use
    /// the tool; the answer text is not evidence.
    ToolHits {
        /// The tools that count, by their full name in the session record
        /// (RC2-OPSH1), e.g. `mcp__interlock__archive_search`. The hit must
        /// be in that call's own result; a shell call (curl and the like)
        /// never counts, since what a shell prints can be made up.
        calls: Vec<String>,
    },
    /// [`RecordCheck::ToolHits`], and the answer reports how many hits the
    /// last of those calls in this run's turn returned (review of
    /// 59de542f): the answer holds that count as a standalone number and
    /// claims no other count of results, hits or matches (a search limit
    /// is not a count). The rest of the answer is free-form.
    ReportedHits {
        /// The tools that count, as in [`RecordCheck::ToolHits`].
        calls: Vec<String>,
    },
    /// A Codex review of this run (review of 59de542f): a Codex session
    /// whose prompt carries the run marker ended with a last non-empty line
    /// that is exactly one of `verdicts`, and the answer reports that same
    /// verdict as its only verdict line (a line starting `FINAL VERDICT`,
    /// any case) and as its last line. Markdown around a line, lines with
    /// no letters or digits (a code fence), and Claude Code's trailing
    /// "Ignoring N permissions.allow entries" notice are not the answer's
    /// last line.
    CodexVerdict {
        /// The allowed verdict lines, e.g. `FINAL VERDICT: MERGE`.
        verdicts: Vec<String>,
    },
}

/// One probe: a prompt plus how its answer is judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// Stable unique id (e.g. `outline-read`).
    pub id: String,
    /// Human title (e.g. `Turn 5: outline-read`).
    pub title: String,
    /// Capability class exercised.
    pub capability: Capability,
    /// Prompt text sent to the executor.
    pub prompt: String,
    /// How the final assistant text is judged.
    pub expect: Expect,
    /// Per-probe timeout in seconds; 0 is rejected by [`Manifest::validate`].
    pub timeout_s: u32,
    /// Free-form tags (e.g. `cowork-2026-09-11`).
    pub tags: Vec<String>,
    /// Probes whose result this one builds on (e.g. the document an earlier
    /// turn created). Each must come earlier in the manifest; a run skips
    /// this probe when one of them did not pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Records the harness checks after the answer text passes (a tool
    /// that really ran). The prompt must carry `{nonce}`, the run marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<RecordCheck>,
}

/// Executor kind a result was produced by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutorKind {
    /// Hermes gateway executor (P0c).
    Gateway,
    /// `claude -p`.
    Claude,
    /// `codex exec`.
    Codex,
    /// `zcode --prompt` (GLM).
    Glm,
    /// Local scripted/command-driven run.
    Local,
}

/// An executor + model pair — one matrix column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Executor {
    /// The executor kind.
    pub kind: ExecutorKind,
    /// Model name (e.g. `fable`).
    pub model: String,
}

impl ExecutorKind {
    /// Lowercase wire name used by the CLI and report filenames.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Glm => "glm",
            Self::Local => "local",
        }
    }

    /// Parse a CLI executor value (`gateway|claude|codex|glm|local`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "gateway" => Some(Self::Gateway),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "glm" => Some(Self::Glm),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

/// A loaded probe manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version (1 in `tests/daily-driver/probes.toml`).
    pub version: u32,
    /// The probes, in run order.
    pub probes: Vec<Probe>,
}

/// The first Regex or Forbid pattern in `expect` that does not compile, with
/// the compiler's message.
fn invalid_pattern(expect: &Expect) -> Option<String> {
    match expect {
        Expect::Regex(pattern) | Expect::Forbid(pattern) => regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .err()
            .map(|e| format!("`{pattern}`: {e}")),
        Expect::AnyOf(parts) | Expect::AllOf(parts) => parts.iter().find_map(invalid_pattern),
        Expect::ContainsAll(_) | Expect::Sentinel(_) => None,
    }
}

impl Manifest {
    /// Parse a manifest from a TOML string.
    pub fn from_toml_str(toml: &str) -> Result<Self, String> {
        toml::from_str(toml).map_err(|e| format!("manifest TOML: {e}"))
    }

    /// Reject duplicate ids, empty prompts, zero timeouts, expectations the
    /// prompt itself would meet (an answer that merely repeats the question
    /// must not pass), and dependencies that are unknown or come later.
    ///
    /// Errors name the offending probe id.
    pub fn validate(&self) -> Result<(), String> {
        for (index, probe) in self.probes.iter().enumerate() {
            if probe.prompt.trim().is_empty() {
                return Err(format!("probe `{}` has an empty prompt", probe.id));
            }
            if probe.timeout_s == 0 {
                return Err(format!("probe `{}` has timeout_s = 0", probe.id));
            }
            if let Some(error) = invalid_pattern(&probe.expect) {
                return Err(format!(
                    "probe `{}` has an invalid regex: {error}",
                    probe.id
                ));
            }
            if crate::evaluate::evaluate(&probe.expect, &probe.prompt)
                == crate::evaluate::Outcome::Pass
            {
                return Err(format!(
                    "probe `{}` passes on its own prompt: an echo of the question would count as an answer",
                    probe.id
                ));
            }
            for dependency in &probe.depends_on {
                if !self.probes[..index]
                    .iter()
                    .any(|earlier| &earlier.id == dependency)
                {
                    return Err(format!(
                        "probe `{}` depends on `{dependency}`, which is not an earlier probe",
                        probe.id
                    ));
                }
            }
        }
        for (i, outer) in self.probes.iter().enumerate() {
            for inner in self.probes.iter().skip(i + 1) {
                if outer.id == inner.id {
                    return Err(format!("duplicate probe id `{}`", outer.id));
                }
            }
        }
        Ok(())
    }
}
