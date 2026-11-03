//! Risk classification (contract §3): registry-declared floor, escalated —
//! never lowered — by argument inspection. Unknown tools are `privileged`.

use awili_engine::events::ToolCall;
use awili_engine_log::authz::RiskClass;

/// Parse a C2 registry-declared class. Anything unrecognized (including
/// the legacy `none/read/write/dangerous` vocabulary) maps conservatively.
pub fn registry_floor_from_str(s: &str) -> RiskClass {
    match s {
        "read_only" | "read" | "pure" => RiskClass::ReadOnly,
        "mutating" | "write" => RiskClass::Mutating,
        "irreversible" | "dangerous" => RiskClass::Irreversible,
        _ => RiskClass::Privileged,
    }
}

/// Built-in floors for the engine's own tools until the C2 registry wires
/// in; unknown tools classify `privileged` (contract §3).
fn builtin_floor(tool: &str) -> RiskClass {
    known_builtin(tool).unwrap_or(RiskClass::Privileged)
}

/// The engine's own tools' floors; `None` for any other tool.
fn known_builtin(tool: &str) -> Option<RiskClass> {
    match tool {
        "fs.read" | "fs.glob" | "fs.grep" => Some(RiskClass::ReadOnly),
        "fs.write" | "fs.edit" | "fs.mkdir" => Some(RiskClass::Mutating),
        "fs.rm" | "shell.exec" | "net.fetch" => Some(RiskClass::Irreversible),
        _ => None,
    }
}

/// RC1-M-FIX1: a call's starting class. The engine's own tools start from
/// their built-in floor (escalated by a registry floor). Any OTHER tool the
/// registry classified — an MCP tool, by its server's own annotations,
/// `privileged` when the server gives none — starts from that class instead
/// of the "unknown tool" `privileged`, which used to swallow every MCP
/// tool's declared class: live 2026-10-03 the owner's read-only memory
/// `recall` raised an approval card in "Ask before changes", where Claude
/// Code's plan mode runs it. Nothing known: `privileged`.
pub fn base_risk(tool: &str, registry_floor: Option<RiskClass>) -> RiskClass {
    match (known_builtin(tool), registry_floor) {
        (Some(b), Some(f)) => escalate(b, f),
        (Some(b), None) => b,
        (None, Some(f)) => f,
        (None, None) => RiskClass::Privileged,
    }
}

/// Public escalation for callers combining registry floors (§3).
pub fn escalate_pub(a: RiskClass, b: RiskClass) -> RiskClass {
    escalate(a, b)
}

