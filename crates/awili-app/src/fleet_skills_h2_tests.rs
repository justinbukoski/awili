//! RC1-H2 proofs (RC1 round 3, Major 2): fleet recipes reach a Work turn only
//! in the owner's fleet folders (a git checkout on the configured Gitea host)
//! and never the in-app engine. A household question about "memory" in an
//! ordinary folder no longer gets "Fleet skills matched … memory-provenance
//! … Interlock" pasted into its prompt.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use gpui::AppContext;

use super::{is_fleet_folder, load_pack};
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_RC1H2_TEST_HOME";
const CONV: &str = "rc1h2-conv";
const ASK: &str = "Which instructions were you given about memory or a boot gate?";
const MARK: &str = "Fleet skills matched this task.";
const REMOTE: &str = "ssh://git@10.77.68.86:2222/owner/fleet-notes.git";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child; `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    None
}

fn gitea_on() -> awili_config::GiteaConfig {
    let mut cfg = awili_config::Config::parse("").expect("empty config parses");
    cfg.gitea.enabled = true;
    cfg.gitea.base_url = "http://10.77.68.86:3000/api/v1".to_string(); // fixture host
    cfg.gitea
}

/// A checkout at `dir` whose `origin` is `url`.
fn checkout(dir: &Path, url: &str) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(
        dir.join(".git").join("config"),
        format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n"),
    )
    .unwrap();
}

/// A pack with one recipe that "memory" selects by name.
fn memory_pack(root: &Path) -> Arc<awili_skills::SkillPack> {
    let dir = root.join("memory-provenance");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: memory-provenance\ndescription: Write and correct Interlock memories.\n\
         when_to_use: Use whenever storing a fact in the interlock memory service.\n\
         tools_needed: [mcp-interlock]\n---\n\n# Memory writes\nUse the Interlock remember tool.\n",
    )
    .unwrap();
    let mut cfg = awili_config::Config::builtin();
    cfg.skills.fleet_pack = Some(root.to_path_buf());
    load_pack(&cfg).expect("pack loads")
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn recording_claude(dir: &Path) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir` that writes its prompt (read from stdin) to
/// `dir/prompt` and answers one stream-json turn.
fn recording_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let script = format!(
        "#!/bin/sh\n\
         cat > {prompt}.part && mv {prompt}.part {prompt}\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\"}}'\n",
        prompt = dir.join("prompt").display(),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn prompt_seen(cx: &mut gpui::TestAppContext, dir: &Path) -> String {
    let file = dir.join("prompt");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if let Ok(p) = std::fs::read_to_string(&file) {
            if !p.is_empty() {
                return crate::test_stdin::claude_prompt(&p).unwrap_or(p);
            }
        }
        assert!(Instant::now() < deadline, "the agent CLI never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One Work submit of [`ASK`] in `folder` with the memory pack loaded and the
/// Gitea family on. Claude: the prompt the fake CLI received. In-app: the
/// prompt text the (captured) worker received.
fn prompt_for(
    cx: &mut gpui::TestAppContext,
    pack: &Arc<awili_skills::SkillPack>,
    folder: &Path,
    in_app: bool,
) -> String {
    let bin_dir = tempfile::tempdir().unwrap();
    let captured: std::rc::Rc<std::cell::RefCell<Option<awili_work::WorkerContext>>> =
        Default::default();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.fleet_pack = Some(pack.clone());
        app.work.connectors.gitea = gitea_on();
        app.work.folder = Some(folder.to_path_buf());
        if in_app {
            app.work.engine.base_url = "http://127.0.0.1:9".into();
            app.work.engine.model = "m".into();
            app.work.executor = WorkExecutor::InApp;
            let keep = captured.clone();
            crate::work::TEST_WORKER_SPAWN.with(|s| {
                *s.borrow_mut() = Some(Box::new(move |ctx| {
                    *keep.borrow_mut() = Some(ctx);
                }));
            });
        } else {
            app.work.agents.claude_bin =
                Some(recording_claude(bin_dir.path()).display().to_string());
            app.work.executor = WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            };
        }
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run(ASK.into(), cx);
    });
    if in_app {
        let ctx = captured
            .borrow_mut()
            .take()
            .expect("the in-app worker was spawned");
        let prompt = match ctx.cmd_rx.try_recv().expect("a prompt reached the worker") {
            awili_work::WorkCmd::Prompt { text, .. } => text,
            _ => panic!("expected a prompt"),
        };
        entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
        prompt
    } else {
        prompt_seen(cx, bin_dir.path())
    }
}

