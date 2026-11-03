//! RC1-M proof tests for the Work-side pieces: the task prefix (no CLAUDE.md;
//! the AGENTS.md chain only for the lanes that cannot read it), the in-app
//! system prompt order, and which lanes leave memory to the owner's hooks.

use super::*;
use awili_persist::StoredProject;

fn folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
    std::fs::create_dir_all(folder.join(".git")).unwrap();
    std::fs::write(folder.join("AGENTS.md"), "PROJECT-AGENTS-FACT\n").unwrap();
    std::fs::write(folder.join("CLAUDE.md"), "PROJECT-CLAUDE-RULE\n").unwrap();
    (tmp, folder)
}

/// RC1-M / RC1-M-FIX8 (whole-slice review P1): the task prefix never reads a
/// CLAUDE.md; for the lane that gets AGENTS.md nowhere else (the Hermes
/// gateway) it carries the bounded user file, then the project chain, then
/// the saved project's field; for every other lane only the field.
#[test]
fn rc1m_task_prefix_never_reads_claude_md() {
    let (_tmp, f) = folder();
    let user = f.parent().unwrap().join("user-AGENTS.md");
    std::fs::write(&user, "USER-RULE: fleet.\n").unwrap();
    let user_md = user.display().to_string();
    let prefix = |projects: &[StoredProject], in_task: bool| {
        folder_instructions_prefix(projects, Some(&f), in_task, Some(&user_md))
    };
    // RC1-M-FIX8 (pre-review P2): with no folder the gateway still gets the
    // user file.
    let bare = folder_instructions_prefix(&[], None, true, Some(&user_md)).unwrap();
    assert!(
        bare.contains("USER-RULE") && !bare.contains("PROJECT-AGENTS-FACT"),
        "{bare}"
    );
    assert_eq!(
        folder_instructions_prefix(&[], None, false, Some(&user_md)),
        None
    );
    // Claude, Codex, ZCode, DeepSeek and the in-app engine get AGENTS.md
    // from their launch or system prompt: nothing is added for them.
    assert_eq!(prefix(&[], false), None);
    // The Hermes gateway gets the user file and the chain in the task.
    let p = prefix(&[], true).unwrap();
    assert!(p.starts_with("[Folder instructions]\n"), "{p}");
    assert!(
        p.find("USER-RULE").unwrap() < p.find("PROJECT-AGENTS-FACT").unwrap(),
        "{p}"
    );
    assert_eq!(p.matches("USER-RULE").count(), 1, "{p}");
    assert!(!p.contains("PROJECT-CLAUDE-RULE"), "{p}");
    // The saved project's own field is kept for every lane, after the files.
    let proj = StoredProject {
        id: 1,
        name: "P".into(),
        folder: f.display().to_string(),
        instructions: "FIELD-RULE".into(),
        created_ts: 0,
    };
    let p = prefix(std::slice::from_ref(&proj), false).unwrap();
    assert_eq!(p, "[Project: P]\nFIELD-RULE");
    let p = prefix(&[proj], true).unwrap();
    assert!(
        p.find("PROJECT-AGENTS-FACT").unwrap() < p.find("FIELD-RULE").unwrap(),
        "{p}"
    );
}

#[test]
fn rc1m_inapp_system_is_memory_then_agents_md_then_brief() {
    let (tmp, f) = folder();
    let user = tmp.path().join("user.md");
    std::fs::write(&user, "USER-AGENTS-RULE\n").unwrap();
    let s = inapp_system(
        Some("POLICY".into()),
        true,
        Some(&f),
        Some(user.to_str().unwrap()),
        Some(tmp.path()),
        None,
    )
    .unwrap();
    let at = |n: &str| s.find(n).unwrap_or_else(|| panic!("{n:?}: {s}"));
    assert!(s.starts_with("POLICY\n\n"), "{s}");
    assert!(at("USER-AGENTS-RULE") < at("PROJECT-AGENTS-FACT"), "{s}");
    assert!(
        at("PROJECT-AGENTS-FACT") < at("Your file tools work in"),
        "{s}"
    );
    assert!(!s.contains("PROJECT-CLAUDE-RULE"), "{s}");
    // No memory, no files: the brief alone.
    let bare = inapp_system(None, false, None, Some(""), None, None).unwrap();
    assert_eq!(bare, crate::agent_brief::owned_engine_brief(""));
}

