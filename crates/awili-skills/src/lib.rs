//! `awili-skills` — the executor-neutral fleet skills pack (daily-driver P1).
//!
//! A skills pack is a directory of agentskills-compatible skills:
//! `<dir>/<name>/SKILL.md`, each a YAML front matter block
//! (`name`, `description`, `when_to_use`, `tools_needed`) over a markdown
//! body of at most [`MAX_BODY_LINES`] lines. [`SkillPack::load`] reads one,
//! [`SkillPack::select`] matches a prompt to skills by whole-token matching
//! (no LLM, no substrings — see [`select`]), and
//! [`SkillPack::render_preamble`] renders the selected skills as a bounded
//! text block (at most [`MAX_PREAMBLE_BYTES`]) of COMPLETE recipes for
//! injection ahead of the executor's prompt — never a silently truncated
//! instruction.
//!
//! Rendering logs only effectively injected names and the omitted count
//! with `tracing::info`. Selection and the user's prompt text are never logged.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use thiserror::Error;

pub mod render;
use render::{omission_summary, section_for, HEADER, SUMMARY_BUDGET};
mod select;

/// The maximum body length for one skill file, in lines. A skill whose body
/// exceeds this fails load (naming the file): an over-long recipe cannot
/// keep the injected preamble inside its bound.
pub const MAX_BODY_LINES: usize = 120;

/// The hard bound for a rendered preamble block, in bytes (6 KiB).
pub const MAX_PREAMBLE_BYTES: usize = 6 * 1024;

/// One fleet skill loaded from `<dir>/<name>/SKILL.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// The skill name; must equal the directory name holding `SKILL.md`.
    pub name: String,
    /// What the skill is (front matter `description`).
    pub description: String,
    /// When the skill should be selected (front matter `when_to_use`).
    pub when_to_use: String,
    /// The recipe body (markdown, front matter removed).
    pub body: String,
    /// The `SKILL.md` file this skill was loaded from.
    pub path: PathBuf,
}

/// A loaded fleet skills pack: every skill in one directory, in
/// directory-name order (deterministic across platforms).
#[derive(Debug)]
pub struct SkillPack {
    /// The directory the pack was loaded from.
    dir: PathBuf,
    /// The skills, sorted by directory name.
    skills: Vec<Skill>,
}

/// Why a skills pack fails to load. Every variant names the offending file
/// (or directory) so the fix is visible in the log.
#[derive(Debug, Error)]
pub enum SkillPackError {
    /// The pack directory does not exist or is not a directory.
    #[error("skills directory {0} does not exist or is not a directory")]
    DirNotAPath(PathBuf),

    /// A skill file (or the pack directory) could not be read.
    #[error("reading {file}: {source}")]
    Io {
        /// The file that failed to read.
        file: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The file does not start with a `---` front matter block, the block is
    /// not closed, or a line in it is not `key: value`.
    #[error(
        "skill {file}: missing or malformed front matter \
         (expected a leading `---` block of `key: value` lines, closed by `---`)"
    )]
    FrontMatter {
        /// The offending `SKILL.md`.
        file: PathBuf,
    },

    /// A required front matter field is absent or empty.
    #[error("skill {file}: front matter is missing required field '{field}'")]
    MissingField {
        /// The offending `SKILL.md`.
        file: PathBuf,
        /// The field name (`name`, `description`, `when_to_use`, `tools_needed`).
        field: &'static str,
    },

    /// The body is longer than [`MAX_BODY_LINES`] lines.
    #[error("skill {file}: body is {lines} lines; the limit is {MAX_BODY_LINES}")]
    BodyTooLong {
        /// The offending `SKILL.md`.
        file: PathBuf,
        /// The body's line count.
        lines: usize,
    },

    /// The front matter `name` differs from the directory that holds the file.
    #[error("skill {file}: name '{name}' must equal its directory name '{dir}'")]
    NameMismatch {
        /// The offending `SKILL.md`.
        file: PathBuf,
        /// The front matter `name`.
        name: String,
        /// The directory name the file lives in.
        dir: String,
    },
}

