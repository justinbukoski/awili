//! RC1-M: hold a Codex planning step's MCP tools to the owner's chosen mode.
//!
//! "Ask before changes" runs Codex in its read-only sandbox, but Codex runs
//! MCP tools outside that sandbox: a tool the owner set to run without asking
//! (`approval_mode = "approve"`, as his mac-shell `run_command` is) would act
//! before he approves the plan (live 2026-10-03, codex 0.159: `touch` ran in
//! a read-only step; RC1-L review P1, Codex gpt-6-sol on 1d90a4f). With
//! approval "writes", a tool its server marks read-only runs and every other
//! tool needs an approval that `codex exec` never grants ("MCP tool call
//! requires approval, but approval policy is never"). No tool is removed.
//! The owner's `config.toml` is only read here, never written.

/// The approval a planning step gives every MCP server.
pub const PLAN_APPROVAL: &str = "writes";

fn quoted(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// A TOML key part: bare when it can be, else quoted.
fn key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        quoted(k)
    }
}

/// The `-c` values that hold a planning step's MCP tools to
/// [`PLAN_APPROVAL`]: for every server in the owner's Codex `config.toml`
/// (`config_text`) that is not switched off, and every server in `extra`
/// (servers Awili adds), the server default; and every per-tool approval the
/// owner set to anything but "prompt" (which already asks). Unparsable text
/// counts as no servers.
pub fn approval_overrides(config_text: &str, extra: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let doc = config_text.parse::<toml::Table>().unwrap_or_default();
    let mut seen: Vec<String> = Vec::new();
    if let Some(servers) = doc.get("mcp_servers").and_then(|s| s.as_table()) {
        for (name, server) in servers {
            if server.get("enabled").and_then(|e| e.as_bool()) == Some(false) {
                continue;
            }
            seen.push(name.clone());
            let base = format!("mcp_servers.{}", key(name));
            out.push(format!(
                "{base}.default_tools_approval_mode={}",
                quoted(PLAN_APPROVAL)
            ));
            let tools = server.get("tools").and_then(|t| t.as_table());
            for (tool, cfg) in tools.into_iter().flatten() {
                let mode = cfg.get("approval_mode").and_then(|m| m.as_str());
                if matches!(mode, Some(m) if m != "prompt" && m != PLAN_APPROVAL) {
                    out.push(format!(
                        "{base}.tools.{}.approval_mode={}",
                        key(tool),
                        quoted(PLAN_APPROVAL)
                    ));
                }
            }
        }
    }
    for name in extra {
        if !seen.iter().any(|s| s == name) {
            out.push(format!(
                "mcp_servers.{}.default_tools_approval_mode={}",
                key(name),
                quoted(PLAN_APPROVAL)
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc1m_every_owner_server_and_auto_tool_is_held_to_writes() {
        let text = r#"
model = "gpt"
[mcp_servers.interlock]
command = "/bin/interlock"
[mcp_servers.interlock.tools.handoff_close]
approval_mode = "approve"
[mcp_servers.mac-shell]
command = "/bin/mac-shell"
[mcp_servers.mac-shell.tools.run_command]
approval_mode = "approve"
[mcp_servers.mac-shell.tools.read_file]
approval_mode = "prompt"
[mcp_servers.docs]
url = "https://example.com/mcp"
default_tools_approval_mode = "auto"
[mcp_servers.off]
command = "/bin/off"
enabled = false
[mcp_servers."odd.name"]
command = "/bin/odd"
"#;
        let got = approval_overrides(text, &["awili-connectors", "docs"]);
        assert_eq!(
            got,
            vec![
                "mcp_servers.docs.default_tools_approval_mode=\"writes\"",
                "mcp_servers.interlock.default_tools_approval_mode=\"writes\"",
                "mcp_servers.interlock.tools.handoff_close.approval_mode=\"writes\"",
                "mcp_servers.mac-shell.default_tools_approval_mode=\"writes\"",
                "mcp_servers.mac-shell.tools.run_command.approval_mode=\"writes\"",
                "mcp_servers.\"odd.name\".default_tools_approval_mode=\"writes\"",
                "mcp_servers.awili-connectors.default_tools_approval_mode=\"writes\"",
            ]
        );
        assert_eq!(
            approval_overrides("not = [toml", &["x"]),
            vec!["mcp_servers.x.default_tools_approval_mode=\"writes\""]
        );
        assert!(approval_overrides("model = \"x\"\n", &[]).is_empty());
    }
}