/// RC1-M-FIX7 (lead ruling, 2026-10-03; RC1-M-FIX6 review P1 x2): only the
/// owner's explicit `[work.agents.memory] hooks_provide_recall = true` makes
/// Awili leave the recall to his Claude and Codex hooks (RC1-N-FIX1: the key
/// is `hooks_provide_memory`, the old one an alias, and the directives go
/// too). The default, a
/// household install, and every other engine keep Awili's recall; hook files
/// are not read (a configured `UserPromptSubmit` hook alone changes nothing).
#[test]
fn rc1m_only_the_owners_claude_and_codex_leave_memory_to_hooks() {
    if !crate::test_child::in_own_process(
        "work::rc1m_tests::rc1m_only_the_owners_claude_and_codex_leave_memory_to_hooks",
    ) {
        return;
    }
    // A hook in the launch home must not matter.
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude/settings.json"),
        serde_json::json!({ "hooks": { "UserPromptSubmit": [
            { "hooks": [ { "type": "command", "command": "memory-hook" } ] } ] } })
        .to_string(),
    )
    .unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: home.join(".codex"),
        home,
    }));
    let parse = |toml: &str| {
        awili_config::Config::parse(toml)
            .unwrap()
            .work
            .agents
            .memory
    };
    let off = parse("[work.agents.memory]\n").expect("the section");
    assert!(!off.hooks_provide_memory, "the default is off");
    // RC1-N-FIX1: the new key, and the old one as an accepted alias (the
    // owner's config keeps working).
    let on = parse("[work.agents.memory]\nhooks_provide_memory = true\n").expect("the section");
    assert!(on.hooks_provide_memory);
    let alias = parse("[work.agents.memory]\nhooks_provide_recall = true\n").expect("the section");
    assert!(alias.hooks_provide_memory, "the old key is an alias");
    for engine in [CodeEngine::Claude, CodeEngine::Codex] {
        assert!(
            !hooks_bring_memory(engine, Some(&off)),
            "default: {engine:?}"
        );
        assert!(
            hooks_bring_memory(engine, Some(&on)),
            "owner says so: {engine:?}"
        );
        assert!(!hooks_bring_memory(engine, None), "household {engine:?}");
    }
    for engine in [CodeEngine::Glm, CodeEngine::Dsh] {
        assert!(!hooks_bring_memory(engine, Some(&on)), "{engine:?}");
    }
    crate::agent_launch::set_test_paths(None);
}