impl SkillPack {
    /// Load every `<dir>/<name>/SKILL.md` into a pack, in directory-name
    /// order. Directories without a `SKILL.md` and non-directory entries
    /// are ignored (a pack may sit beside other files); a malformed skill
    /// file fails the whole load, naming the file.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, SkillPackError> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Err(SkillPackError::DirNotAPath(dir.to_path_buf()));
        }
        let entries = std::fs::read_dir(dir).map_err(|e| SkillPackError::Io {
            file: dir.to_path_buf(),
            source: e,
        })?;
        let mut skill_dirs: Vec<PathBuf> = entries
            .filter_map(|e| match e {
                Ok(e) if e.file_type().is_ok_and(|t| t.is_dir()) => Some(e.path()),
                _ => None,
            })
            .collect();
        skill_dirs.sort();

        let mut skills = Vec::new();
        for skill_dir in skill_dirs {
            let file = skill_dir.join("SKILL.md");
            if !file.is_file() {
                continue;
            }
            skills.push(load_one(
                skill_dir.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                &file,
            )?);
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            skills,
        })
    }

    /// The skills in load order.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// The directory this pack was loaded from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Select the skills a prompt matches (no LLM, no substrings):
    /// whole-alphanumeric tokens, lowercased and deduplicated, with English
    /// stopwords and short fragments dropped (the tool identifiers `ssh`,
    /// `dsh`, `git` are retained). A query token that is a token of a skill's
    /// `name` is an explicit match (one suffices); otherwise the skill's
    /// metadata (`description` + `when_to_use`) must contain at least two
    /// distinct meaningful query tokens. Deterministic order: name hits
    /// desc, metadata hits desc, load order.
    ///
    /// Selection is silent; rendering logs only effectively injected names.
    pub fn select(&self, query: &str) -> Vec<&Skill> {
        let selected = select::selected_indices(&self.skills, query);
        selected.into_iter().map(|(i, _)| &self.skills[i]).collect()
    }

    /// Render the selected skills as the injection block: at most
    /// [`MAX_PREAMBLE_BYTES`] (6 KiB), and always COMPLETE recipes — a
    /// partially rendered instruction would silently corrupt the run, so when
    /// the bound is reached the remaining skills are dropped and an explicit
    /// bounded omission summary names them (all names when they fit in the
    /// reserved summary budget; otherwise the count plus a bounded names
    /// list). An empty selection renders as the empty string (nothing to
    /// inject).
    ///
    /// The block describes the recipes as CONFIGURED guidance subordinate to
    /// the user's current instructions and live evidence — never as verified
    /// facts. The EFFECTIVELY injected names (not merely the selected ones)
    /// are logged with `tracing::info`.
    pub fn render_preamble(&self, selected: &[&Skill]) -> String {
        if selected.is_empty() {
            return String::new();
        }
        let mut out = String::from(HEADER);
        let limit = MAX_PREAMBLE_BYTES - SUMMARY_BUDGET;
        let mut injected: Vec<&str> = Vec::new();
        let mut omitted: Vec<&str> = Vec::new();
        for skill in selected {
            let section = section_for(skill);
            if out.len() + section.len() <= limit {
                out.push_str(&section);
                injected.push(skill.name.as_str());
            } else {
                omitted.push(skill.name.as_str());
            }
        }
        if !omitted.is_empty() {
            out.push_str(&omission_summary(&omitted));
        }
        tracing::info!(
            injected = injected.join(", ").as_str(),
            omitted = omitted.len(),
            bytes = out.len(),
            "fleet skills: rendered preamble"
        );
        out
    }
}

/// The front matter fields required of every `SKILL.md`, in the order
/// [`load_one`] checks them.
const REQUIRED_FIELDS: [&str; 4] = ["name", "description", "when_to_use", "tools_needed"];

/// Parse and validate one `SKILL.md`.
fn load_one(dir_name: &str, file: &Path) -> Result<Skill, SkillPackError> {
    let text = std::fs::read_to_string(file).map_err(|e| SkillPackError::Io {
        file: file.to_path_buf(),
        source: e,
    })?;
    let (fields, body) = split_front_matter(file, &text)?;
    let mut values: Vec<(String, String)> = Vec::new();
    for key in REQUIRED_FIELDS {
        values.push((
            key.to_string(),
            require_field(file, &fields, key)?.to_string(),
        ));
    }
    let name = values[0].1.clone();
    let description = values[1].1.clone();
    let when_to_use = values[2].1.clone();

    if name != dir_name {
        return Err(SkillPackError::NameMismatch {
            file: file.to_path_buf(),
            name,
            dir: dir_name.to_string(),
        });
    }
    let lines = body.lines().count();
    if lines > MAX_BODY_LINES {
        return Err(SkillPackError::BodyTooLong {
            file: file.to_path_buf(),
            lines,
        });
    }
    Ok(Skill {
        name,
        description,
        when_to_use,
        body,
        path: file.to_path_buf(),
    })
}

/// Split `SKILL.md` text into (front matter fields, body).
///
/// The file must begin with a `---` line. The front matter is the run of
/// `key: value` lines up to the next `---` line: blank lines and `#`
/// comments are skipped, and unknown keys are tolerated (agentskills
/// metadata may grow). The body is everything after the closing `---`.
fn split_front_matter(
    file: &Path,
    text: &str,
) -> Result<(Vec<(String, String)>, String), SkillPackError> {
    let lines: Vec<&str> = text.lines().collect();
    match lines.first() {
        Some(l) if l.trim() == "---" => {}
        _ => {
            return Err(SkillPackError::FrontMatter {
                file: file.to_path_buf(),
            })
        }
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut closing: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        let t = line.trim();
        if t == "---" {
            closing = Some(i);
            break;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let (k, v) = t.split_once(':').ok_or(SkillPackError::FrontMatter {
            file: file.to_path_buf(),
        })?;
        fields.push((k.trim().to_string(), v.trim().to_string()));
    }
    let Some(closing) = closing else {
        return Err(SkillPackError::FrontMatter {
            file: file.to_path_buf(),
        });
    };
    // Body starts after the closing `---` line.
    let body = lines[closing + 1..].join("\n");
    Ok((fields, body))
}

/// The value of a required front matter field; absent or empty is an error.
fn require_field<'a>(
    file: &Path,
    fields: &'a [(String, String)],
    key: &'static str,
) -> Result<&'a str, SkillPackError> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or(SkillPackError::MissingField {
            file: file.to_path_buf(),
            field: key,
        })
}