/// The real Work submit path: the recipe reaches a Claude turn in a fleet
/// checkout only — not in an ordinary folder, and never the in-app engine,
/// even in a fleet checkout.
#[gpui::test]
fn rc1h2_recipes_reach_only_fleet_folders_and_never_in_app(cx: &mut gpui::TestAppContext) {
    if isolated("fleet_skills::h2_tests::rc1h2_recipes_reach_only_fleet_folders_and_never_in_app")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let packs = tempfile::tempdir().unwrap();
    let pack = memory_pack(packs.path());
    let household = tempfile::tempdir().unwrap();
    let fleet = tempfile::tempdir().unwrap();
    checkout(fleet.path(), REMOTE);

    let plain = prompt_for(cx, &pack, household.path(), false);
    assert!(plain.contains(ASK), "{plain}");
    assert!(
        !plain.contains(MARK),
        "no recipe in a household folder: {plain}"
    );
    assert!(!plain.contains("Interlock"), "{plain}");

    let fleet_prompt = prompt_for(cx, &pack, fleet.path(), false);
    assert!(
        fleet_prompt.contains(MARK),
        "a fleet checkout keeps its recipes"
    );
    assert!(fleet_prompt.contains(ASK), "{fleet_prompt}");

    let in_app = prompt_for(cx, &pack, fleet.path(), true);
    assert!(in_app.contains(ASK), "{in_app}");
    assert!(
        !in_app.contains(MARK),
        "never for the in-app engine: {in_app}"
    );
}

/// What counts as a fleet folder.
#[test]
fn rc1h2_fleet_folder_is_a_checkout_on_the_gitea_host() {
    let gitea = gitea_on();
    let root = tempfile::tempdir().unwrap();

    let plain = root.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(!is_fleet_folder(&plain, &gitea), "no .git");

    let fleet = root.path().join("fleet");
    checkout(&fleet, REMOTE);
    assert!(is_fleet_folder(&fleet, &gitea));
    let sub = fleet.join("docs").join("notes");
    std::fs::create_dir_all(&sub).unwrap();
    assert!(
        is_fleet_folder(&sub, &gitea),
        "a folder inside the checkout"
    );

    let scp = root.path().join("scp");
    checkout(&scp, "git@10.77.68.86:owner/x.git");
    assert!(is_fleet_folder(&scp, &gitea), "scp-form remote");

    let other = root.path().join("other");
    checkout(&other, "https://github.com/someone/x.git");
    assert!(!is_fleet_folder(&other, &gitea), "another host");

    let mut off = gitea.clone();
    off.enabled = false;
    assert!(!is_fleet_folder(&fleet, &off), "the Gitea family is off");

    // A linked worktree: `.git` is a `gitdir:` file, the remote lives in the
    // shared config named by `commondir`.
    let main_git = root.path().join("main").join(".git");
    let wt_git = main_git.join("worktrees").join("wt");
    std::fs::create_dir_all(&wt_git).unwrap();
    std::fs::write(
        main_git.join("config"),
        format!("[remote \"origin\"]\n\turl = {REMOTE}\n"),
    )
    .unwrap();
    std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
    let wt = root.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
    assert!(is_fleet_folder(&wt, &gitea), "a linked worktree");
}