/// RC1-M-FIX6 (audit production P1), superseded by RC1-N-FIX1 (lead
/// ruling 2026-10-04): the Chat gate pins Awili's preamble whole — the
/// standing directives (a directive may span lines) and the recall — or,
/// where the backend's own hooks bring the owner's memory, pins nothing; the
/// lane text stays either way. Decided before pinning, never by parsing the
/// rendered preamble back out (the old recall-only `without_recall` is gone).
#[test]
fn rc1m_fix6_without_recall_keeps_the_policy() {
    use awili_providers::{ChatProvider, ChatRequest, ProviderEvent};
    use futures::StreamExt;
    struct Seen(std::sync::Arc<std::sync::Mutex<Vec<ChatRequest>>>, bool);
    impl ChatProvider for Seen {
        fn stream(&self, req: ChatRequest) -> futures::stream::BoxStream<'static, ProviderEvent> {
            self.0.lock().unwrap().push(req);
            futures::stream::iter(vec![ProviderEvent::Done]).boxed()
        }
        fn memory_from_hooks(&self) -> bool {
            self.1
        }
    }
    let dirs = [
        awili_memory::Directive {
            id: None,
            text: "RULE-ONE".into(),
            priority: 100,
        },
        awili_memory::Directive {
            id: None,
            text: "RULE-TWO first line\n\nRULE-TWO after a blank line".into(),
            priority: 90,
        },
    ];
    let recall = [awili_memory::RecallEntry {
        id: None,
        text: "RECALLED".into(),
        category: None,
        priority: 50,
    }];
    let both = awili_memory::try_system_preamble(&dirs, &recall, None)
        .unwrap()
        .unwrap();
    for hooks in [false, true] {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Seen(seen.clone(), hooks);
        let req = ChatRequest {
            model: String::new(),
            system: Some("LANE".into()),
            messages: vec![awili_providers::ChatMessage::user("q")],
        };
        let mut stream = crate::recall::open_gated_stream(&provider, req, &Ok(Some(both.clone())));
        futures::executor::block_on(async { while stream.next().await.is_some() {} });
        let system = seen.lock().unwrap()[0].system.clone().unwrap_or_default();
        for part in [
            "RULE-ONE",
            "RULE-TWO first line",
            "RULE-TWO after a blank line",
            "RECALLED",
        ] {
            assert_eq!(
                system.contains(part),
                !hooks,
                "hooks={hooks} {part}: {system}"
            );
        }
        assert!(system.ends_with("LANE"), "the lane text stays: {system}");
    }
}

/// RC1-M-FIX1 (owner, 2026-10-03: "any model … full capability"): on the
/// owner's install (`[work.agents.memory]` present) the in-app engine's Work
/// catalog carries his own MCP servers (here his `~/.claude.json` user scope
/// names the test server as `interlock`), and the FIRST turn open of a new
/// worker already has their tools — it waits for every configured server,
/// not only the reserved one (live, before the fix: Gemma's turns had
/// mac-shell's tools and none of interlock's). A household config (no
/// memory section) gets none of his servers.
#[test]
fn rc1m_fix1_owner_servers_reach_the_first_in_app_turn() {
    if !crate::test_child::in_own_process(
        "work::rc1m_tests::rc1m_fix1_owner_servers_reach_the_first_in_app_turn",
    ) {
        return;
    }
    // Build the stdio test server (once per test binary) and find it.
    // RC2-R4: the testserver copy no parallel build rewrites (see
    // `mcp_runtime::tests::testserver_bin`).
    let server = crate::mcp_runtime::tests::testserver_bin();
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(
        home.join(".claude.json"),
        serde_json::json!({ "mcpServers": {
            "interlock": { "type": "stdio", "command": server.display().to_string(), "args": [] },
            "web-docs": { "type": "http", "url": "https://example.com/mcp" }
        } })
        .to_string(),
    )
    .unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: home.join(".codex"),
        home: home.clone(),
    }));
    let household = awili_config::Config::parse("").unwrap();
    assert!(
        AwiliApp::compose_work_mcp_from("/tmp", &household)
            .unwrap()
            .is_none(),
        "a household install gets none of the owner's servers"
    );
    let owner = awili_config::Config::parse("[work.agents.memory]\n").unwrap();
    let mut mcp = AwiliApp::compose_work_mcp_from("/tmp", &owner)
        .unwrap()
        .expect("the owner's servers compose");
    let (tools, _roots, degradations, _seed, required_missing) = (mcp.turn_open)();
    let origins: Vec<&str> = tools
        .iter()
        .map(|t| t.identity.origin.origin.as_str())
        .collect();
    assert!(
        origins.contains(&"mcp:interlock"),
        "the first turn has the owner's server's tools: {origins:?} {degradations:?}"
    );
    assert!(
        !origins.iter().any(|o| o.contains("web-docs")),
        "{origins:?}"
    );
    assert!(required_missing.is_empty(), "{required_missing:?}");
    crate::agent_launch::set_test_paths(None);
}

