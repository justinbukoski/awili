//! URL helpers shared by the connector clients. Kept in their own module so
//! neither client file carries generic percent-encoding logic.

use crate::error::ConnectorError;

/// Extract the host (no port) from a URL string. Returns `None` if the URL has
/// no recognizable host.
pub(crate) fn host(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let host = after_scheme.split(['/', '?', '#']).next()?.to_string();
    let host = host.rsplit('@').next()?.to_string();
    let host = host.split(':').next()?.to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Percent-encode a single URL path segment. Preserves `/` so callers can pass
/// multi-segment paths like `src/lib.rs`.
pub(crate) fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encode a repo-internal path, collapsing any leading `/` so the
/// `raw_file` URL has exactly one `/raw/` segment (no double).
///
/// Fails closed BEFORE any request is built: after percent-decoding, every
/// component must be non-empty and not `.` or `..`. A dot segment would let a
/// repo path escape the repo root on the server side (`/raw/../secrets`), so
/// such a path is rejected locally instead of being sent authenticated.
pub(crate) fn repo_path(path: &str) -> Result<String, ConnectorError> {
    let p = path.trim_start_matches('/');
    for comp in p.split('/') {
        let decoded = percent_decode(comp);
        if decoded.is_empty() || decoded == "." || decoded == ".." {
            return Err(ConnectorError::Decode(format!(
                "repo path component {comp:?} is empty, \".\", or \"..\" after percent decoding; refusing to build a raw file URL"
            )));
        }
    }
    Ok(segment(p))
}

/// A Gitea owner or repo name as ONE safe URL path component (S6(a)).
/// Allowed: non-empty, only ASCII `[A-Za-z0-9._-]`, and not `.` or `..`.
/// Anything else (`/`, `\`, `%`, space, `?`, `#`, non-ASCII) is refused
/// before any request is built.
pub(crate) fn repo_component<'a>(value: &'a str, field: &str) -> Result<&'a str, ConnectorError> {
    let safe = !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'));
    if safe {
        Ok(value)
    } else {
        Err(ConnectorError::Decode(format!(
            "gitea {field} {value:?} is not a single safe path component; refusing to build an authenticated URL"
        )))
    }
}

/// Percent-decode a single path component (`%XX` → byte). Invalid escape
/// sequences are left verbatim — they cannot decode to `.` or `..`, so the
/// validation above still holds.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parses_api_base() {
        assert_eq!(
            host("http://198.51.100.7:3000/api/v1"),
            Some("198.51.100.7".to_string())
        );
        assert_eq!(
            host("https://gitea.example.com/api/v1"),
            Some("gitea.example.com".to_string())
        );
        assert_eq!(host("garbage"), None);
    }

    #[test]
    fn repo_path_strips_leading_slash() {
        assert_eq!(repo_path("/src/lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(repo_path("src/lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(repo_path("a/b c.txt").unwrap(), "a/b%20c.txt");
    }

    #[test]
    fn repo_path_rejects_dot_segments_after_decoding() {
        for bad in [
            ".",
            "..",
            "a/../b",
            "a/./b",
            "/../x",
            "%2e",
            "%2E%2E",
            "a/%2e%2e/b",
            "",
            "/",
            "//",
            "a//b",
            "a/",
        ] {
            assert!(repo_path(bad).is_err(), "path {bad:?} must be rejected");
        }
        // Valid multi-component paths survive. A double-encoded `%252E` decodes
        // to the literal text `%2E` (a legal filename, not `.`), so it is kept —
        // and re-encoding that literal doubles the percent again (`%25252E`),
        // which a server decodes back to `%2E`: the filename round-trips.
        assert_eq!(repo_path("src/lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(repo_path("a/b c.txt").unwrap(), "a/b%20c.txt");
        assert_eq!(repo_path("%252E%252E.txt").unwrap(), "%25252E%25252E.txt");
    }

    #[test]
    fn repo_component_accepts_only_single_safe_names() {
        for good in ["testuser", "awili", "test-user.name_1", "a.b"] {
            assert_eq!(
                repo_component(good, "owner").ok(),
                Some(good),
                "{good:?} must be accepted"
            );
        }
        for bad in [
            "", ".", "..", "a/b", "../admin", "%2e%2e", "a%2Fb", "a\\b", "a b", "a?b", "a#b", "ünï",
        ] {
            assert!(
                repo_component(bad, "owner").is_err(),
                "{bad:?} must be refused before any request is built"
            );
        }
    }
}