fn escalate(a: RiskClass, b: RiskClass) -> RiskClass {
    fn rank(r: RiskClass) -> u8 {
        match r {
            RiskClass::ReadOnly => 0,
            RiskClass::Mutating => 1,
            RiskClass::Irreversible => 2,
            RiskClass::Privileged => 3,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// Secrets deny-list patterns (contract §8): any touching call is at least
/// `privileged`, even inside the folder root.
///
/// The common dotfile secret names (`.env`, `.env.*`, `.netrc`, `.npmrc`,
/// `.pypirc`) are compared on NORMALIZED PATH COMPONENTS (S1(b), F0532/F0533):
/// a component equals the name or, for `.env`, starts with `.env.` so
/// `.env.local` matches while a harmless basename merely containing `env`
/// (`src/environment.rs`) does not. The legacy textual patterns are
/// unchanged.
pub(crate) fn touches_secrets(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let has_secret_component = p.split('/').any(|c| {
        c == ".env" || c.starts_with(".env.") || c == ".netrc" || c == ".npmrc" || c == ".pypirc"
    });
    if has_secret_component {
        return true;
    }
    p.contains("/.ssh")
        || p.starts_with(".ssh")
        || p.contains("/.gnupg")
        || p.contains("id_ed25519")
        || p.contains("id_rsa")
        || p.contains("token")
        || p.contains("secret")
        || p.contains("credential")
        || p.contains("keychain")
        || p.contains("/.aws/")
        || (p.ends_with(".conf") && (p.contains("smtp") || p.contains("anthropic")))
}

/// Extract filesystem-path argument values for screening.
///
/// This crate receives JSON arguments but not the tool descriptor's input
/// schema.  It therefore uses the argument key plus an unambiguous,
/// standalone path shape as a narrow role signal: commands, URLs and ordinary
/// payload fields are not paths. In particular, a shell command is an opaque
/// program string, not a path merely because it mentions an executable or
/// URL. Tool risk and runtime sandboxing continue to govern it.
///
/// This is deliberately not schema enforcement: schema-less ambiguous values
/// can still be classified only from their key and shape. Values selected as
/// paths are NORMALIZED before screening so encoded traversal/secret forms
/// cannot slip past the textual layer.
pub(crate) fn path_args(arguments: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return vec![];
    };
    let mut out = Vec::new();
    collect_paths(&v, None, &mut out);
    out
}

const PATH_KEY_HINTS: &[&str] = &[
    "path",
    "file",
    "dir",
    "cwd",
    "source",
    "src",
    "target",
    "dest",
    "destination",
    "from",
    "to",
    "root",
    "location",
    "output",
    "input",
];

pub(crate) fn key_is_pathy_pub(k: &str) -> bool {
    key_is_pathy(k)
}

/// RC2-ENGG2 (RC2-ENGALL slice review P2): a key hints at a path by its
/// WORDS, never by a fragment inside one (`to` used to name `topic`,
/// `token`, `tool` and `photo` paths). A word hints when it is a hint
/// (`to`, `src`, `output`), a hint's plural (`inputs`), ends in a path noun
/// (`filepath`, `workdir`, `outfile`, `webroot`; not `profile`), or is a
/// path noun's name (`filename`, `dirname`).
fn key_is_pathy(k: &str) -> bool {
    // A compound key that explicitly names a path keeps that role even when
    // another word describes its payload (`script_path`, `content_file`).
    // Conversely `targetURL` has no path noun and remains a URL payload.
    if key_words(k).iter().any(|w| word_has_path_noun(w)) {
        return true;
    }
    if key_is_non_path_payload(k) {
        return false;
    }
    key_words(k).iter().any(|w| word_hints_path(w))
}

/// Keys whose values are explicitly payload rather than filesystem paths.
/// A URL can contain slashes and `..`; it must not be reinterpreted as a
/// local path because another word in its compound key is `target` or `to`.
fn key_is_non_path_payload(k: &str) -> bool {
    key_words(k).iter().any(|w| {
        matches!(
            w.as_str(),
            "url"
                | "uri"
                | "query"
                | "search"
                | "content"
                | "prose"
                | "text"
                | "prompt"
                | "message"
                | "description"
                | "subject"
                | "instruction"
                | "instructions"
                | "command"
                | "cmd"
                | "script"
                | "code"
        )
    })
}

/// The nouns a compound key word may end in and still name a path.
const PATH_NOUNS: &[&str] = &["path", "file", "dir", "directory", "folder", "root"];

fn word_hints_path(w: &str) -> bool {
    let singular = w.strip_suffix('s').filter(|s| !s.is_empty()).unwrap_or(w);
    let hint = |x: &str| PATH_KEY_HINTS.contains(&x);
    hint(w) || hint(singular) || word_has_path_noun(w)
}

fn word_has_path_noun(w: &str) -> bool {
    let singular = w.strip_suffix('s').filter(|s| !s.is_empty()).unwrap_or(w);
    (singular != "profile" && PATH_NOUNS.iter().any(|n| singular.ends_with(n)))
        || PATH_NOUNS.iter().any(|n| {
            w.strip_prefix(n)
                .is_some_and(|r| r == "name" || r == "names")
        })
}

/// A key's words, lowercased: split at every character that is not a letter
/// or digit (`_`, `-`, `.`), at a lower-to-upper change (`toPath`), at the
/// end of a capital run (`URLPath` is `url`, `path`) and between letters and
/// digits (`file2`).
fn key_words(k: &str) -> Vec<String> {
    let chars: Vec<char> = k.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase() && c.is_uppercase() && next_lower)
                || (prev.is_alphabetic() != c.is_alphabetic());
            if boundary {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// A string is path-shaped if (after normalization) it contains a path
/// separator, starts with / ~ or ., or contains a traversal token.
fn value_is_pathy(s: &str) -> bool {
    s.contains('/')
        || s.contains('\\')
        || s.starts_with('~')
        || s.starts_with('.')
        || s.contains("..")
}

/// A standalone path-shaped value has no prose/command whitespace and is not
/// a URL. This preserves screening for an opaque field carrying dot-slash-x,
/// while never treating a command or URL role as a filesystem path.
fn value_is_standalone_path(s: &str) -> bool {
    !s.chars().any(char::is_whitespace) && !s.contains("://") && value_is_pathy(s)
}

fn value_is_path_argument(key: Option<&str>, value: &str) -> bool {
    match key {
        Some(k) if key_is_pathy(k) => true,
        Some(k) if key_is_non_path_payload(k) => false,
        _ => value_is_standalone_path(value),
    }
}

/// Percent-decode to a fixed point (bounded) so %2e%2e%2f and
/// double-encoded variants normalize before screening.
pub(crate) fn normalize_path_text(s: &str) -> String {
    // TRUE fixed point (Codex final-gate P0: any fixed ceiling invites
    // off-by-N bypasses). Termination is structural but NOT via byte
    // length (Codex: %ff -> U+FFFD is 3 bytes -> 3 bytes): every pass that
    // changes the string consumed at least one valid %XX sequence, and
    // decoded output introduces no new '%' beyond what literal %25 yields
    // from strictly-consumed sequences — the count of decodable sequences
    // is strictly monotonically decreasing, bounding passes by the input's
    // '%' count.
    let mut cur = s.to_string();
    loop {
        let dec = percent_decode(&cur);
        if dec == cur {
            return cur;
        }
        cur = dec;
    }
}

/// ALL normalized filesystem-path argument values, sorted+deduped — shared
/// by screening and constraint capture/matching so the two layers can never
/// disagree about what counts as a path.
pub(crate) fn path_profile(arguments: &str) -> Vec<String> {
    let mut v = path_args(arguments);
    v.sort();
    v.dedup();
    v
}

/// All opaque command argument values, sorted+deduped.  A command is not a
/// filesystem path, but a grant for a command must bind the exact command so
/// an approval cannot be reused to run a different command.
pub(crate) fn command_profile(arguments: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return vec![];
    };
    let mut out = Vec::new();
    collect_commands(&v, None, &mut out);
    out.sort();
    out.dedup();
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn collect_paths(v: &serde_json::Value, key: Option<&str>, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                collect_paths(val, Some(k), out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_paths(x, key, out)),
        serde_json::Value::String(s) => {
            let norm = normalize_path_text(s);
            if value_is_path_argument(key, &norm) {
                out.push(norm);
            }
        }
        _ => {}
    }
}

fn collect_commands(v: &serde_json::Value, key: Option<&str>, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                collect_commands(val, Some(k), out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_commands(x, key, out)),
        serde_json::Value::String(s) if key.is_some_and(key_is_command) => out.push(s.clone()),
        _ => {}
    }
}

fn key_is_command(k: &str) -> bool {
    key_words(k)
        .iter()
        .any(|w| matches!(w.as_str(), "command" | "cmd"))
}

/// True only when `..` is a complete path component.  Names such as
/// `draft..final.txt` are ordinary filenames, not directory traversal.
pub(crate) fn has_path_traversal(path: &str) -> bool {
    path.split(['/', '\\']).any(|component| component == "..")
}

/// RC2-ENGB1 (TOP43 #17): the path arguments the folder check cannot place
/// for a tool that does NOT resolve relative paths against the folder (any
/// tool but the engine's own file tools — an MCP server resolves them
/// against its own working directory or the home folder). Normalized like
/// [`path_args`]; an absolute path is never listed here (the folder check
/// places it). Listed:
/// - a home or variable form under a path-naming key: `~`, `~/x`,
///   `~user/x`, `$HOME/x`, `${HOME}/x`, and `./x`;
/// - any other relative value under a key that names a path (`path`,
///   `file_path`, `filename`, `dir`, `working_directory`, `folder`, `cwd`, …);
/// - a relative value that looks like a path or a file name (`a/b`,
///   `notes.txt`) under a key that hints at one (`source`, `target`, `to`, …).
///
/// Free text, commands and URLs under other keys are left alone, so ordinary
/// calls keep working.
pub(crate) fn unplaced_path_args(arguments: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return vec![];
    };
    let mut out = Vec::new();
    collect_unplaced(&v, None, &mut out);
    out
}