/// RC1-M-FIX1: the in-app engine's memory preamble is on for the owner's
/// install (his `[work.agents.memory]`) even with an empty `[memory]
/// base_url` — the in-app equivalent of his Claude/Codex memory hooks;
/// elsewhere `[memory]` decides as before.
#[test]
fn rc1m_fix1_owner_install_turns_the_in_app_memory_preamble_on() {
    let off = crate::MemoryCfg {
        enabled: false,
        base_url: String::new(),
        token: String::new(),
        min_priority: 90,
        recall: true,
        write: false,
    };
    let on = crate::MemoryCfg {
        enabled: true,
        ..off.clone()
    };
    let owner = awili_config::AgentMemoryConfig::default();
    assert!(inapp_memory_on(&off, Some(&owner)));
    assert!(!inapp_memory_on(&off, None));
    assert!(inapp_memory_on(&on, None));
}

/// RC1-M-FIX1: with the memory preamble present the in-app system text says
/// it is the bootstrap (no bootstrap tool call); without it, it does not.
/// The in-app brief carries the plan rule; the Claude/Codex brief does not.
#[test]
fn rc1m_fix1_in_app_text_names_the_bootstrap_and_the_plan_rule() {
    let with = inapp_system(Some("POLICY".into()), true, None, Some(""), None, None).unwrap();
    assert!(
        with.ends_with(crate::agent_brief::OWNED_MEMORY_LINE),
        "{with}"
    );
    let without = inapp_system(None, false, None, Some(""), None, None).unwrap();
    assert!(
        !without.contains(crate::agent_brief::OWNED_MEMORY_LINE),
        "{without}"
    );
    assert!(
        without.contains(crate::agent_brief::OWNED_PLAN_RULE),
        "{without}"
    );
    assert!(!crate::agent_brief::BRIEF.contains("concrete numbered plan"));
}

/// RC1-M-FIX3 (RC1-M-FIX1 review P1, Codex gpt-6-sol on c9bfe05): an Awili
/// `[[mcp.servers]]` entry the owner switched off (`enabled = false`), or
/// keeps out of Work, is his explicit opt-out: the server of that name in his
/// `~/.claude.json` is not brought back.
#[test]
fn rc1m_fix3_an_opted_out_server_is_not_reimported() {
    if !crate::test_child::in_own_process(
        "work::rc1m_tests::rc1m_fix3_an_opted_out_server_is_not_reimported",
    ) {
        return;
    }
    // RC2-R4: the testserver copy no parallel build rewrites (see
    // `mcp_runtime::tests::testserver_bin`).
    let server = crate::mcp_runtime::tests::testserver_bin();
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(
        home.join(".claude.json"),
        serde_json::json!({ "mcpServers": {
            "interlock": { "type": "stdio", "command": server.display().to_string() },
            "mac-shell": { "type": "stdio", "command": server.display().to_string() }
        } })
        .to_string(),
    )
    .unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: home.join(".codex"),
        home: home.clone(),
    }));
    let s = server.display();
    for (case, entry) in [
        (
            "switched off",
            format!("name = \"interlock\"\ncommand = \"{s}\"\nenabled = false\n"),
        ),
        (
            "not in Work",
            format!("name = \"interlock\"\ncommand = \"{s}\"\nmodes = [\"chat\"]\n"),
        ),
    ] {
        let cfg = awili_config::Config::parse(&format!(
            "[work.agents.memory]\n\n[[mcp.servers]]\n{entry}"
        ))
        .unwrap();
        let mut mcp = AwiliApp::compose_work_mcp_from("/tmp", &cfg)
            .unwrap()
            .expect("the other owner server composes");
        // RC2-APP (the RUN batch's flake): under a loaded machine the
        // required server (mac-shell) can take longer than one turn open's
        // bounded wait to come up. Open the turn again until it is there
        // (bounded), checking the opt-out on EVERY open.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let (tools, _r, degradations, _s, missing) = (mcp.turn_open)();
            let origins: Vec<&str> = tools
                .iter()
                .map(|t| t.identity.origin.origin.as_str())
                .collect();
            assert!(
                !origins.contains(&"mcp:interlock"),
                "{case}: {origins:?} {degradations:?}"
            );
            if missing.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{case}: still missing after 90 s: {missing:?} {degradations:?}"
            );
        }
    }
    crate::agent_launch::set_test_paths(None);
}

