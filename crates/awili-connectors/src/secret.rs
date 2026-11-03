//! Secret resolution for the connector clients. A [`SecretRef`] names *where*
//! a secret lives — a file (whole file, or one `KEY=VALUE` line) or an
//! environment variable — without ever holding the value itself, so it is safe
//! to store in config and to render in `Debug`. Resolving yields a [`Secret`]
//! whose value is only accessible through `as_str()` and never appears in any
//! `Debug` output or error message.

use std::fmt;
use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::error::ConnectorError;

/// A reference to a secret: where it is, not what it is. Debug-renderable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretRef {
    /// Read the secret from `path`. When `key` is `None` the whole file is the
    /// secret (trimmed); when `Some("K")` the file is a `KEY=VALUE` config file
    /// and the value of the `K` line is taken.
    File {
        /// Path to the secret file. `~` is expanded against `$HOME`.
        path: PathBuf,
        /// `None` = whole file is the token; `Some(key)` = `key`'s `KEY=VALUE` line.
        key: Option<String>,
    },
    /// Read the secret from the environment variable `name`.
    Env {
        /// The variable name (e.g. `"OUTLINE_API_KEY"`), not the value.
        name: String,
    },
}

/// A resolved secret value. The value is accessible only via [`Secret::as_str`]
/// and is NEVER rendered: `Debug` prints `<redacted>`, and there is no
/// `Display` impl, so a log line cannot leak it by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    value: String,
}

impl fmt::Debug for Secret {
    /// The value never prints. Presence is observable, contents are not.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secret")
            .field("value", &"<redacted>")
            .finish()
    }
}

impl Secret {
    /// The secret's value. Treat as sensitive: never log, never format.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl SecretRef {
    /// Resolve this reference to a concrete [`Secret`].
    ///
    /// Errors are [`ConnectorError::Secret`] and name only the file/key or the
    /// variable name that failed — never a secret value.
    pub fn resolve(&self) -> Result<Secret, ConnectorError> {
        match self {
            SecretRef::File { path, key } => self.resolve_file(path, key.as_deref()),
            SecretRef::Env { name } => std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(|v| Secret {
                    value: v.trim().to_string(),
                })
                .ok_or_else(|| ConnectorError::Secret {
                    path: None,
                    key: Some(name.clone()),
                    reason: "environment variable is unset or empty".to_string(),
                }),
        }
    }

    /// Resolve a file-backed reference. `~` is expanded against `$HOME`.
    fn resolve_file(&self, raw: &Path, key: Option<&str>) -> Result<Secret, ConnectorError> {
        let path = expand_home(raw);
        let bytes = fs::read(&path).map_err(|e| ConnectorError::Secret {
            path: Some(path.clone()),
            key: key.map(str::to_string),
            reason: io_reason(&e),
        })?;
        let text = String::from_utf8(bytes).map_err(|_| ConnectorError::Secret {
            path: Some(path.clone()),
            key: key.map(str::to_string),
            reason: "file is not valid UTF-8".to_string(),
        })?;
        match key {
            None => whole_file_secret(&text, &path),
            Some(key) => key_line_secret(&text, key, &path),
        }
    }
}

/// Whole-file token: the file IS the secret. Trimmed; empty is an error.
fn whole_file_secret(text: &str, path: &Path) -> Result<Secret, ConnectorError> {
    let value = text.trim();
    if value.is_empty() {
        return Err(ConnectorError::Secret {
            path: Some(path.to_path_buf()),
            key: None,
            reason: "file is empty".to_string(),
        });
    }
    Ok(Secret {
        value: value.to_string(),
    })
}

/// `KEY=VALUE` line: take the value of the first line whose key matches. An
/// optional `export ` prefix is allowed, and one layer of matching quotes is
/// stripped. Only the key name may appear in errors, never the value.
fn key_line_secret(text: &str, key: &str, path: &Path) -> Result<Secret, ConnectorError> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let value = strip_quotes(value.trim());
        if value.is_empty() {
            return Err(ConnectorError::Secret {
                path: Some(path.to_path_buf()),
                key: Some(key.to_string()),
                reason: "key is present but its value is empty".to_string(),
            });
        }
        return Ok(Secret {
            value: value.to_string(),
        });
    }
    Err(ConnectorError::Secret {
        path: Some(path.to_path_buf()),
        key: Some(key.to_string()),
        reason: "key not found in file".to_string(),
    })
}

/// Strip one layer of matching single or double quotes.
fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[s.len() - 1] == b'"') || (b[0] == b'\'' && b[s.len() - 1] == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Expand a leading `~` against `$HOME`. A path that is not `~` or `~/...`
/// is returned unchanged.
fn expand_home(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if s == "~" {
        return home_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home_dir()
            .map(|h| Path::new(&h).join(rest))
            .unwrap_or_else(|| path.to_path_buf());
    }
    path.to_path_buf()
}

fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

/// Render an `io::Error` as a short, secret-free reason string.
fn io_reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "file not found".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        other => format!("io error: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pid-suffixed temp dir so parallel test binaries never collide.
    fn seam_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("awili-connectors-seam-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn seam_file(name: &str) -> PathBuf {
        seam_dir().join(name)
    }

    #[test]
    fn secret_ref_file_whole_token_trims_newline() {
        let p = seam_file("whole-token");
        fs::write(&p, "ghp_aBcDef123\n").unwrap();
        let s = SecretRef::File { path: p, key: None }.resolve().unwrap();
        assert_eq!(s.as_str(), "ghp_aBcDef123");
    }

    #[test]
    fn secret_ref_file_key_value_with_export_and_quotes() {
        let p = seam_file("key-value.conf");
        fs::write(
            &p,
            "# comment line\nFOO=bar\nexport OUTLINE_API_KEY=\"sk-outline-quoted-secret\"\nOTHER = 'single-quoted'\n",
        )
        .unwrap();
        let s = SecretRef::File {
            path: p.clone(),
            key: Some("OUTLINE_API_KEY".to_string()),
        }
        .resolve()
        .unwrap();
        assert_eq!(s.as_str(), "sk-outline-quoted-secret");
        let other = SecretRef::File {
            path: p,
            key: Some("OTHER".to_string()),
        }
        .resolve()
        .unwrap();
        assert_eq!(other.as_str(), "single-quoted");
    }

    #[test]
    fn secret_ref_missing_key_error_names_key_not_value() {
        let p = seam_file("missing-key.conf");
        fs::write(&p, "PRESENT=secret-value-here\n").unwrap();
        let err = SecretRef::File {
            path: p,
            key: Some("ABSENT".to_string()),
        }
        .resolve()
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ABSENT"), "error must name the key: {msg}");
        assert!(
            !msg.contains("secret-value-here"),
            "error must not echo any value: {msg}"
        );
        assert!(
            !format!("{err:?}").contains("secret-value-here"),
            "Debug must not echo any value: {err:?}"
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret {
            value: "ultra-super-secret-value".to_string(),
        };
        let rendered = format!("{secret:?}");
        assert!(
            rendered.contains("<redacted>"),
            "Debug must show redaction: {rendered}"
        );
        assert!(
            !rendered.contains("ultra-super-secret-value"),
            "Debug must not contain the value: {rendered}"
        );
    }
}