fn collect_unplaced(v: &serde_json::Value, key: Option<&str>, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                collect_unplaced(val, Some(k), out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_unplaced(x, key, out)),
        serde_json::Value::String(s) => {
            let p = normalize_path_text(s);
            if is_unplaced(&p, key) {
                out.push(p);
            }
        }
        _ => {}
    }
}

fn is_unplaced(p: &str, key: Option<&str>) -> bool {
    if !value_is_path_argument(key, p) {
        return false;
    }
    if p.is_empty() || p.starts_with('/') {
        return false;
    }
    let first = p.split('/').next().unwrap_or("");
    let one_word = !first.chars().any(char::is_whitespace);
    let home = p.starts_with('~') && one_word;
    let var = p.starts_with('$')
        && p[1..].starts_with(|c: char| c == '{' || c == '_' || c.is_ascii_alphabetic())
        && one_word;
    if home || var || p == "." || p.starts_with("./") {
        return true;
    }
    match key {
        Some(k) if key_names_a_path(k) => true,
        Some(k) if key_is_pathy(k) => value_is_pathy(p) || looks_like_a_file_name(p),
        _ => false,
    }
}

/// A key whose value IS a path: `path`, `file_path`, `filePath`, `paths`,
/// `filename`, `file`, `files`, `dir`, `workdir`, `directory`,
/// `working_directory`, `folder`, `cwd`.
fn key_names_a_path(k: &str) -> bool {
    if key_words(k).iter().any(|w| word_has_path_noun(w)) {
        return true;
    }
    if key_is_non_path_payload(k) {
        return false;
    }
    let k = k.to_ascii_lowercase();
    ["path", "paths", "filename", "dir", "directory", "folder"]
        .iter()
        .any(|s| k.ends_with(s))
        || ["file", "files", "cwd"].contains(&k.as_str())
        || k.ends_with("_file")
}