/// RC1-M-FIX8 (pre-review P1): a local Build's request — the bytes the fake
/// runner receives on its stdin — carries the bounded user AGENTS.md and the
/// project chain once each, then the task.
#[test]
fn rc1m_fix8_local_build_request_carries_agents_md() {
    use crate::build_dispatch::{build_request, ResolvedBuild};
    use crate::build_dispatch_tests::{fake_ssh, input, snapshot, targets, SCRIPT_FIXTURE};
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let (tmp, f) = folder();
    let user = f.parent().unwrap().join("user-AGENTS.md");
    std::fs::write(&user, "USER-RULE: fleet.\n").unwrap();
    let spec = local_build_spec("BUILD-TASK", Some(&f), Some(&user.display().to_string()));
    let runner = tmp.path().join("runner");
    std::fs::create_dir_all(&runner).unwrap();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(&runner, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", spec)).unwrap();
    std::fs::write(
        runner.join("snapshot.json"),
        serde_json::to_vec(&snapshot(
            &request,
            awili_dispatch::protocol::BuildState::Queued,
        ))
        .unwrap(),
    )
    .unwrap();
    resolved.submit(&request).unwrap();
    let sent: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(runner.join("last-request")).unwrap())
            .unwrap();
    let text = sent["spec_text"].as_str().expect("the spec on the wire");
    assert_eq!(text.matches("USER-RULE").count(), 1, "{text}");
    assert_eq!(text.matches("PROJECT-AGENTS-FACT").count(), 1, "{text}");
    assert!(!text.contains("PROJECT-CLAUDE-RULE"), "{text}");
    assert!(text.ends_with("BUILD-TASK"), "{text}");
    assert!(text.find("USER-RULE").unwrap() < text.find("PROJECT-AGENTS-FACT").unwrap());
}

/// RC1-N item 1 (round 4 M3 and 4b): the in-app engine is never offered
/// the memory server's `bootstrap` tool (lead: "don't offer ... at all") —
/// neither on the owner's install (his `interlock` from `~/.claude.json`)
/// nor when a config names an `interlock` server itself with memory off.
/// Its other tools stay, and another server's `bootstrap` is offered.
#[test]
fn rc1n_the_in_app_engine_is_never_offered_memory_bootstrap() {
    if !crate::test_child::in_own_process(
        "work::rc1m_tests::rc1n_the_in_app_engine_is_never_offered_memory_bootstrap",
    ) {
        return;
    }
    // RC2-R4: the testserver copy no parallel build rewrites (see
    // `mcp_runtime::tests::testserver_bin`).
    let server = crate::mcp_runtime::tests::testserver_bin();
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(
        home.join(".claude.json"),
        serde_json::json!({ "mcpServers": {
            "interlock": {
                "type": "stdio",
                "command": server.display().to_string(),
                "args": [],
                "env": { "MCP_EXTRA_TOOL": "bootstrap" }
            }
        } })
        .to_string(),
    )
    .unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: home.join(".codex"),
        home: home.clone(),
    }));
    let names = |cfg: &awili_config::Config, origin: &str| -> Vec<String> {
        let mut mcp = AwiliApp::compose_work_mcp_from("/tmp", cfg)
            .unwrap()
            .expect("the servers compose");
        let (tools, _roots, degradations, _seed, _missing) = (mcp.turn_open)();
        let of: Vec<String> = tools
            .iter()
            .filter(|t| t.identity.origin.origin == origin)
            .map(|t| t.identity.name.clone())
            .collect();
        assert!(!of.is_empty(), "{origin}'s tools arrived: {degradations:?}");
        of
    };
    let owner = awili_config::Config::parse("[work.agents.memory]\n").unwrap();
    let offered = names(&owner, "mcp:interlock");
    assert!(!offered.iter().any(|n| n == "bootstrap"), "{offered:?}");
    assert!(offered.iter().any(|n| n == "echo"), "{offered:?}");
    let own = format!(
        "[memory]\nbase_url = \"\"\n\
         [[mcp.servers]]\nname = \"interlock\"\ncommand = \"{s}\"\nmodes = [\"work\"]\n\
         [mcp.servers.env]\nMCP_EXTRA_TOOL = \"bootstrap\"\n\
         [[mcp.servers]]\nname = \"other\"\ncommand = \"{s}\"\nmodes = [\"work\"]\n\
         [mcp.servers.env]\nMCP_EXTRA_TOOL = \"bootstrap\"\n",
        s = server.display()
    );
    let no_memory = awili_config::Config::parse(&own).unwrap();
    let offered = names(&no_memory, "mcp:interlock");
    assert!(!offered.iter().any(|n| n == "bootstrap"), "{offered:?}");
    let other = names(&no_memory, "mcp:other");
    assert!(other.iter().any(|n| n == "bootstrap"), "{other:?}");
    crate::agent_launch::set_test_paths(None);
}