/// RC1-H2-FIX1 (Codex T2 P1 on 578c3c0): only a REMOTE on the Gitea host
/// makes a fleet folder. A household checkout whose only Gitea URL is a
/// submodule's (or an `insteadOf` rewrite) gets no recipes; section and key
/// names are case-insensitive, and a section ends at the next header.
#[test]
fn rc1h2_fix1_only_remote_urls_count() {
    let gitea = gitea_on();
    let root = tempfile::tempdir().unwrap();
    let write = |name: &str, config: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git").join("config"), config).unwrap();
        dir
    };
    let household = write(
        "household",
        &format!(
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = https://github.com/someone/x.git\n\
             [submodule \"fleet\"]\n\turl = {REMOTE}\n\
             [url \"{REMOTE}\"]\n\tinsteadOf = fleet:\n"
        ),
    );
    assert!(
        !is_fleet_folder(&household, &gitea),
        "a submodule or insteadOf URL on the Gitea host is not a remote"
    );
    let fleet = write(
        "fleet",
        &format!(
            "[submodule \"other\"]\n\turl = https://github.com/someone/y.git\n\
             [Remote \"origin\"]\n\tURL = {REMOTE}\n"
        ),
    );
    assert!(
        is_fleet_folder(&fleet, &gitea),
        "a remote on the host counts"
    );
    let commented = write(
        "commented",
        &format!("[remote \"origin\"]\n\t# url = {REMOTE}\n\turl = https://github.com/a/b.git\n"),
    );
    assert!(
        !is_fleet_folder(&commented, &gitea),
        "a comment is not a URL"
    );
}

/// RC1-H2-FIX1 (Codex T2 P2 on 578c3c0): the NEAREST `.git` decides. A
/// checkout nested inside a fleet checkout whose own config cannot be read,
/// is too large, or whose gitfile points nowhere gets no recipes — the
/// search never continues to the fleet checkout above it. A plain folder
/// inside a fleet checkout (no `.git` of its own) still counts.
#[test]
fn rc1h2_fix1_the_nearest_checkout_decides() {
    let gitea = gitea_on();
    let root = tempfile::tempdir().unwrap();
    let outer = root.path().join("outer");
    checkout(&outer, REMOTE);
    let plain = outer.join("docs");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(
        is_fleet_folder(&plain, &gitea),
        "no .git of its own: the outer checkout"
    );

    // A nested checkout with no readable config (`.git` holds no `config`).
    let unreadable = outer.join("unreadable");
    std::fs::create_dir_all(unreadable.join(".git")).unwrap();
    assert!(!is_fleet_folder(&unreadable, &gitea), "unreadable: deny");
    let below = unreadable.join("sub");
    std::fs::create_dir_all(&below).unwrap();
    assert!(!is_fleet_folder(&below, &gitea), "below it too");

    // A nested checkout whose config is over the size cap.
    let huge = outer.join("huge");
    std::fs::create_dir_all(huge.join(".git")).unwrap();
    let mut text = format!("[remote \"origin\"]\n\turl = {REMOTE}\n");
    text.push_str(&"# padding\n".repeat(30_000));
    std::fs::write(huge.join(".git").join("config"), text).unwrap();
    assert!(!is_fleet_folder(&huge, &gitea), "too large to check: deny");

    // A nested gitfile pointing at a git dir that does not exist.
    let dangling = outer.join("dangling");
    std::fs::create_dir_all(&dangling).unwrap();
    std::fs::write(dangling.join(".git"), "gitdir: ../no-such-gitdir\n").unwrap();
    assert!(
        !is_fleet_folder(&dangling, &gitea),
        "a broken gitfile: deny"
    );

    // A nested gitfile to a submodule-style git dir with its own household
    // remote: that checkout's own remote decides.
    let module_git = outer.join(".git").join("modules").join("kid");
    std::fs::create_dir_all(&module_git).unwrap();
    std::fs::write(
        module_git.join("config"),
        "[remote \"origin\"]\n\turl = https://github.com/someone/kid.git\n",
    )
    .unwrap();
    let kid = outer.join("kid");
    std::fs::create_dir_all(&kid).unwrap();
    std::fs::write(kid.join(".git"), "gitdir: ../.git/modules/kid\n").unwrap();
    assert!(
        !is_fleet_folder(&kid, &gitea),
        "a nested household checkout"
    );
}