/// One word of name characters ending in a short extension: `notes.txt`,
/// `a.tar.gz` (an address like `a@b.org` is not one).
fn looks_like_a_file_name(p: &str) -> bool {
    if !p
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return false;
    }
    match p.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Classify a call: builtin/registry floor escalated by argument facts.
pub fn classify_risk(call: &ToolCall, _folder_root: Option<&str>) -> RiskClass {
    classify_risk_from(call, builtin_floor(&call.name))
}

/// [`classify_risk`] from a given starting class ([`base_risk`]): argument
/// inspection only escalates it.
pub fn classify_risk_from(call: &ToolCall, base: RiskClass) -> RiskClass {
    let mut risk = base;
    for p in path_args(&call.arguments) {
        if touches_secrets(&p) {
            risk = escalate(risk, RiskClass::Privileged);
        }
    }
    // Deletion escalation (§8): rm-like tools, or truthy recursive/force
    // flags anywhere in the PARSED arguments (Codex C1p2 P1: substring
    // matching misses whitespace/reordering/aliases).
    if call.name == "fs.rm" || has_truthy_flag(&call.arguments, &["recursive", "force", "rf"]) {
        risk = escalate(risk, RiskClass::Irreversible);
    }
    risk
}

fn has_truthy_flag(arguments: &str, names: &[&str]) -> bool {
    fn walk(v: &serde_json::Value, names: &[&str]) -> bool {
        match v {
            serde_json::Value::Object(m) => m.iter().any(|(k, val)| {
                let truthy = match val {
                    serde_json::Value::Bool(b) => *b,
                    serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    serde_json::Value::String(s) => {
                        matches!(s.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
                    }
                    _ => false,
                };
                (names.contains(&k.to_ascii_lowercase().as_str()) && truthy) || walk(val, names)
            }),
            serde_json::Value::Array(a) => a.iter().any(|x| walk(x, names)),
            _ => false,
        }
    }
    serde_json::from_str::<serde_json::Value>(arguments)
        .map(|v| walk(&v, names))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    //! S1(b) proof test (F0532/F0533): the common dotfile secret names gain a
    //! component-based privileged floor while a harmless basename that merely
    //! contains `env` does not. The extended cases (RVS1B-FIX1, F1) prove the
    //! same through `classify_risk`: a call whose arguments name a secret
    //! dotfile gets the `Privileged` class from the public entry point.

    use awili_engine::events::{AwiliCallId, ToolCall};
    use awili_engine_log::authz::RiskClass;

    use super::{base_risk, classify_risk, classify_risk_from, touches_secrets};

    /// Build a `ToolCall` with a fresh call id (same shape as
    /// `tests/authorize.rs` — copied locally, not imported).
    fn call(tool: &str, args: &str) -> ToolCall {
        ToolCall {
            awili_call_id: AwiliCallId("test-call".into()),
            provider_call_id: None,
            name: tool.into(),
            arguments: args.into(),
        }
    }

    #[test]
    fn secret_basename_floor() {
        // Privileged: exact names at any depth, `~`-prefixed, `.env.*` family.
        assert!(touches_secrets(".env"));
        assert!(touches_secrets("project/.env"));
        assert!(touches_secrets("/abs/path/.env"));
        assert!(touches_secrets("~/.env"));
        assert!(touches_secrets(".env.local"));
        assert!(touches_secrets("config/.env.production"));
        assert!(touches_secrets(".netrc"));
        assert!(touches_secrets("home/user/.npmrc"));
        assert!(touches_secrets("/root/.pypirc"));
        // Harmless basenames that merely contain `env` stay unchanged.
        assert!(!touches_secrets("src/environment.rs"));
        assert!(!touches_secrets("environment.rs"));
        assert!(!touches_secrets("env.txt"));
        assert!(!touches_secrets("src/environ.rs"));
        // Existing listed secret locations keep their floor.
        assert!(touches_secrets("/home/u/.ssh/id_ed25519"));
        assert!(touches_secrets("/home/u/.aws/credentials.json"));

        // RVS1B-FIX1 (F1): the floor must be visible through `classify_risk`
        // (the public entry point), not only `touches_secrets` directly.
        let c = call("fs.read", r#"{"path": ".env"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.read {{\"path\": \".env\"}}"
        );
        let c = call("fs.read", r#"{"path": "project/.env.local"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.read {{\"path\": \"project/.env.local\"}}"
        );
        let c = call("fs.read", r#"{"path": "/home/u/.netrc"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.read {{\"path\": \"/home/u/.netrc\"}}"
        );
        let c = call("fs.write", r#"{"path": ".env", "content": "x"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.write {{\"path\": \".env\", \"content\": \"x\"}}"
        );
        let c = call("fs.read", r#"{"target": {"file": "config/.npmrc"}}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.read {{\"target\": {{\"file\": \"config/.npmrc\"}}}}"
        );
        let c = call("fs.read", r#"{"path": "%2eenv"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.read {{\"path\": \"%2eenv\"}}"
        );
        let c = call("fs.read", r#"{"path": "CONFIG/.ENV"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Privileged,
            "fs.read {{\"path\": \"CONFIG/.ENV\"}}"
        );
        // A harmless basename merely containing `env` keeps its ordinary floor.
        let c = call("fs.read", r#"{"path": "src/environment.rs"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::ReadOnly,
            "fs.read {{\"path\": \"src/environment.rs\"}}"
        );
        let c = call(
            "fs.write",
            r#"{"path": "src/environment.rs", "content": "x"}"#,
        );
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::Mutating,
            "fs.write {{\"path\": \"src/environment.rs\", \"content\": \"x\"}}"
        );
        let c = call("fs.read", r#"{"path": "config/env.txt"}"#);
        assert_eq!(
            classify_risk(&c, None),
            RiskClass::ReadOnly,
            "fs.read {{\"path\": \"config/env.txt\"}}"
        );
    }

    /// RC1-M-FIX1: an MCP tool starts from the class its registry entry
    /// declares (its server's own annotations), not from the "unknown tool"
    /// `privileged`; the engine's own tools keep their floors (a registry
    /// floor only raises them); argument inspection still escalates; nothing
    /// known is `privileged`.
    #[test]
    fn rc1m_fix1_an_mcp_tool_starts_from_its_declared_class() {
        assert_eq!(
            base_risk("recall", Some(RiskClass::ReadOnly)),
            RiskClass::ReadOnly
        );
        assert_eq!(
            base_risk("remember", Some(RiskClass::Mutating)),
            RiskClass::Mutating
        );
        assert_eq!(
            base_risk("run_command", Some(RiskClass::Privileged)),
            RiskClass::Privileged
        );
        assert_eq!(base_risk("recall", None), RiskClass::Privileged);
        assert_eq!(base_risk("fs.read", None), RiskClass::ReadOnly);
        assert_eq!(
            base_risk("fs.read", Some(RiskClass::Mutating)),
            RiskClass::Mutating
        );
        assert_eq!(
            base_risk("fs.rm", Some(RiskClass::ReadOnly)),
            RiskClass::Irreversible
        );
        let read = call("recall", r#"{"query":"hostd outage"}"#);
        assert_eq!(
            classify_risk_from(&read, base_risk("recall", Some(RiskClass::ReadOnly))),
            RiskClass::ReadOnly
        );
        let secret = call("read_file", r#"{"path":"/Users/j/.ssh/id_ed25519"}"#);
        assert_eq!(
            classify_risk_from(&secret, base_risk("read_file", Some(RiskClass::ReadOnly))),
            RiskClass::Privileged
        );
        assert_eq!(
            classify_risk(&read, None),
            RiskClass::Privileged,
            "no registry: unknown"
        );
    }
}