/// RC2-ENGG3 (RC2-ENGALL slice review P3): a server's own tool description
/// reaches the model request the production way, end to end — the in-repo
/// MCP test server advertises `echo`, the runtime captures the
/// advertisement, the Work catalog projects it, the in-app engine worker
/// opens a turn with it, and the request it sends carries the server's
/// words. (The engine-side test starts at the catalog with a hand-built
/// advertisement, so it could not see capture stop copying descriptions.)
#[test]
fn rc2engg3_a_servers_own_description_reaches_the_model_request() {
    if !crate::test_child::in_own_process(
        "work::rc1m_tests::rc2engg3_a_servers_own_description_reaches_the_model_request",
    ) {
        return;
    }
    // RC2-R4: the testserver copy no parallel build rewrites (see
    // `mcp_runtime::tests::testserver_bin`).
    let server = crate::mcp_runtime::tests::testserver_bin();
    let cfg = awili_config::Config::parse(&format!(
        "[memory]\nbase_url = \"\"\n\
         [[mcp.servers]]\nname = \"probe\"\ncommand = \"{}\"\nmodes = [\"work\"]\n",
        server.display()
    ))
    .unwrap();
    let mcp = AwiliApp::compose_work_mcp_from("/tmp", &cfg)
        .unwrap()
        .expect("the test server composes");

    /// The model side: records each request, answers with one short reply.
    #[derive(Clone, Default)]
    struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
    impl awili_seam::Transport for Recorder {
        fn stream(&mut self, request_body: &str) -> Result<String, awili_seam::SeamError> {
            self.0.lock().unwrap().push(request_body.to_string());
            let chunk = |p: &str| format!("data: {p}\n\n");
            Ok([
                chunk(r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"done"},"finish_reason":null}]}"#),
                chunk(r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#),
                chunk(r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#),
                "data: [DONE]\n\n".to_string(),
            ]
            .concat())
        }
    }
    let model = Recorder::default();
    let dir = tempfile::tempdir().unwrap();
    let engine_cfg = awili_engine_worker::AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        dir.path().join("b2.jsonl"),
        dir.path().join("grants.jsonl"),
    );
    let authority = awili_engine_worker::authority().expect("worker authority");
    let worker = awili_engine_worker::AwiliEngineWorker::with_transport(
        engine_cfg,
        authority,
        Box::new(model.clone()),
    )
    .unwrap()
    .with_mcp(mcp);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let handle = awili_work::spawn_worker(
        Box::new(worker),
        awili_work::WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    cmd_tx
        .send(awili_work::WorkCmd::Prompt {
            text: "what can the probe do?".into(),
            system: None,
        })
        .unwrap();
    loop {
        match events.recv_blocking().expect("worker alive") {
            awili_work::WorkEvent::TurnComplete => break,
            awili_work::WorkEvent::Failed(m) => panic!("the turn failed: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    handle.join().unwrap();

    let sent = model.0.lock().unwrap().clone();
    let req: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    let tools = req["tools"].as_array().expect("tools advertised");
    let echo = tools
        .iter()
        .find(|t| t["function"]["name"] == "echo")
        .unwrap_or_else(|| panic!("the server's echo is offered: {tools:?}"));
    assert_eq!(
        echo["function"]["description"], "Echo the text argument back.",
        "{echo}"
    );
}

/// RC2-GEMMA2 (R7-speed): the in-app system text the engine pins is the
/// same bytes whatever was recalled (or when nothing was): the recalled
/// notes split off into the user's message, and what stays — rules,
/// AGENTS.md, brief — is what the model server caches per folder. The
/// memory line says where the notes now are and names the words Gemma
/// copied (round 5 Minor 2).
#[test]
fn rc2gemma2_the_in_app_system_is_the_same_bytes_whatever_was_recalled() {
    let (_tmp, f) = folder();
    let dirs = [awili_memory::Directive {
        id: None,
        text: "RULE-ONE".into(),
        priority: 100,
    }];
    let note = |text: &str| awili_memory::RecallEntry {
        id: None,
        text: text.into(),
        category: None,
        priority: 50,
    };
    let pinned = |recall: &[awili_memory::RecallEntry]| {
        let policy = awili_memory::try_system_preamble(&dirs, recall, None).unwrap();
        let system = inapp_system(policy, true, Some(&f), Some(""), None, None).unwrap();
        awili_seam::split_past_context(&system)
    };
    let (a, block_a) = pinned(&[note("NOTE-A")]);
    let (b, block_b) = pinned(&[note("NOTE-B"), note("NOTE-C")]);
    let (none, block_none) = pinned(&[]);
    assert_eq!(a, b, "a different recall leaves the system text unchanged");
    assert_eq!(a, none, "no recall: the same bytes");
    assert!(block_a.is_some_and(|b| b.contains("NOTE-A")));
    assert!(block_b.is_some_and(|b| b.contains("NOTE-B") && b.contains("NOTE-C")));
    assert!(block_none.is_none());
    for part in ["RULE-ONE", "PROJECT-AGENTS-FACT"] {
        assert!(a.contains(part), "{part} stays in the system text: {a}");
    }
    assert!(!a.contains("NOTE-"), "{a}");
    assert!(a.ends_with(crate::agent_brief::OWNED_MEMORY_LINE), "{a}");
    let line = crate::agent_brief::OWNED_MEMORY_LINE;
    assert!(line.contains("open the user's message"), "{line}");
    assert!(!line.contains("at the top are"), "{line}");
    for word in ["standing directives", "past context", "interlock", "tunnel"] {
        assert!(line.contains(&format!("\"{word}\"")), "{word}: {line}");
    }
}

/// RC2-FIXA2 (RC2-GEMMA review P2): with memory on and NO standing
/// directives, a turn with recalled notes and a turn without any pin the
/// same system bytes (the model server's cache holds), and both say where
/// the memory is; with memory off the line is not there.
#[test]
fn rc2fixa_no_directives_the_system_is_the_same_bytes_with_and_without_recall() {
    let (_tmp, f) = folder();
    let note = awili_memory::RecallEntry {
        id: None,
        text: "NOTE-A".into(),
        category: None,
        priority: 50,
    };
    let pinned = |recall: &[awili_memory::RecallEntry], memory_on: bool| {
        let policy = awili_memory::try_system_preamble(&[], recall, None).unwrap();
        let system = inapp_system(policy, memory_on, Some(&f), Some(""), None, None).unwrap();
        awili_seam::split_past_context(&system)
    };
    let (with, block) = pinned(std::slice::from_ref(&note), true);
    let (without, no_block) = pinned(&[], true);
    assert!(block.is_some_and(|b| b.contains("NOTE-A")));
    assert!(no_block.is_none());
    assert_eq!(with, without, "recall or not: the same bytes");
    assert!(
        with.ends_with(crate::agent_brief::OWNED_MEMORY_LINE),
        "{with}"
    );
    let (off, _) = pinned(&[], false);
    assert!(
        !off.contains(crate::agent_brief::OWNED_MEMORY_LINE),
        "{off}"
    );
}

/// RC2-GEMMA7 (round 5 Minor 2, S-O3): the in-app brief turns unknowns into
/// stated assumptions instead of a list of questions, and says how to run a
/// shell command in the Work folder.
#[test]
fn rc2gemma7_the_brief_asks_for_a_plan_and_names_the_working_directory() {
    let brief = crate::agent_brief::owned_engine_brief("/p");
    // The unknowns-become-assumptions rule was reworded (a84b1475, "distinguish
    // action requests from plan-only replies") into `OWNED_PLAN_RULE`; the
    // sentence this test quoted has not been in the brief since.
    assert!(brief.contains(crate::agent_brief::OWNED_PLAN_RULE));
    assert!(brief.contains("stating assumptions as steps"));
    assert!(brief.contains("Do not reply with only questions"));
    assert!(brief.contains("A shell command starts in the home folder: to run it here, give /p as its working directory."));
}

/// RC2-R59 (usability RC2 round 4, Major 2: each in-app turn waited about
/// 11 s before its request left while the owner's LightCraft server, which
/// could not start, was started again every 150 ms): a configured server
/// that dies at start is started once per turn open and not waited for —
/// the turn does not wait the whole 10 s for it.
#[test]
fn rc2r59_a_server_that_cannot_start_is_not_waited_for() {
    if !crate::test_child::in_own_process(
        "work::rc1m_tests::rc2r59_a_server_that_cannot_start_is_not_waited_for",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let starts = home.join("starts");
    let broken = home.join("broken-server");
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(
        &broken,
        &format!(
            "#!/bin/sh\necho start >> '{}'\nsleep 0.05\necho 'library already open' >&2\nexit 1\n",
            starts.display()
        ),
        0o755,
    );
    std::fs::write(
        home.join(".claude.json"),
        serde_json::json!({ "mcpServers": {
            "lightcraft": { "type": "stdio", "command": broken.display().to_string(), "args": [] }
        } })
        .to_string(),
    )
    .unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: home.join(".codex"),
        home: home.clone(),
    }));
    let owner = awili_config::Config::parse("[work.agents.memory]\n").unwrap();
    let mut mcp = AwiliApp::compose_work_mcp_from("/tmp", &owner)
        .unwrap()
        .expect("the owner's server composes");
    let (tools, _roots, _degradations, _seed, _missing) = (mcp.turn_open)();
    assert!(tools.is_empty(), "it never came up");
    let count = std::fs::read_to_string(&starts)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert!(
        count <= 1 + super::TURN_OPEN_RESTARTS,
        "started {count} times in one turn open"
    );
    crate::agent_launch::set_test_paths(None);
}

/// RC3-S1a: the in-app system text names the configured model, so a local
/// model does not claim to be Claude; with no name the text is unchanged.
#[test]
fn rc3_s1a_inapp_system_names_the_configured_model() {
    let named = inapp_system(None, false, None, Some(""), None, Some("Gemma 4 26B")).unwrap();
    assert!(named.contains("You are Gemma 4 26B"), "{named}");
    assert!(
        named.contains("do not claim to be a different model"),
        "{named}"
    );
    let bare = inapp_system(None, false, None, Some(""), None, None).unwrap();
    assert!(!bare.contains("You are "), "{bare}");
    let blank = inapp_system(None, false, None, Some(""), None, Some("  ")).unwrap();
    assert_eq!(blank, bare);
}

/// RC3-S1a: the brief tells every agent to look in the folder before saying
/// it cannot tell.
#[test]
fn rc3_s1a_brief_says_look_in_the_folder_first() {
    let line =
        "look with your tools (list the folder, read the file) before you say you cannot tell";
    assert!(crate::agent_brief::BRIEF.contains(line));
    assert!(crate::agent_brief::owned_engine_brief("/w").contains(line));
}

