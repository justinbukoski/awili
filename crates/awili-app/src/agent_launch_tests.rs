//! RC1-M proof tests: the AGENTS.md chain, the CLAUDE.md excludes, and the
//! exact launch of every engine — pure over a fake home, plus real spawns
//! through `CodeProvider` with fake
//! `claude` / `codex` binaries that record their argv.

use std::path::{Path, PathBuf};

use super::*;
use awili_code::Autonomy::{AskFirst, Autonomous as Auto};
use awili_code::CodeEngine::{Claude, Codex, Dsh, Glm};

/// A fake owner home: `~/.agents/AGENTS.md` (USER-RULE), `~/.codex/AGENTS.md`
/// (a symlink to it, as on the owner's Mac), `~/.claude.json` with two user
/// servers and one local-scope server for the project, a Codex config with
/// an auto-approved tool, a fake interlock program; and a git project with a
/// nested folder, AGENTS.md files and a CLAUDE.md.
struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    sub: PathBuf,
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fx() -> Fx {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let home = base.join("home");
    let project = base.join("work").join("proj");
    let sub = project.join("crates").join("a");
    std::fs::create_dir_all(home.join(".agents")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(home.join(".agents/AGENTS.md"), "USER-RULE: fleet rules.\n").unwrap();
    std::os::unix::fs::symlink(
        home.join(".agents/AGENTS.md"),
        home.join(".codex/AGENTS.md"),
    )
    .unwrap();
    std::fs::write(home.join(".local/bin/interlock-mcp"), "#!/bin/sh\n").unwrap();
    std::fs::write(
        home.join(".claude.json"),
        serde_json::json!({
            "mcpServers": { "mac-shell": {"command": "x"}, "interlock": {"command": "y"} },
            "projects": {
                project.display().to_string(): { "mcpServers": { "local-db": {"command": "z"} } },
                "/elsewhere": { "mcpServers": { "not-here": {"command": "z"} } }
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join(".codex/config.toml"),
        "model = \"gpt\"\n[mcp_servers.mac-shell]\ncommand = \"/bin/ms\"\n\
         [mcp_servers.mac-shell.tools.run_command]\napproval_mode = \"approve\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"proj-tool":{"command":"p"}}}"#,
    )
    .unwrap();
    std::fs::write(project.join("AGENTS.md"), "ROOT-FACT\n").unwrap();
    std::fs::write(sub.join("AGENTS.md"), "SUB-FACT\n").unwrap();
    std::fs::write(project.join("CLAUDE.md"), "CLAUDE-RULE\n").unwrap();
    Fx {
        _tmp: tmp,
        home,
        project,
        sub,
    }
}

impl Fx {
    fn paths(&self) -> LaunchPaths {
        LaunchPaths {
            home: self.home.clone(),
            codex_home: self.home.join(".codex"),
        }
    }
}

fn owner_mem() -> AgentMemoryConfig {
    AgentMemoryConfig {
        interlock_command: Some("~/.local/bin/interlock-mcp".into()),
        interlock_env: [(
            "INTERLOCK_URL".to_string(),
            "http://127.0.0.1:1".to_string(),
        )]
        .into_iter()
        .collect(),
        codex_model: Some("gpt-6.1-sol".into()),
        codex_reasoning_effort: Some("low".into()),
        ..Default::default()
    }
}

fn spec<'a>(engine: CodeEngine, autonomy: Autonomy, cwd: &'a Path) -> LaunchSpec<'a> {
    LaunchSpec {
        engine,
        autonomy,
        cwd,
        system: Some("SYSTEM-TEXT"),
        mem: None,
        agents_md: None,
        connectors: None,
    }
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

#[test]
fn rc1m_user_agents_md_default_configured_and_off() {
    let f = fx();
    let got = user_agents_md(None, &f.home).expect("the default file");
    assert_eq!(got.path, f.home.join(".agents/AGENTS.md"));
    assert_eq!(got.text, "USER-RULE: fleet rules.\n");
    let other = f.home.join("mine.md");
    std::fs::write(&other, "MINE").unwrap();
    assert_eq!(
        user_agents_md(Some("~/mine.md"), &f.home).unwrap().text,
        "MINE"
    );
    assert_eq!(user_agents_md(Some(""), &f.home), None);
    assert_eq!(user_agents_md(Some("~/missing.md"), &f.home), None);
    std::fs::write(&other, "  \n").unwrap();
    assert_eq!(user_agents_md(Some("~/mine.md"), &f.home), None, "blank");
}

#[test]
fn rc1m_project_chain_is_codex_semantics() {
    let f = fx();
    let chain = project_chain(&f.sub);
    let texts: Vec<&str> = chain.iter().map(|d| d.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["ROOT-FACT\n", "SUB-FACT\n"],
        "root first, one per folder"
    );
    // An override wins in its folder; nothing above the git root is read.
    std::fs::write(f.sub.join("AGENTS.override.md"), "OVERRIDE\n").unwrap();
    std::fs::write(
        f.project.parent().unwrap().join("AGENTS.md"),
        "ABOVE-ROOT\n",
    )
    .unwrap();
    let texts: Vec<String> = project_chain(&f.sub).into_iter().map(|d| d.text).collect();
    assert_eq!(texts, vec!["ROOT-FACT\n", "OVERRIDE\n"]);
    // No git root: the folder alone.
    let lone = f.home.join("lone");
    std::fs::create_dir_all(lone.join("x")).unwrap();
    std::fs::write(lone.join("AGENTS.md"), "LONE-PARENT\n").unwrap();
    std::fs::write(lone.join("x/AGENTS.md"), "LONE\n").unwrap();
    let texts: Vec<String> = project_chain(&lone.join("x"))
        .into_iter()
        .map(|d| d.text)
        .collect();
    assert_eq!(texts, vec!["LONE\n"]);
    // 32 KiB for the whole chain: the root file fills it, the next is cut.
    std::fs::remove_file(f.sub.join("AGENTS.override.md")).unwrap();
    std::fs::write(
        f.project.join("AGENTS.md"),
        "r".repeat(PROJECT_DOC_MAX_BYTES - 4),
    )
    .unwrap();
    let chain = project_chain(&f.sub);
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[1].text, "SUB-", "{:?}", chain[1].text);
    let total: usize = chain.iter().map(|d| d.text.len()).sum();
    assert_eq!(total, PROJECT_DOC_MAX_BYTES);
}

#[test]
fn rc1m_instructions_block_names_files_once() {
    let f = fx();
    let user = user_agents_md(None, &f.home).unwrap();
    let codex_copy = AgentsMd {
        path: f.home.join(".codex/AGENTS.md"),
        text: user.text.clone(),
    };
    let mut docs = vec![user, codex_copy];
    docs.extend(project_chain(&f.sub));
    let block = instructions_block(&docs).unwrap();
    assert_eq!(
        block.matches("USER-RULE").count(),
        1,
        "same file once: {block}"
    );
    let (r, s) = (
        block.find("ROOT-FACT").unwrap(),
        block.find("SUB-FACT").unwrap(),
    );
    assert!(block.find("USER-RULE").unwrap() < r && r < s, "{block}");
    assert!(block.contains(&format!(
        "Instructions from {}:",
        f.project.join("AGENTS.md").display()
    )));
    assert_eq!(instructions_block(&[]), None);
}

#[test]
fn rc1m_claude_excludes_every_claude_md() {
    let home = Path::new("/Users/o");
    let v: serde_json::Value = serde_json::from_str(&claude_settings_arg(home)).unwrap();
    let ex: Vec<&str> = v["claudeMdExcludes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    for want in [
        "**/CLAUDE.md",
        "**/CLAUDE.local.md",
        "**/.claude/rules/**",
        "/Users/o/.claude/CLAUDE.md",
        "/Users/o/.claude/rules/**",
    ] {
        assert!(ex.contains(&want), "{want}: {ex:?}");
    }
    assert_eq!(v.as_object().unwrap().len(), 1, "only the excludes: {v}");
}

/// Claude Work (Auto): the owner's settings stay in force (no
/// `--setting-sources`, no `--strict-mcp-config`, no MCP or hook files of
/// Awili's); the excludes; ONE `--append-system-prompt` with the user
/// AGENTS.md then Awili's text; no allow flag outside a planning step.
#[test]
fn rc1m_claude_work_launch() {
    let f = fx();
    let l = launch(&f.paths(), &spec(Claude, Auto, &f.sub));
    assert_eq!(l.prompt_system, None);
    assert_eq!(l.args.len(), 4, "{:?}", l.args);
    assert_eq!(
        value_after(&l.args, "--settings"),
        Some(claude_settings_arg(&f.home).as_str())
    );
    assert_eq!(
        value_after(&l.args, "--append-system-prompt"),
        Some("USER-RULE: fleet rules.\n\nSYSTEM-TEXT")
    );
    for gone in [
        "--setting-sources",
        "--strict-mcp-config",
        "--mcp-config",
        "--system-prompt",
    ] {
        assert!(!l.args.iter().any(|a| a == gone), "{gone}: {:?}", l.args);
    }
    // No system text: the user file alone. Nothing at all: no flag.
    let mut s = spec(Claude, Auto, &f.sub);
    s.system = None;
    assert_eq!(
        value_after(&launch(&f.paths(), &s).args, "--append-system-prompt"),
        Some("USER-RULE: fleet rules.")
    );
    s.agents_md = Some("");
    assert!(!launch(&f.paths(), &s)
        .args
        .iter()
        .any(|a| a == "--append-system-prompt"));
}

/// The user file is not appended when Claude Code reads it itself: it IS the
/// AGENTS.md of the working folder or a folder above it (the owner's
/// `~/Projects/claude-md` repo).
#[test]
fn rc1m_claude_skips_a_user_file_it_reads_natively() {
    let f = fx();
    let own = f.project.join("AGENTS.md").display().to_string();
    let s = LaunchSpec {
        agents_md: Some(&own),
        ..spec(Claude, Auto, &f.sub)
    };
    assert_eq!(
        value_after(&launch(&f.paths(), &s).args, "--append-system-prompt"),
        Some("SYSTEM-TEXT")
    );
}

#[cfg(unix)]
/// RC1-N-FIX2 (coordinator measurement 2026-10-04: a Claude turn whose
/// working folder is in the owner's `~/Projects/claude-md` repo carried his
/// AGENTS.md twice when the user file was appended there): the owner's
/// `~/.agents/AGENTS.md` is a SYMLINK to the repo's `AGENTS.md`. Awili
/// compares real paths, so with the working folder anywhere in that repo the
/// user file is never appended — Claude reads the same file itself — and the
/// first-turn launch carries it zero times; outside the repo it is appended
/// once. Also through a symlinked working folder and the reverse link (the
/// repo's file a link to the home one).
#[test]
fn rc1n_fix2_claude_never_appends_a_symlinked_user_file_it_reads() {
    let f = fx();
    let user = f.home.join(".agents/AGENTS.md");
    std::fs::remove_file(&user).unwrap();
    std::fs::write(f.project.join("AGENTS.md"), "OWNER-FLEET-RULES\n").unwrap();
    std::os::unix::fs::symlink(f.project.join("AGENTS.md"), &user).unwrap();
    let appended = |cwd: &std::path::Path| {
        value_after(
            &launch(&f.paths(), &spec(Claude, Auto, cwd)).args,
            "--append-system-prompt",
        )
        .unwrap_or("")
        .to_string()
    };
    for cwd in [f.project.clone(), f.sub.clone()] {
        let a = appended(&cwd);
        assert!(!a.contains("OWNER-FLEET-RULES"), "{cwd:?}: {a}");
        assert_eq!(a, "SYSTEM-TEXT");
    }
    // A working folder reached through a symlink.
    let link = f.home.join("proj-link");
    std::os::unix::fs::symlink(&f.project, &link).unwrap();
    assert!(!appended(&link.join("crates/a")).contains("OWNER-FLEET-RULES"));
    // Outside the repo: Claude does not read it, so it is appended, once.
    let elsewhere = f.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(appended(&elsewhere).matches("OWNER-FLEET-RULES").count(), 1);
    // The reverse link: the repo's AGENTS.md points at the home file.
    std::fs::remove_file(&user).unwrap();
    std::fs::remove_file(f.project.join("AGENTS.md")).unwrap();
    std::fs::write(&user, "OWNER-FLEET-RULES\n").unwrap();
    std::os::unix::fs::symlink(&user, f.project.join("AGENTS.md")).unwrap();
    assert!(!appended(&f.sub).contains("OWNER-FLEET-RULES"));
}

/// Claude Ask: plan mode comes from `permission_args`; the launch allows
/// every MCP server the owner has for this folder BY NAME — user scope,
/// local scope for this project, project `.mcp.json`, and Awili's connector
/// server — so their read-only tools run on every model (plan mode still
/// refuses their writing tools). One argv element.
#[test]
fn rc1m_claude_ask_allows_every_server_by_name() {
    let f = fx();
    let s = LaunchSpec {
        connectors: Some("awili-connectors"),
        ..spec(Claude, AskFirst, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    let allow: Vec<&String> = l
        .args
        .iter()
        .filter(|a| a.starts_with("--allowedTools"))
        .collect();
    assert_eq!(
        allow,
        vec![
            "--allowedTools=mcp__awili-connectors,mcp__interlock,mcp__local-db,mcp__mac-shell,mcp__proj-tool"
        ]
    );
    assert!(!l
        .args
        .iter()
        .any(|a| a.starts_with("--disallowedTools") || a.starts_with("--tools")));
    assert_eq!(claude_server_allow_arg(&[]), None);
}

/// Codex: the owner's own CODEX_HOME (no env, no private home); Awili's
/// text as `developer_instructions` (a TOML string); the user file only when
/// Codex does not read it from its home already; the model overrides from
/// `[work.agents.memory]`; in Ask every MCP server held to "writes".
#[test]
fn rc1m_codex_launch() {
    let f = fx();
    let mem = owner_mem();
    let s = LaunchSpec {
        mem: Some(&mem),
        ..spec(Codex, Auto, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    assert_eq!(
        l.args,
        vec![
            "-c",
            "developer_instructions=\"SYSTEM-TEXT\"",
            "-c",
            "model=\"gpt-6.1-sol\"",
            "-c",
            "model_reasoning_effort=\"low\"",
        ]
    );
    // ~/.codex/AGENTS.md is not the configured file: Codex gets it here.
    std::fs::remove_file(f.home.join(".codex/AGENTS.md")).unwrap();
    let l = launch(&f.paths(), &s);
    assert_eq!(
        l.args[1],
        "developer_instructions=\"USER-RULE: fleet rules.\\n\\nSYSTEM-TEXT\""
    );
    // Ask: the owner's servers (and the auto-approved tool) and Awili's
    // connector server held to "writes".
    let s = LaunchSpec {
        connectors: Some("awili-connectors"),
        ..spec(Codex, AskFirst, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    let c: Vec<&str> = l
        .args
        .windows(2)
        .filter(|w| w[0] == "-c")
        .map(|w| w[1].as_str())
        .collect();
    for want in [
        "mcp_servers.mac-shell.default_tools_approval_mode=\"writes\"",
        "mcp_servers.mac-shell.tools.run_command.approval_mode=\"writes\"",
        "mcp_servers.awili-connectors.default_tools_approval_mode=\"writes\"",
    ] {
        assert!(c.contains(&want), "{want}: {c:?}");
    }
    assert!(!l
        .args
        .iter()
        .any(|a| a.contains("CODEX_HOME") || a == "--ignore-user-config"));
    // A project's own Codex layer is held too.
    std::fs::create_dir_all(f.project.join(".codex")).unwrap();
    std::fs::write(
        f.project.join(".codex/config.toml"),
        "[mcp_servers.proj-db]\ncommand = \"/bin/db\"\n[mcp_servers.proj-db.tools.drop]\napproval_mode = \"auto\"\n",
    )
    .unwrap();
    let l = launch(&f.paths(), &s);
    for want in [
        "mcp_servers.proj-db.default_tools_approval_mode=\"writes\"",
        "mcp_servers.proj-db.tools.drop.approval_mode=\"writes\"",
        "mcp_servers.mac-shell.tools.run_command.approval_mode=\"writes\"",
    ] {
        assert_eq!(
            l.args.iter().filter(|a| *a == want).count(),
            1,
            "{want}: {:?}",
            l.args
        );
    }
    assert_eq!(
        l.args
            .iter()
            .filter(|a| *a == "mcp_servers.awili-connectors.default_tools_approval_mode=\"writes\"")
            .count(),
        1,
        "{:?}",
        l.args
    );
}

/// RC1-M-FIX1: the in-app engine gets the owner's own MCP servers — his
/// Claude Code user-scope stdio servers, `~` expanded, as ordinary
/// `trust = "prompt"` servers (read-only tools run, others ask) — and his
/// memory server from `[work.agents.memory]` only when that file names none.
/// URL servers and programs that are missing or relative are left out.
#[test]
fn rc1m_fix1_owner_mcp_servers_are_his_stdio_servers() {
    let f = fx();
    let bin = f.home.join(".local/bin");
    std::fs::write(bin.join("mac-shell-mcp"), "#!/bin/sh\n").unwrap();
    std::fs::write(
        f.home.join(".claude.json"),
        serde_json::json!({ "mcpServers": {
            "mac-shell": { "type": "stdio", "command": "~/.local/bin/mac-shell-mcp", "args": ["-q"] },
            "interlock": { "command": bin.join("interlock-mcp").display().to_string(),
                           "env": { "INTERLOCK_URL": "http://127.0.0.1:1", "INTERLOCK_READER_TOKEN_FILE": "~/.config/t" } },
            "docs": { "type": "http", "url": "https://example.com/mcp" },
            "gone": { "command": "/nowhere/server" },
            "relative": { "command": "bin/server" }
        } })
        .to_string(),
    )
    .unwrap();
    let mem = owner_mem();
    let got = owner_mcp_servers(&f.home, &mem);
    let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(
        names.contains(&"mac-shell") && names.contains(&"interlock"),
        "{names:?}"
    );
    let ms = got.iter().find(|s| s.name == "mac-shell").unwrap();
    assert_eq!(ms.command, bin.join("mac-shell-mcp").display().to_string());
    assert_eq!(ms.args, vec!["-q".to_string()]);
    let il = got.iter().find(|s| s.name == "interlock").unwrap();
    assert_eq!(
        il.env
            .get("INTERLOCK_READER_TOKEN_FILE")
            .map(String::as_str),
        Some(f.home.join(".config/t").display().to_string().as_str())
    );
    for s in &got {
        assert_eq!(s.trust, awili_config::McpTrust::Prompt, "{}", s.name);
        assert!(
            s.enabled && s.modes.contains(&awili_config::McpMode::Work),
            "{}",
            s.name
        );
    }
    // No interlock in his file: the memory section's program is used.
    std::fs::write(f.home.join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
    let got = owner_mcp_servers(&f.home, &mem);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "interlock");
    assert_eq!(
        got[0].command,
        bin.join("interlock-mcp").display().to_string()
    );
    assert_eq!(
        got[0].env.get("INTERLOCK_URL").map(String::as_str),
        Some("http://127.0.0.1:1")
    );
    // Neither: nothing.
    assert!(owner_mcp_servers(&f.home, &AgentMemoryConfig::default()).is_empty());
}

/// RC1-M-FIX2 (RC1-M review P2): an instruction file is read only up to the
/// cap — a huge one (a sparse 16 GiB AGENTS.md here) is never read whole.
#[test]
fn rc1m_fix2_a_huge_instruction_file_is_read_only_to_the_cap() {
    let f = fx();
    let big = f.home.join("huge-AGENTS.md");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(16 * 1024 * 1024 * 1024).unwrap();
    std::fs::write(f.project.join("AGENTS.md"), "x").unwrap();
    let started = std::time::Instant::now();
    let user = user_agents_md(Some(big.to_str().unwrap()), &f.home).expect("read to the cap");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(user.text.len(), PROJECT_DOC_MAX_BYTES);
}

/// RC1-M-FIX6 (live 2026-10-03, claude 2.1.282 with a scripted model: the
/// approved step's `--resume` request carried the planning step's system
/// text byte for byte): Claude keeps a session's appended system prompt
/// across `--resume`, so the planning-step text leads the planning turn's
/// PROMPT and never rides in `--append-system-prompt`; a turn without
/// planning text has none in its prompt.
#[test]
fn rc1m_fix6_claude_planning_text_belongs_to_its_turn() {
    let f = fx();
    let system = format!("{}\n\nBRIEF-TEXT", awili_code::PLAN_FIRST_SYSTEM);
    let s = LaunchSpec {
        system: Some(&system),
        ..spec(Claude, AskFirst, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    // RC2-R4 (R3-F9b, R5-6): Claude's planning text also says to give the
    // plan in the reply, never in a plan file (deliberate test change).
    assert_eq!(
        l.prompt_system.as_deref(),
        Some(format!("{}\n{CLAUDE_PLAN_IN_REPLY}", awili_code::PLAN_FIRST_SYSTEM).as_str())
    );
    let appended = value_after(&l.args, "--append-system-prompt").unwrap_or_default();
    assert!(appended.contains("BRIEF-TEXT"), "{appended}");
    assert!(
        !l.args.join("\n").contains("read-only planning step"),
        "{:?}",
        l.args
    );
    let s = LaunchSpec {
        system: Some("BRIEF-TEXT"),
        ..spec(Claude, Auto, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    assert_eq!(l.prompt_system, None);
    assert!(value_after(&l.args, "--append-system-prompt")
        .unwrap_or_default()
        .contains("BRIEF-TEXT"));
}

/// RC1-M-FIX2 (live: the approved turn of a Codex planning step still
/// believed it was read-only, because Codex keeps a session's developer
/// instructions across `exec resume`): the planning-step text leads the
/// planning turn's PROMPT; `developer_instructions` carries only the stable
/// text; a turn without planning text has none in its prompt.
#[test]
fn rc1m_fix2_codex_planning_text_belongs_to_its_turn() {
    let f = fx();
    let system = format!("{}\n\nBRIEF-TEXT", awili_code::PLAN_FIRST_SYSTEM);
    let s = LaunchSpec {
        system: Some(&system),
        ..spec(Codex, AskFirst, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    assert_eq!(
        l.prompt_system.as_deref(),
        Some(awili_code::PLAN_FIRST_SYSTEM)
    );
    assert_eq!(
        value_after(&l.args, "-c"),
        Some("developer_instructions=\"BRIEF-TEXT\"")
    );
    assert!(
        !l.args.join("\n").contains("read-only planning step"),
        "{:?}",
        l.args
    );
    let s = LaunchSpec {
        system: Some("BRIEF-TEXT"),
        ..spec(Codex, Auto, &f.sub)
    };
    let l = launch(&f.paths(), &s);
    assert_eq!(l.prompt_system, None);
    assert_eq!(
        value_after(&l.args, "-c"),
        Some("developer_instructions=\"BRIEF-TEXT\"")
    );
}

#[test]
fn rc1m_toml_basic_string_escapes_everything_codex_parses() {
    assert_eq!(
        toml_basic_string("a\"b\\c\nd\te\u{1}"),
        "\"a\\\"b\\\\c\\nd\\te\\u0001\""
    );
    assert_eq!(toml_basic_string("x\u{7f}é"), "\"x\\u007Fé\"");
}

#[cfg(unix)]
/// GLM and DeepSeek have no system channel: Awili's text leads the prompt,
/// no flag is added (ZCode reads AGENTS.md itself).
/// RC1-M / RC1-M-FIX8 (whole-slice review P1): GLM (ZCode) and DeepSeek take
/// Awili's text as prompt text, and that text now carries the AGENTS.md
/// files they do not read themselves: DeepSeek reads none, so it gets the
/// user file and the whole project chain (root first); ZCode reads its own
/// `~/.zcode/AGENTS.md` and the nearest workspace `AGENTS.md`, so those are
/// left out.
#[test]
fn rc1m_glm_and_dsh_take_text_in_the_prompt() {
    let f = fx();
    let at = |s: &str, n: &str| s.find(n).unwrap_or_else(|| panic!("{n:?}: {s}"));
    for engine in [Glm, Dsh] {
        let l = launch(&f.paths(), &spec(engine, Auto, &f.sub));
        assert!(l.args.is_empty(), "{engine:?}: {:?}", l.args);
        let p = l.prompt_system.unwrap_or_default();
        assert!(at(&p, "USER-RULE") < at(&p, "ROOT-FACT"), "{engine:?}: {p}");
        assert!(
            at(&p, "ROOT-FACT") < at(&p, "SYSTEM-TEXT"),
            "{engine:?}: {p}"
        );
        assert!(!p.contains("CLAUDE-RULE"), "{p}");
        assert_eq!(
            p.contains("SUB-FACT"),
            engine == Dsh,
            "{engine:?}: ZCode reads the nearest AGENTS.md itself: {p}"
        );
        for doc in ["USER-RULE", "ROOT-FACT"] {
            assert_eq!(p.matches(doc).count(), 1, "{engine:?} {doc}: {p}");
        }
    }
    // ZCode's own user file is the owner's: not repeated.
    std::fs::create_dir_all(f.home.join(".zcode")).unwrap();
    std::os::unix::fs::symlink(
        f.home.join(".agents/AGENTS.md"),
        f.home.join(".zcode/AGENTS.md"),
    )
    .unwrap();
    let p = launch(&f.paths(), &spec(Glm, Auto, &f.sub))
        .prompt_system
        .unwrap_or_default();
    assert!(!p.contains("USER-RULE") && p.contains("ROOT-FACT"), "{p}");
    // RC1-M-FIX8 (pre-review P2): ZCode reads only `AGENTS.md`; where Awili
    // selects the folder's `AGENTS.override.md`, Awili sends the override
    // (ZCode also reads the shadowed `AGENTS.md` itself — its own behaviour).
    std::fs::write(f.sub.join("AGENTS.override.md"), "SUB-OVERRIDE\n").unwrap();
    let p = launch(&f.paths(), &spec(Glm, Auto, &f.sub))
        .prompt_system
        .unwrap_or_default();
    assert!(p.contains("SUB-OVERRIDE") && !p.contains("SUB-FACT"), "{p}");
    std::fs::remove_file(f.sub.join("AGENTS.override.md")).unwrap();
    // A workspace file that is the user file itself (an alias): ZCode reads
    // it as both, so Awili sends it as neither.
    std::fs::remove_file(f.sub.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(f.home.join(".agents/AGENTS.md"), f.sub.join("AGENTS.md")).unwrap();
    let p = launch(&f.paths(), &spec(Glm, Auto, &f.sub))
        .prompt_system
        .unwrap_or_default();
    assert!(!p.contains("USER-RULE") && p.contains("ROOT-FACT"), "{p}");
    std::fs::remove_file(f.sub.join("AGENTS.md")).unwrap();
    std::fs::write(f.sub.join("AGENTS.md"), "SUB-FACT\n").unwrap();
}

/// RC1-M-FIX8 (whole-slice review P1, the Code path): a GLM or DeepSeek turn
/// spawned through the shared `CodeProvider` (the Code tab's and the Work
/// agent lane's spawn path) carries the AGENTS.md files in its prompt.
#[test]
fn rc1m_fix8_spawned_glm_and_dsh_carry_agents_md() {
    if !crate::test_child::in_own_process(
        "agent_launch::tests::rc1m_fix8_spawned_glm_and_dsh_carry_agents_md",
    ) {
        return;
    }
    use crate::code_provider::CodeProvider;
    use awili_providers::{ChatMessage, ChatRequest};
    let f = fx();
    set_test_paths(Some(f.paths()));
    let dir = f.home.join("bins");
    std::fs::create_dir_all(&dir).unwrap();
    for (engine, name, out) in [
        (Glm, "zcode", r#"{"type":"result","result":"ok"}"#),
        (Dsh, "dsh", "ok"),
    ] {
        let bin = recorder(&dir, name, out);
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let p = CodeProvider::new(
            bin.display().to_string(),
            engine,
            Auto,
            f.sub.display().to_string(),
            None,
            None,
            slot,
        )
        .with_agent_memory(Some(owner_mem()));
        let (_c, rx) = std::sync::mpsc::channel();
        drain(p.run(
            ChatRequest {
                model: String::new(),
                system: Some("SYSTEM-TEXT".into()),
                messages: vec![ChatMessage::user("do the thing")],
            },
            rx,
        ));
        let argv = recorded(&dir, name);
        let all = argv.join("\n");
        assert!(
            all.contains("USER-RULE") && all.contains("ROOT-FACT"),
            "{engine:?}: {argv:?}"
        );
        assert_eq!(all.matches("USER-RULE").count(), 1, "{engine:?}: {argv:?}");
        assert!(all.contains("do the thing"), "{argv:?}");
        assert!(!all.contains("CLAUDE-RULE"), "{argv:?}");
    }
    set_test_paths(None);
}

#[test]
fn rc1m_apply_args_puts_flags_before_the_prompt() {
    let args: Vec<String> = ["-p", "x", "PROMPT"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let extra = vec!["--a".to_string(), "v".to_string()];
    assert_eq!(
        apply_args(args.clone(), &extra),
        vec!["-p", "x", "--a", "v", "PROMPT"]
    );
    assert_eq!(apply_args(args.clone(), &[]), args);
}

/// RC1-G3: the test-paths setter is process-wide state, so it refuses in the
/// shared test process.
#[test]
fn rc1g3_test_paths_refuse_a_shared_process() {
    let refused = std::panic::catch_unwind(|| set_test_paths(None));
    assert!(
        refused.is_err(),
        "set_test_paths must refuse in the shared test process"
    );
    assert!(TEST_PATHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_none());
    println!("RC1G3: set_test_paths refused in a shared process");
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn recorder(dir: &Path, name: &str, out: &str) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake CLI that writes its argv (NUL after each) and `CODEX_HOME` to
/// `<name>.rec`, then answers one turn.
fn recorder(dir: &Path, name: &str, out: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join(name);
    let rec = dir.join(format!("{name}.rec"));
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncat > {}.stdin\n{{ prev=\"\"; for a in \"$@\"; do if [ \"$prev\" = --append-system-prompt-file ]; then printf '%s\\0' \"$(cat \"$a\")\"; elif [ \"$a\" = --append-system-prompt-file ]; then printf '%s\\0' --append-system-prompt; else printf '%s\\0' \"$a\"; fi; prev=\"$a\"; done; \
             printf 'ENV CODEX_HOME=%s AUTO_MEM=%s\\0' \"$CODEX_HOME\" \"$CLAUDE_CODE_DISABLE_AUTO_MEMORY\"; }} > {}\n\
             printf '%s\\n' '{out}'\n",
            rec.display(),
            rec.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

/// argv, then (when the CLI was sent one) the prompt it read on stdin, then the
/// trailing ENV record.
fn recorded(dir: &Path, name: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_to_string(dir.join(format!("{name}.rec")))
        .unwrap()
        .split('\0')
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();
    let stdin = std::fs::read_to_string(dir.join(format!("{name}.rec.stdin"))).unwrap_or_default();
    if !stdin.is_empty() {
        let env = v.pop().unwrap();
        v = crate::test_stdin::argv_with_prompt(v, &stdin);
        v.push(env);
    }
    v
}

fn drain(rx: async_channel::Receiver<awili_providers::ProviderEvent>) {
    while let Ok(ev) = rx.recv_blocking() {
        if ev == awili_providers::ProviderEvent::Done {
            break;
        }
    }
}

/// Real spawns through `CodeProvider` (the Code tab and the Work agent lane):
/// the launch above is what the CLIs get; the prompt is the user's words alone
/// for Claude and Codex; no CODEX_HOME and no auto-memory switch are set.
#[test]
fn rc1m_spawned_launches() {
    if !crate::test_child::in_own_process("agent_launch::tests::rc1m_spawned_launches") {
        return;
    }
    use crate::code_provider::CodeProvider;
    use awili_providers::{ChatMessage, ChatRequest};
    let f = fx();
    set_test_paths(Some(f.paths()));
    let dir = f.home.join("bins");
    std::fs::create_dir_all(&dir).unwrap();
    let claude = recorder(
        &dir,
        "claude",
        r#"{"type":"result","subtype":"success","result":"ok"}"#,
    );
    let codex = recorder(&dir, "codex", r#"{"type":"turn.completed"}"#);
    let req = |system: Option<&str>| ChatRequest {
        model: String::new(),
        system: system.map(str::to_string),
        messages: vec![ChatMessage::user("do the thing")],
    };
    let cwd = f.sub.display().to_string();
    for (engine, bin, name) in [(Claude, &claude, "claude"), (Codex, &codex, "codex")] {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let p = CodeProvider::new(
            bin.display().to_string(),
            engine,
            Auto,
            cwd.clone(),
            None,
            None,
            slot,
        )
        .with_agent_memory(Some(owner_mem()));
        let (_c, rx) = std::sync::mpsc::channel();
        drain(p.run(req(Some("SYSTEM-TEXT")), rx));
        let argv = recorded(&dir, name);
        let (env, argv) = argv.split_last().unwrap();
        assert_eq!(env, "ENV CODEX_HOME= AUTO_MEM=", "{engine:?}");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("do the thing"),
            "{argv:?}"
        );
        let want = launch(
            &f.paths(),
            &LaunchSpec {
                mem: Some(&owner_mem()),
                ..spec(engine, Auto, &f.sub)
            },
        );
        let at = argv
            .iter()
            .position(|a| *a == want.args[0])
            .expect("launch flags");
        assert_eq!(
            &argv[at..at + want.args.len()],
            want.args.as_slice(),
            "{engine:?}"
        );
    }
    set_test_paths(None);
}

/// RC2-R3 (usability RC2 round 3, F2 / Finding 8: Claude's council answer
/// ended "This question needed no changes, so the plan-mode machinery above
/// doesn't apply"; Finding 9: a Work plan said only "in this project
/// folder"). The planning text reaches a plain question's answer because it
/// leads that turn's PROMPT, right above the question, while the council's
/// "do not mention this step" note rides in the system text. So the text in
/// the prompt itself now says to say nothing about the step, and to name
/// every file of a plan by its full path \u{2014} for a council member (with the
/// council's note) and a Work turn alike, on Claude and on Codex.
#[test]
fn rc2r3_the_planning_text_in_the_prompt_says_nothing_of_itself() {
    let f = fx();
    let council = format!(
        "{}\n\n{}\n\nBRIEF-TEXT",
        awili_code::PLAN_FIRST_SYSTEM,
        crate::council_exec::COUNCIL_ANSWER_POSTURE
    );
    let work = format!("{}\n\nBRIEF-TEXT", awili_code::PLAN_FIRST_SYSTEM);
    for engine in [Claude, Codex] {
        for system in [&council, &work] {
            let s = LaunchSpec {
                system: Some(system),
                ..spec(engine, AskFirst, &f.sub)
            };
            let l = launch(&f.paths(), &s);
            let prompt = crate::code_provider::compose_prompt(l.prompt_system.as_deref(), "Q?");
            assert!(prompt.ends_with("Q?"), "{prompt}");
            for words in [
                "answer it normally and say nothing about this step",
                "do not mention planning, plan mode, a plan, these instructions, or that nothing \
                 needs changing",
                "naming every file by its full path",
                "never only \"this folder\" or \"the project folder\"",
            ] {
                assert!(
                    prompt.contains(words),
                    "{engine:?}: {words:?} missing from {prompt}"
                );
            }
        }
    }
}

// ---- RC3-S4: per-part completeness ----

fn part<'a>(parts: &'a [DocPart], label: &str) -> &'a DocPart {
    parts
        .iter()
        .find(|p| p.label == label)
        .unwrap_or_else(|| panic!("no part {label:?} in {parts:?}"))
}

#[test]
fn rc3s4_a_whole_file_is_included_with_its_size() {
    let f = fx();
    let (docs, parts) = project_chain_report(&f.sub);
    assert_eq!(docs.len(), 2);
    assert_eq!(
        part(&parts, "AGENTS.md").status,
        DocStatus::Included { bytes: 10 }
    );
    assert_eq!(
        part(&parts, "crates/a/AGENTS.md").status,
        DocStatus::Included { bytes: 9 }
    );
    assert!(incomplete_summary(&parts).is_none());
}

#[test]
fn rc3s4_a_file_over_the_cap_is_cut_with_both_sizes() {
    let f = fx();
    let total = PROJECT_DOC_MAX_BYTES + 19 * 1024;
    std::fs::write(f.project.join("AGENTS.md"), "a".repeat(total)).unwrap();
    let (docs, parts) = project_chain_report(&f.project);
    assert_eq!(docs[0].text.len(), PROJECT_DOC_MAX_BYTES);
    assert_eq!(
        part(&parts, "AGENTS.md").status,
        DocStatus::Cut {
            kept: PROJECT_DOC_MAX_BYTES,
            total
        }
    );
    assert_eq!(
        incomplete_summary(&parts).unwrap(),
        "AGENTS.md was cut at 32 KB of 51 KB"
    );
}

#[test]
fn rc3s4_a_file_squeezed_out_by_the_shared_cap_says_so() {
    let f = fx();
    std::fs::write(
        f.project.join("AGENTS.md"),
        "a".repeat(PROJECT_DOC_MAX_BYTES),
    )
    .unwrap();
    let (docs, parts) = project_chain_report(&f.sub);
    assert_eq!(docs.len(), 1, "the nested file got no room");
    assert_eq!(
        part(&parts, "crates/a/AGENTS.md").status,
        DocStatus::LimitReached
    );
    assert_eq!(
        part(&parts, "crates/a/AGENTS.md").sentence(),
        "crates/a/AGENTS.md was left out: the 32 KB limit was already used by earlier files"
    );
    assert!(incomplete_summary(&parts).is_some());
}

#[cfg(unix)]
#[test]
fn rc3s4_a_link_out_of_the_project_is_not_read() {
    let f = fx();
    let outside = f.home.join("secret.md");
    std::fs::write(&outside, "SECRET-OUTSIDE\n").unwrap();
    std::fs::remove_file(f.sub.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(&outside, f.sub.join("AGENTS.md")).unwrap();
    let (docs, parts) = project_chain_report(&f.sub);
    assert!(docs.iter().all(|d| !d.text.contains("SECRET")), "{docs:?}");
    assert_eq!(
        part(&parts, "crates/a/AGENTS.md").status,
        DocStatus::OutsideFolder
    );
    assert!(incomplete_summary(&parts)
        .unwrap()
        .contains("points outside this folder"),);
    // The in-project root file is still read.
    assert!(docs.iter().any(|d| d.text.contains("ROOT-FACT")));
    // A link that stays inside the project is fine.
    std::fs::remove_file(f.sub.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(f.project.join("AGENTS.md"), f.sub.join("AGENTS.md")).unwrap();
    let (_, parts) = project_chain_report(&f.sub);
    assert!(matches!(
        part(&parts, "crates/a/AGENTS.md").status,
        DocStatus::Included { .. }
    ));
}

#[cfg(unix)]
#[test]
fn rc3s4_an_unreadable_or_dangling_file_is_reported() {
    let f = fx();
    std::fs::remove_file(f.sub.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(f.home.join("nope.md"), f.sub.join("AGENTS.md")).unwrap();
    let (_, parts) = project_chain_report(&f.sub);
    assert_eq!(
        part(&parts, "crates/a/AGENTS.md").status,
        DocStatus::Unreadable
    );
    // Not readable by permission (root reads anything: skip there).
    use std::os::unix::fs::PermissionsExt;
    let p = f.project.join("AGENTS.md");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::File::open(&p).is_ok();
    let (_, parts) = project_chain_report(&f.project);
    let status = part(&parts, "AGENTS.md").status.clone();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    if !readable {
        assert_eq!(status, DocStatus::Unreadable);
    }
}

#[test]
fn rc3s4_the_users_file_is_a_part_and_the_cli_may_own_it() {
    let f = fx();
    let (doc, p) = user_agents_md_report(None, &f.home);
    assert!(doc.is_some());
    assert_eq!(p.unwrap().label, "Your AGENTS.md");
    // Codex reads $CODEX_HOME/AGENTS.md (a link to the same file) itself.
    let parts = context_parts(&f.paths(), Some(Codex), Some(&f.sub), None);
    assert_eq!(
        part(&parts, "Your AGENTS.md").status,
        DocStatus::LeftToCli { cli: "Codex" }
    );
    assert!(
        incomplete_summary(&parts).is_none(),
        "by design, not a problem"
    );
    // The in-app engine gets it from Awili.
    let parts = context_parts(&f.paths(), None, Some(&f.sub), None);
    assert!(matches!(
        part(&parts, "Your AGENTS.md").status,
        DocStatus::Included { .. }
    ));
    // A user file over the cap is cut.
    let big = f.home.join("big.md");
    std::fs::write(&big, "b".repeat(PROJECT_DOC_MAX_BYTES + 5)).unwrap();
    let (_, p) = user_agents_md_report(Some(big.to_str().unwrap()), &f.home);
    assert_eq!(
        p.unwrap().status,
        DocStatus::Cut {
            kept: PROJECT_DOC_MAX_BYTES,
            total: PROJECT_DOC_MAX_BYTES + 5
        }
    );
}

#[test]
fn rc3s4_claude_md_is_not_a_part() {
    let f = fx();
    std::fs::write(f.sub.join("CLAUDE.local.md"), "x").unwrap();
    for engine in [Claude, Codex] {
        let parts = context_parts(&f.paths(), Some(engine), Some(&f.sub), None);
        assert!(
            parts.iter().all(|p| !p.label.contains("CLAUDE")),
            "{parts:?}"
        );
    }
}

#[test]
fn rc3s4_every_status_has_a_plain_sentence() {
    let s = |status| {
        DocPart {
            label: "AGENTS.md".into(),
            status,
        }
        .sentence()
    };
    assert_eq!(
        s(DocStatus::Included { bytes: 100 }),
        "AGENTS.md was included (100 bytes)"
    );
    assert_eq!(
        s(DocStatus::Cut {
            kept: 32768,
            total: 52224
        }),
        "AGENTS.md was cut at 32 KB of 51 KB"
    );
    assert!(s(DocStatus::OutsideFolder).contains("outside this folder"));
    assert_eq!(
        s(DocStatus::LeftToCli { cli: "Codex" }),
        "AGENTS.md is left to Codex"
    );
    assert_eq!(s(DocStatus::Unreadable), "AGENTS.md could not be read");
    assert!(DocStatus::LimitReached.is_incomplete());
    assert!(!DocStatus::LeftToCli { cli: "x" }.is_incomplete());
}
