//! RC2-R2F7 (usability RC2 round 2, F3 MAJOR: a council's in-app Gemma
//! refused to read `/Projects/…` — "outside my session root" — after asking
//! for a read-only glob). A host whose model works for the owner anywhere
//! (a council member: there is no folder to point it at) lets the engine's
//! own file tools reach every folder the owner can: a call with a full path
//! outside the session's folder is authorized and run against THAT path's
//! own folder instead. Nothing else changes: reads pass freely, a change
//! still stops at the owner's card, secret files and deletions still ask,
//! `..` is still refused, and the sandbox still walks descriptor by
//! descriptor beneath the root it is given.

use std::path::{Component, Path};

/// The folder one engine file-tool call runs against when the host lets
/// the model reach any folder: for a full path outside `session_root`, the
/// folder that holds what the call names (the file's folder for
/// `fs.read`/`fs.write`/`fs.edit`; RC2-R2G3: the searched folder itself for
/// `fs.glob` and `fs.grep`, so a search at a mounted volume's root (`/Projects`)
/// never starts on the volume above it, whose sandbox refuses the mount
/// crossing). `None` — the session's own folder — for a relative path, a
/// path inside the session's folder, a path with `..`, or anything that is
/// not one of the engine's file tools.
pub fn call_root(name: &str, arguments: &str, session_root: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let arg = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
    let (path, searched_dir) = match name {
        "fs.read" | "fs.write" | "fs.edit" => (arg("path")?, false),
        "fs.glob" => (arg("pattern")?.rsplit_once('/')?.0, true),
        "fs.grep" => (arg("under")?, true),
        _ => return None,
    };
    // A glob of "/*" searches the top folder itself.
    let path = if searched_dir && path.is_empty() {
        "/"
    } else {
        path
    };
    let p = Path::new(path);
    if !p.is_absolute() || p.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    if p.starts_with(Path::new(session_root)) {
        return None;
    }
    let root: std::path::PathBuf = if searched_dir {
        p.components().collect()
    } else {
        p.parent().unwrap_or(p).to_path_buf()
    };
    Some(root.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::call_root;

    #[test]
    fn rc2r2f7_a_full_path_outside_the_folder_runs_against_its_own_folder() {
        let s = "/Users/j";
        let r = |name: &str, args: &str| call_root(name, args, s);
        assert_eq!(
            r("fs.glob", r#"{"pattern":"/Projects/night/w1/*"}"#).as_deref(),
            // RC2-R2G3: a search runs from the searched folder itself.
            Some("/Projects/night/w1")
        );
        assert_eq!(
            r("fs.read", r#"{"path":"/Projects/night/w1/alpha.txt"}"#).as_deref(),
            Some("/Projects/night/w1")
        );
        assert_eq!(
            r(
                "fs.write",
                r#"{"path":"/Projects/w1/notes.txt","contents":"hi"}"#
            )
            .as_deref(),
            Some("/Projects/w1")
        );
        assert_eq!(
            r("fs.grep", r#"{"needle":"x","under":"/Projects/w1"}"#).as_deref(),
            Some("/Projects/w1")
        );
        assert_eq!(r("fs.glob", r#"{"pattern":"/*"}"#).as_deref(), Some("/"));
        // The session's own folder, a relative path, `..` and other tools
        // keep the session's folder.
        assert_eq!(r("fs.read", r#"{"path":"/Users/j/notes.txt"}"#), None);
        assert_eq!(r("fs.read", r#"{"path":"notes.txt"}"#), None);
        assert_eq!(r("fs.read", r#"{"path":"/Projects/../etc/passwd"}"#), None);
        assert_eq!(r("mcp.read_file", r#"{"path":"/Projects/a"}"#), None);
        assert_eq!(
            r("fs.read", r#"{"path":"/Users/jx/a.txt"}"#).as_deref(),
            Some("/Users/jx")
        );
    }
}
