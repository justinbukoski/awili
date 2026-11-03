//! Non-Unix stand-in for the filesystem sandbox. Every operation refuses.
//!
//! The real mechanism needs descriptor-relative no-follow resolution, which
//! this platform layer does not have yet, so there is deliberately NO
//! best-effort fallback: a pathname-based "sandbox" is exactly the design the
//! C3 rewrite replaced. TODO(windows-phase1).

use std::io;
use std::path::Path;

use awili_engine_log::authz::AuthzEvidence;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// RC2-ENG6 (T19): the authorization handed to the sandbox does not
    /// cover this call (or names no folder) — refused, but not a path
    /// escape.
    #[error("not authorized: {0}")]
    NotAuthorized(String),
    #[error("path escapes the session root: {0}")]
    Escape(String),
    #[error("symlink encountered at {0} (no-follow resolution)")]
    Symlink(String),
    #[error("multiply-linked file at {0} (hardlink containment)")]
    Hardlink(String),
    #[error("not a regular file: {0}")]
    NotRegularFile(String),
    #[error("invalid path component in {0}")]
    InvalidComponent(String),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// A bounded operation exceeded its declared limit (C2-u4). NOT a
    /// containment refusal — but still a failed dispatch: over-limit
    /// results are refused whole, never truncated (a truncated read is
    /// a lie the model would act on).
    #[error("limit exceeded: {0}")]
    TooLarge(String),
}

impl SandboxError {
    /// Containment-class refusals (C2-u4 design finding 5): errors that
    /// mean the sandbox REFUSED something for a security reason, as
    /// opposed to ordinary tree conditions (unreadable file, fifo) or
    /// resource limits. During tree-walking operations (grep), a
    /// containment-class refusal fails the WHOLE dispatch; ordinary
    /// conditions are skipped-and-reported.
    pub fn is_containment(&self) -> bool {
        matches!(
            self,
            SandboxError::Escape(_)
                | SandboxError::NotAuthorized(_)
                | SandboxError::Symlink(_)
                | SandboxError::Hardlink(_)
                | SandboxError::InvalidComponent(_)
        )
    }
}

/// Resource limits for the builtin-facing bounded operations (C2-u4
/// design finding 4). Enforced WHILE reading/walking — before unbounded
/// allocation — never by truncating an already-built result.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Max bytes read from (or written to) a single file.
    pub max_file_bytes: usize,
    /// Max directory entries visited in one walk (glob/grep).
    pub max_entries: usize,
    /// Max grep matches accumulated before the search is refused whole.
    pub max_matches: usize,
    /// Max total result bytes a bounded operation may ACCUMULATE (glob
    /// path bytes, grep matched-line bytes). Enforced before every
    /// append — a result is refused whole, never truncated.
    pub max_result_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1 << 20, // 1 MiB
            max_entries: 10_000,
            max_matches: 2_000,
            max_result_bytes: 1 << 20, // 1 MiB
        }
    }
}

/// Never constructed on this platform: `open_root` always errors.
pub struct Sandbox {
    _never: std::convert::Infallible,
}

fn unavailable() -> SandboxError {
    SandboxError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "the filesystem sandbox is not available on this platform yet (not supported on Windows)",
    ))
}

/// One entry from a sandboxed directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Path relative to the sandbox root, as the caller would pass it back.
    pub rel: String,
    pub is_dir: bool,
}

/// Search result: hits PLUS every entry that could not be read and why —
/// so a containment refusal is visible rather than silently absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepReport {
    pub matches: Vec<Match>,
    /// (relative path, reason) for entries skipped during the search.
    pub skipped: Vec<(String, String)>,
}

/// A grep hit: 1-based line number and the matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub rel: String,
    pub line_no: usize,
    pub line: String,
}

impl Sandbox {
    pub fn open_root(_root: &Path) -> Result<Self, SandboxError> {
        Err(unavailable())
    }
    pub fn root_display(&self) -> &str {
        match self._never {}
    }
    pub fn read(&self, _rel: &str) -> Result<Vec<u8>, SandboxError> {
        match self._never {}
    }
    pub fn read_bounded(&self, _rel: &str, _limits: &Limits) -> Result<Vec<u8>, SandboxError> {
        match self._never {}
    }
    pub fn write_bounded(
        &self,
        _rel: &str,
        _contents: &[u8],
        _limits: &Limits,
    ) -> Result<(), SandboxError> {
        match self._never {}
    }
    pub fn write(&self, _rel: &str, _contents: &[u8]) -> Result<(), SandboxError> {
        match self._never {}
    }
    pub fn exists(&self, _rel: &str) -> Result<bool, SandboxError> {
        match self._never {}
    }
    pub fn edit(&self, _rel: &str, _find: &[u8], _replace: &[u8]) -> Result<usize, SandboxError> {
        match self._never {}
    }
    pub fn edit_bounded(
        &self,
        _rel: &str,
        _find: &[u8],
        _replace: &[u8],
        _limits: &Limits,
    ) -> Result<usize, SandboxError> {
        match self._never {}
    }
    pub fn list_dir(&self, _rel: &str) -> Result<Vec<DirEntry>, SandboxError> {
        match self._never {}
    }
    pub fn glob(&self, _pattern: &str) -> Result<Vec<String>, SandboxError> {
        match self._never {}
    }
    pub fn glob_bounded(
        &self,
        _pattern: &str,
        _limits: &Limits,
    ) -> Result<Vec<String>, SandboxError> {
        match self._never {}
    }
    pub fn grep(&self, _needle: &str, _under: &str) -> Result<GrepReport, SandboxError> {
        match self._never {}
    }
    pub fn grep_bounded(
        &self,
        _needle: &str,
        _under: &str,
        _limits: &Limits,
    ) -> Result<GrepReport, SandboxError> {
        match self._never {}
    }
    pub fn files_bounded(
        &self,
        _under: &str,
        _limits: &Limits,
    ) -> Result<Vec<String>, SandboxError> {
        match self._never {}
    }
    pub fn grep_bounded_where(
        &self,
        _needle: &str,
        _under: &str,
        _limits: &Limits,
        _keep: &dyn Fn(&str) -> bool,
    ) -> Result<GrepReport, SandboxError> {
        match self._never {}
    }
}

/// Same authorization handshake as the Unix build, then the unavailable error.
pub fn sandbox_for_evidence(
    evidence: &AuthzEvidence,
    turn_id: &str,
    call: &awili_engine::events::ToolCall,
) -> Result<Sandbox, SandboxError> {
    if !evidence.authorizes(turn_id, call) {
        return Err(SandboxError::NotAuthorized(
            "authorization does not cover this call".into(),
        ));
    }
    let Some(root) = evidence.scope_root() else {
        return Err(SandboxError::NotAuthorized(
            "authorization carries no scope root".into(),
        ));
    };
    Sandbox::open_root(Path::new(root))
}
