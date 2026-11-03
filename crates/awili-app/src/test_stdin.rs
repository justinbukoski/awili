//! S1b: the agent CLIs are asked on stdin, so the fake CLIs the tests run
//! record their stdin beside their argv. These read it back.

/// The prompt a fake Claude was sent: the first stream-json user line's text.
pub(crate) fn claude_prompt(stdin: &str) -> Option<String> {
    let line = stdin.lines().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let content = &v["message"]["content"];
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// `argv` with the prompt the CLI read on `stdin` pushed last, where the
/// positional prompt used to be (a Claude stream-json line, else raw text).
pub(crate) fn argv_with_prompt(mut argv: Vec<String>, stdin: &str) -> Vec<String> {
    let prompt = claude_prompt(stdin).unwrap_or_else(|| stdin.trim_end().to_string());
    argv.push(prompt);
    argv
}

/// The prompt a fake Claude recorded raw stdin into `path`: the user line's
/// text, or the file as is when it is not a stream-json line.
pub(crate) fn read_prompt(path: &std::path::Path) -> std::io::Result<String> {
    let raw = std::fs::read_to_string(path)?;
    Ok(claude_prompt(&raw).unwrap_or(raw))
}
