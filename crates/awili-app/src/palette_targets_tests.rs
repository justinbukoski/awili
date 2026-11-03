//! M2i proof tests: the pure path-mode row math (fake filesystem in
//! closures), then the behaviour of `use_folder` / `activate_code_tab` in an
//! isolated app (the same `isolated()` subprocess pattern as the other
//! behavioural tests in this crate).

use super::{display_path, folder_rows, path_mode, CHILD_CAP};
use gpui::AppContext;

const HOME: &str = "/h";

/// A fake tree rooted at `/h`: `night/{usability,rv2,.hidden}`.
/// `list_dirs` reads the fixed child table; `is_dir` checks the fixed set.
/// The query's `~` expands against `HOME`.
fn tree() -> (impl Fn(&str) -> Vec<String>, impl Fn(&str) -> bool) {
    let set = [
        "/h".to_string(),
        "/h/night".to_string(),
        "/h/night/usability".to_string(),
        "/h/night/rv2".to_string(),
        "/h/night/.hidden".to_string(),
    ];
    let child: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::from([
        ("/h", vec!["night"]),
        ("/h/night", vec!["usability", "rv2", ".hidden"]),
    ]);
    let list = move |dir: &str| -> Vec<String> {
        child
            .get(dir)
            .map(|v| v.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    };
    let is_dir = move |p: &str| set.contains(&p.to_string());
    (list, is_dir)
}

fn rows(
    query: &str,
    saved: &[String],
    tree: (impl Fn(&str) -> Vec<String>, impl Fn(&str) -> bool),
) -> Vec<String> {
    let (list, is_dir) = tree;
    folder_rows(query, HOME, saved, list, is_dir)
}

/// R2h: the env var naming the private HOME inside the child.
const M2I_CHILD: &str = "AWILI_M2I_TEST_HOME";

/// R2h: run `test` again in a subprocess with a private HOME (the U2c-FIX1
/// isolation) and return a fresh folder under that HOME inside the child;
/// `None` in the parent once the child passed. Nothing reads
/// `AWILI_APP_DIR`, so these app tests used to open the owner's real Awili
/// database and leave "awili-m2i-…" projects and folder groups in it (RC1
/// round 2, Minor 15).
fn isolated_app_dir(test: &str) -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os(M2I_CHILD) {
        let dir = std::path::PathBuf::from(home).join("m2i");
        std::fs::create_dir_all(&dir).unwrap();
        return Some(dir);
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(M2I_CHILD, home.path())
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

#[test]
fn m2i_path_mode_only_for_paths() {
    assert!(path_mode("~/x"));
    assert!(path_mode("/tmp"));
    assert!(!path_mode("glm"));
    assert!(!path_mode(""));
    assert!(!path_mode("x/y"));
}

#[test]
fn m2i_folder_rows_complete_the_last_segment() {
    // A trailing "/" lists the dir's children (no hidden) — the dir itself is
    // the FIRST row (step 1), then its children.
    let t = tree();
    assert_eq!(
        rows("~/night/", &[], t),
        vec!["/h/night", "/h/night/rv2", "/h/night/usability"]
    );
    // The last segment prefix-matches case-insensitively (partial mode:
    // the typed path is NOT offered as a row — it's a prefix, not a dir).
    let t = tree();
    assert_eq!(rows("~/night/u", &[], t), vec!["/h/night/usability"]);
    // A segment starting with "." reveals hidden siblings (partial mode:
    // the typed path is a prefix, not a dir, so it is NOT offered).
    let t = tree();
    assert_eq!(rows("~/night/.h", &[], t), vec!["/h/night/.hidden"]);
    // The cap holds: at most CHILD_CAP children. The typed path itself is
    // appended after the child scan, so it never displaces a child — but it
    // also must not be double-counted (it is not a child of itself).
    let list = |_d: &str| (0..12).map(|i| format!("d{i}")).collect();
    let is_dir = |p: &str| {
        p == "/h/many"
            || p.strip_prefix("/h/many/d")
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
    };
    let r = folder_rows("~/many/", HOME, &[], list, is_dir);
    // 8 children + the typed dir itself = 9 rows (the dir is appended after
    // the child cap, so it never displaces a child).
    assert_eq!(
        r.len(),
        CHILD_CAP + 1,
        "at most {CHILD_CAP} children: {r:?}"
    );
}

#[test]
fn m2i_saved_projects_come_first_and_dedupe() {
    let saved = vec!["/h/night/usability".to_string()];
    // M2i-FIX1 (T2 P1, a spec-mandated correction): the saved row comes
    // FIRST (M2i: "saved Code projects ... first"), then the typed dir itself.
    let t = tree();
    assert_eq!(
        rows("~/night", &saved, t),
        vec!["/h/night/usability", "/h/night"]
    );
    // The parent spec's own case: "~/n" puts the saved project first, once.
    let t = tree();
    assert_eq!(
        rows("~/n", &saved, t),
        vec!["/h/night/usability", "/h/night"]
    );
    // A saved folder matches even when the typed dir is itself being completed.
    let t = tree();
    assert_eq!(
        rows("~/night/usability", &saved, t),
        vec!["/h/night/usability"]
    );
    // A bare "~" offers the typed path itself (home) as the first row.
    let t = tree();
    assert_eq!(rows("~", &[], t).into_iter().next(), Some("/h".to_string()));
    // But "~/night" does NOT re-list the dir itself when its parent row
    // ("~/") is already offered (the parent's children cover it).
    // Here the saved row is the dir itself, so the parent is not offered —
    // the typed dir appears (step 2), but NOT duplicated by the child scan.
    let t = tree();
    {
        let r = rows("~/night", &saved, t);
        assert_eq!(r.iter().filter(|p| p.as_str() == "/h/night").count(), 1);
    }
}

#[test]
fn m2i_missing_dir_offers_nothing_for_itself() {
    let t = tree();
    assert_eq!(rows("~/nope/", &[], t), Vec::<String>::new());
}

#[test]
fn m2i_display_path_abbreviates_home() {
    assert_eq!(display_path("/h/night/rv2", "/h"), "~/night/rv2");
    assert_eq!(display_path("/h", "/h"), "~");
    assert_eq!(display_path("/tmp/x", "/h"), "/tmp/x");
}

#[test]
fn m2i_use_folder_in_code_adds_and_activates_a_project() {
    // The regression itself: in a Code-mode app, `use_folder` with a fresh
    // folder adds a project AND switches to it.
    let Some(app_dir) = isolated_app_dir(
        "palette_targets::tests::m2i_use_folder_in_code_adds_and_activates_a_project",
    ) else {
        return;
    };
    std::env::set_var("AWILI_APP_DIR", &app_dir);
    // `use_folder` requires an existing directory; create one.
    let proj = app_dir.join("proj");
    let alpha = proj.join("alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    let alpha_str = alpha.to_str().unwrap().to_string();

    let mut cx = gpui::TestAppContext::single();
    cx.executor().allow_parking();
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(&mut cx, |app, cx| {
        app.mode = crate::work::Mode::Code;
        app.ensure_code_tab(cx);
        app.use_folder(&alpha_str, cx);
    });
    wait_for_project(&entity, &mut cx, &alpha_str);
    entity.read_with(&cx, |app, _| {
        // The fallback folder is an unsaved selection; only the explicit
        // `use_folder` choice becomes a saved project.
        assert!(
            app.work.projects.len() == 1,
            "a new project was added: {:?}",
            app.work.projects
        );
        assert!(
            app.work.code_active_project < app.work.projects.len(),
            "the active project index is valid"
        );
        assert_eq!(
            app.work.projects[app.work.code_active_project].folder, alpha_str,
            "the new project is active"
        );
    });
    let _ = std::fs::remove_dir_all(&app_dir);
}

#[test]
fn m2i_tab_switch_restores_its_project() {
    // The regression itself: switching to a tab whose `project_folder`
    // matches a saved project switches the active project back to it.
    let Some(app_dir) =
        isolated_app_dir("palette_targets::tests::m2i_tab_switch_restores_its_project")
    else {
        return;
    };
    std::env::set_var("AWILI_APP_DIR", &app_dir);
    // Create the project folders the projects will point at.
    let a = app_dir.join("a");
    let b = app_dir.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let a_str = a.to_str().unwrap().to_string();
    let b_str = b.to_str().unwrap().to_string();

    let mut cx = gpui::TestAppContext::single();
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(&mut cx, |app, cx| {
        app.mode = crate::work::Mode::Code;
        app.ensure_code_tab(cx);
        // Add `a` and `b` as projects; active = `b` (the last added).
        app.add_code_project(a.clone(), cx);
        app.add_code_project(b.clone(), cx);
    });
    wait_for_project(&entity, &mut cx, &b_str);
    entity.update(&mut cx, |app, _cx| {
        // Bind tab 0 to `a`: set its `project_folder`, then switch away to `b`
        // so the active project is NOT `a`. Then switching back to tab 0 must
        // restore `a`.
        // RC2-UX (R2-17, deliberate change): tab 0 has already worked in `a`
        // (one row) — a tab that has not run yet now follows the project
        // picked in it, so "switch away to `b`" opens `b` in a fresh tab.
        if let Some(tab) = app.work.code_tabs.get_mut(0) {
            tab.project_folder = Some(a_str.clone());
            tab.convo.push(awili_core::Role::User, "ran in a");
            let row = crate::MessageRow::new(tab.convo.message(0).unwrap());
            tab.rows.push(row);
        }
        let b_idx = app
            .work
            .projects
            .iter()
            .position(|p| p.folder == b_str)
            .unwrap();
        app.switch_code_project(b_idx);
        app.activate_code_tab(0);
    });
    entity.read_with(&cx, |app, _| {
        let a_idx = app
            .work
            .projects
            .iter()
            .position(|p| p.folder == a_str)
            .unwrap();
        assert_eq!(
            app.work.code_active_project,
            a_idx,
            "switching to tab 0 restored project a (active = {:?}, projects = {:?})",
            app.work.code_active_project,
            app.work
                .projects
                .iter()
                .map(|p| &p.folder)
                .collect::<Vec<_>>()
        );
    });
    let _ = std::fs::remove_dir_all(&app_dir);
}

#[test]
fn m2i_use_folder_in_work_binds_the_folder() {
    // The regression itself: in Work mode, `use_folder` binds the folder.
    let Some(app_dir) =
        isolated_app_dir("palette_targets::tests::m2i_use_folder_in_work_binds_the_folder")
    else {
        return;
    };
    std::env::set_var("AWILI_APP_DIR", &app_dir);
    // `use_folder` requires an existing directory.
    let proj = app_dir.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let proj_str = proj.to_str().unwrap().to_string();

    let mut cx = gpui::TestAppContext::single();
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(&mut cx, |app, cx| {
        app.mode = crate::work::Mode::Work;
        app.use_folder(&proj_str, cx);
    });
    entity.read_with(&cx, |app, _| {
        assert_eq!(
            app.work.folder.as_deref(),
            Some(std::path::Path::new(&proj_str)),
            "Work mode bound the folder"
        );
    });
    let _ = std::fs::remove_dir_all(&app_dir);
}

// ---- M2i-FIX1 (T2 review of c44e6ee) -----------------------------------------

/// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
/// config and opens persistence). Same pattern as `s1a_proof_tests::isolated`.
/// Returns the private home inside the child; `None` in the parent once the
/// child has passed.
fn isolated(test: &str) -> Option<std::path::PathBuf> {
    const CHILD: &str = "AWILI_M2I_FIX1_HOME";
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated M2i-FIX1 test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "subprocess must actually execute the regression: {stdout}"
    );
    None
}

/// M2i-FIX1 (T2 P2 x2): the REAL palette path, in the whole app painted in a
/// test window. `open_palette_with` (what ⌘O calls) types a path query; the
/// render mirrors it into the palette; `palette_cmds` builds the rows the
/// modal paints; Enter goes through the root capture handler
/// (`on_palette_key`) to `use_folder`. `zeta` sorts after 600 other folders,
/// so it is offered only when the prefix filter runs before any cap.
#[gpui::test]
fn m2i_fix1_enter_on_a_folder_row_past_500_names_binds_it(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("palette_targets::tests::m2i_fix1_enter_on_a_folder_row_past_500_names_binds_it")
    else {
        return;
    };
    let proj = home.join("proj");
    for i in 0..600 {
        std::fs::create_dir_all(proj.join(format!("a{i:03}"))).unwrap();
    }
    let zeta = proj.join("zeta");
    std::fs::create_dir_all(zeta.join("inner")).unwrap();
    let zeta_str = zeta.to_str().unwrap().to_string();

    let (entity, window) = cx.add_window_view(|_, cx| crate::AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    window.update(|window, cx| {
        entity.update(cx, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.mode = crate::work::Mode::Work;
            app.open_palette_with("~/proj/ze", window, cx);
        });
    });
    window.run_until_parked();

    // The rows the modal paints: one folder row, `zeta`, and it is focused.
    entity.read_with(window, |app, _| {
        let cmds = app.palette_cmds();
        let folders: Vec<String> = cmds
            .iter()
            .filter_map(|c| match c {
                crate::palette::PaletteCmd::UseFolder(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            folders,
            vec![zeta_str.clone()],
            "zeta is offered although 600 names sort before it"
        );
        assert_eq!(
            cmds.get(app.palette.focused_idx),
            Some(&crate::palette::PaletteCmd::UseFolder(zeta_str.clone())),
            "the focused row is the zeta folder row"
        );
    });

    // Enter runs the focused row through the real key path.
    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert!(
            !app.palette.open,
            "Enter ran the row and closed the palette"
        );
        assert_eq!(
            app.work.folder.as_deref(),
            Some(zeta.as_path()),
            "Enter bound the folder the palette showed"
        );
    });
}

// ---- M2h: switch the executor from the keyboard --------------------------------

/// The `persist` strings of the `UseExecutor` rows, in order.
fn executor_persists(cmds: Vec<crate::palette::PaletteCmd>) -> Vec<String> {
    cmds.into_iter()
        .map(|c| match c {
            crate::palette::PaletteCmd::UseExecutor { persist, .. } => persist,
            other => panic!("not an executor row: {other:?}"),
        })
        .collect()
}

/// M2h: an "Executor: …" row matches on the word "executor" or on its label,
/// and nothing else.
#[test]
fn m2h_executor_rows_match_word_or_label() {
    let rows = vec![
        ("In-app · Gemma".to_string(), "in-app".to_string()),
        ("GLM · default".to_string(), "agent:glm".to_string()),
    ];
    let both = vec!["in-app".to_string(), "agent:glm".to_string()];
    let glm = vec!["agent:glm".to_string()];
    assert_eq!(
        executor_persists(super::executor_rows("executor", &rows)),
        both
    );
    assert_eq!(
        executor_persists(super::executor_rows("executor ", &rows)),
        both
    );
    assert_eq!(executor_persists(super::executor_rows("", &rows)), both);
    assert_eq!(executor_persists(super::executor_rows("glm", &rows)), glm);
    assert_eq!(
        executor_persists(super::executor_rows("executor glm", &rows)),
        glm
    );
    assert!(super::executor_rows("zzz", &rows).is_empty());
}

/// M2h: the four "New Code tab: …" rows match "code tab", "executor" (⌘E) or
/// their engine.
#[test]
fn m2h_code_tab_rows_match_word_or_engine() {
    use crate::palette::PaletteCmd::NewCodeTab;
    use awili_code::CodeEngine;
    assert_eq!(super::code_tab_rows("code tab").len(), 4);
    assert_eq!(super::code_tab_rows("executor ").len(), 4);
    assert_eq!(
        super::code_tab_rows("deepseek"),
        vec![NewCodeTab(CodeEngine::Dsh)]
    );
    assert_eq!(
        super::code_tab_rows("executor glm"),
        vec![NewCodeTab(CodeEngine::Glm)]
    );
    assert!(super::code_tab_rows("zzz").is_empty());
}

/// The whole app painted in a focused test window, in `mode`.
fn painted_app(
    cx: &mut gpui::TestAppContext,
    mode: crate::work::Mode,
) -> (gpui::Entity<crate::AwiliApp>, &mut gpui::VisualTestContext) {
    let (entity, window) = cx.add_window_view(|_, cx| crate::AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    entity.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = mode;
        if mode == crate::work::Mode::Code {
            app.ensure_code_tab(cx);
        }
    });
    window.run_until_parked();
    (entity, window)
}

/// M2h (F05): ⌘E opens the palette on "executor ", typing "claude" leaves the
/// Claude row focused, and Enter switches the Work executor to Claude, says so,
/// and persists it (read back from the store on disk).
#[gpui::test]
fn m2h_cmd_e_claude_enter_sets_and_persists_the_executor(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("palette_targets::tests::m2h_cmd_e_claude_enter_sets_and_persists_the_executor")
    else {
        return;
    };
    let claude = crate::work::WorkExecutor::Agent {
        engine: awili_code::CodeEngine::Claude,
        model: None,
    };
    let (entity, window) = painted_app(cx, crate::work::Mode::Work);
    entity.read_with(window, |app, _| {
        assert_ne!(app.work.executor, claude, "the test starts elsewhere");
    });

    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-e"));
    entity.read_with(window, |app, _| {
        assert!(app.palette.open, "⌘E opened the palette");
        assert_eq!(app.palette.query, "executor ");
    });
    window.simulate_input("claude");
    entity.read_with(window, |app, _| {
        assert_eq!(app.palette.query, "executor claude");
        let cmds = app.palette_cmds();
        assert_eq!(
            cmds.get(app.palette.focused_idx),
            Some(&crate::palette::PaletteCmd::UseExecutor {
                label: "Claude · default".to_string(),
                persist: "agent:claude".to_string(),
            }),
            "the focused row is Claude: {cmds:?}"
        );
    });

    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert!(!app.palette.open, "Enter ran the row");
        assert_eq!(app.work.executor, claude, "the executor is Claude");
        let toast = app
            .toast
            .visible()
            .map(|m| m.to_string())
            .unwrap_or_default();
        assert!(toast.starts_with("Executor: Claude"), "{toast:?}");
    });

    // The persistence worker writes asynchronously: poll a separate read
    // connection with a bounded deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let stored = awili_persist::Store::open_default()
            .unwrap()
            .get_app_state(crate::persistence::WORK_EXECUTOR_KEY)
            .unwrap();
        if stored.as_deref() == Some("agent:claude") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "work_executor never persisted as agent:claude: {stored:?}"
        );
        window.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// M2h: an unparsable executor row changes nothing and says nothing.
#[gpui::test]
fn m2h_garbage_persist_is_ignored(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated("palette_targets::tests::m2h_garbage_persist_is_ignored") else {
        return;
    };
    let (entity, window) = painted_app(cx, crate::work::Mode::Work);
    entity.update(window, |app, cx| {
        let before = app.work.executor.clone();
        app.use_executor("nope", cx);
        assert_eq!(app.work.executor, before);
        assert!(app.toast.visible().is_none(), "no toast");
    });
}

/// M2h: in Code, ⌘E then "deep" then Enter opens a DeepSeek tab.
#[gpui::test]
fn m2h_cmd_e_in_code_opens_a_deepseek_tab(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated("palette_targets::tests::m2h_cmd_e_in_code_opens_a_deepseek_tab")
    else {
        return;
    };
    let (entity, window) = painted_app(cx, crate::work::Mode::Code);
    let tabs = entity.read_with(window, |app, _| app.work.code_tabs.len());

    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-e"));
    window.simulate_input("deep");
    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert!(!app.palette.open, "Enter ran the row");
        assert_eq!(app.work.code_tabs.len(), tabs + 1, "one new tab");
        assert_eq!(
            app.work.code_tabs[app.work.code_active].engine,
            awili_code::CodeEngine::Dsh,
            "the new active tab is DeepSeek"
        );
    });
}

/// RC2-UX (R2-15, round 2 Minor 8): ⌘O lists the recent folders and the
/// saved projects first, wherever they live; typing keeps those whose path
/// starts with it or whose name contains the last typed part.
#[test]
fn rc2ux_cmd_o_lists_recent_and_saved_folders_first() {
    use super::{known_folder_rows, remember_recent, RECENT_CAP};
    let dirs = ["/h/night", "/v/r1", "/v/proj/f2c", "/v/proj/alpha"];
    let is_dir = |p: &str| dirs.contains(&p);
    let recent = vec![
        "/v/r1".to_string(),
        "/h/night".to_string(),
        "/gone".to_string(),
    ];
    let saved = vec![
        "/v/proj/f2c".to_string(),
        "/v/proj/alpha".to_string(),
        "/v/r1".to_string(),
    ];
    assert_eq!(
        known_folder_rows("~/", HOME, &recent, &saved, is_dir),
        vec!["/v/r1", "/h/night", "/v/proj/alpha", "/v/proj/f2c"],
        "recent first (newest first), then saved by name; once each; only existing folders"
    );
    assert_eq!(
        known_folder_rows("~/f2", HOME, &recent, &saved, is_dir),
        vec!["/v/proj/f2c"],
        "a saved project outside the typed prefix is found by its name"
    );
    assert_eq!(
        known_folder_rows("~/ni", HOME, &recent, &saved, is_dir),
        vec!["/h/night"]
    );
    assert_eq!(
        known_folder_rows("/v/proj/", HOME, &recent, &saved, is_dir),
        vec!["/v/proj/alpha", "/v/proj/f2c"]
    );
    let mut r: Vec<String> = Vec::new();
    for i in 0..(RECENT_CAP + 3) {
        remember_recent(&mut r, &format!("/f{i}/"));
    }
    remember_recent(&mut r, "/f5");
    assert_eq!(r.len(), RECENT_CAP);
    assert_eq!(r[0], "/f5");
    assert_eq!(r.iter().filter(|p| p.as_str() == "/f5").count(), 1);
}

/// RC2-UX (R2-15): a folder chosen in Work (and one opened as a Code
/// project) is listed first by ⌘O — the real palette rows — and is still
/// there after a relaunch; a "Save as project" folder elsewhere is listed in
/// Work too.
#[gpui::test]
fn rc2ux_cmd_o_remembers_folders_across_a_relaunch(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("palette_targets::tests::rc2ux_cmd_o_remembers_folders_across_a_relaunch")
    else {
        return;
    };
    let work = home.join("work-a");
    let code = home.join("deep").join("code-b");
    let saved = tempfile::tempdir().unwrap();
    let saved = saved.path().join("f2c");
    for d in [&work, &code, &saved] {
        std::fs::create_dir_all(d).unwrap();
    }
    let s = |p: &std::path::Path| p.display().to_string();
    let folder_rows_of = |app: &crate::AwiliApp| -> Vec<String> {
        app.palette_cmds()
            .into_iter()
            .filter_map(|c| match c {
                crate::palette::PaletteCmd::UseFolder(p) => Some(p),
                _ => None,
            })
            .collect()
    };
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = crate::work::Mode::Work;
        app.use_folder(&s(&work), cx);
        app.mode = crate::work::Mode::Code;
        app.ensure_code_tab(cx);
        app.use_folder(&s(&code), cx);
        app.mode = crate::work::Mode::Work;
        app.work.projects = vec![awili_persist::StoredProject {
            id: 1,
            name: "f2c".into(),
            folder: s(&saved),
            instructions: String::new(),
            created_ts: 0,
        }];
        app.palette.set_query("~/");
        let rows = folder_rows_of(app);
        assert_eq!(rows[..3], [s(&code), s(&work), s(&saved)], "{rows:?}");
        app.palette.set_query("~/f2");
        assert_eq!(folder_rows_of(app).first(), Some(&s(&saved)));
    });
    // The list is saved; a relaunch reads it back.
    let store = awili_persist::Store::open_default().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let want = serde_json::to_string(&vec![s(&code), s(&work)]).unwrap();
    while store
        .get_app_state(super::RECENT_FOLDERS_KEY)
        .unwrap()
        .as_deref()
        != Some(want.as_str())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the list was never saved"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let relaunched = cx.new(crate::AwiliApp::new);
    relaunched.read_with(cx, |app, _| {
        assert_eq!(app.work.recent_folders, vec![s(&code), s(&work)]);
    });
}

// ---- RC2-R1F3: the ⌘O folder box stays the folder box ------------------------

/// The app in Work mode in a painted test window, with the standard key
/// bindings (⌘A, ⌘K) bound as `main` binds them.
fn painted_work_app(
    cx: &mut gpui::TestAppContext,
) -> (gpui::Entity<crate::AwiliApp>, &mut gpui::VisualTestContext) {
    cx.update(|cx| cx.bind_keys(crate::app_lifecycle::standard_bindings()));
    let (entity, window) = cx.add_window_view(|_, cx| crate::AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    entity.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(crate::work::Mode::Work, cx);
    });
    window.run_until_parked();
    (entity, window)
}

/// RC2-R1F3 (usability RC2 round 1, N3: "selecting all and pressing
/// Backspace to empty the box dropped to the Search conversations…
/// palette"). Through the real keys: ⌘O opens the folder box, ⌘A and
/// Backspace empty it — it is still the folder box (folder rows only, the
/// recent folder first, its own words in the empty box), and Enter binds
/// the recent folder.
#[gpui::test]
fn rc2r1f3_emptying_the_folder_box_keeps_it_a_folder_box(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("palette_targets::tests::rc2r1f3_emptying_the_folder_box_keeps_it_a_folder_box")
    else {
        return;
    };
    let k3 = home.join("sandbox").join("k3");
    std::fs::create_dir_all(&k3).unwrap();
    let k3_str = k3.to_str().unwrap().to_string();
    let (entity, window) = painted_work_app(cx);
    entity.update(window, |app, _| {
        app.work.recent_folders = vec![k3_str.clone()];
        // A conversation the search would list.
        app.conversations
            .create_kind(1_000, awili_core::ConvKind::Work);
    });
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-o"));
    window.run_until_parked();
    let typed = |window: &mut gpui::VisualTestContext| {
        entity.read_with(window, |app, cx| {
            app.palette_input.read(cx).text().to_string()
        })
    };
    assert_eq!(typed(window), "~/");
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-a backspace"));
    window.run_until_parked();
    assert_eq!(typed(window), "", "the box is empty");
    entity.read_with(window, |app, cx| {
        assert!(app.palette.open, "still open");
        let cmds = app.palette_cmds();
        assert!(
            cmds.iter()
                .all(|c| matches!(c, crate::palette::PaletteCmd::UseFolder(_))),
            "folders only, never the conversation search: {cmds:?}"
        );
        assert_eq!(
            cmds.first(),
            Some(&crate::palette::PaletteCmd::UseFolder(k3_str.clone())),
            "the recent folder first"
        );
        assert_eq!(
            app.palette_input.read(cx).placeholder(),
            super::super::FOLDER_BOX_PLACEHOLDER
        );
    });
    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert!(!app.palette.open);
        assert_eq!(app.work.folder.as_deref(), Some(k3.as_path()));
    });
    // ⌘K is the conversation search again.
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-k"));
    window.run_until_parked();
    entity.read_with(window, |app, cx| {
        assert!(app.palette.open && !app.palette.folder_box);
        assert_eq!(
            app.palette_input.read(cx).placeholder(),
            super::super::PALETTE_SEARCH_PLACEHOLDER
        );
    });
}

/// RC2-R1F3 (round 1 N7: typing "continue" into the ⌘K palette showed
/// "aaaaaaaa" under the test tool). Through the real key path, every typed
/// character lands as itself — also when a key's name is not the character
/// it types (another keyboard layout, or a tool that sends every key as the
/// "a" key with its own character, as the test tool's key events did): the
/// box takes the typed character, never the key's name.
/// RC2-R1G4: the second half replays macOS's two steps (a key-down built
/// from the key code, then `insertText:` into the installed text-input
/// handler); AppKit itself cannot run in a unit test.
#[gpui::test]
fn rc2r1f3_the_search_box_keeps_every_typed_character(cx: &mut gpui::TestAppContext) {
    if isolated("palette_targets::tests::rc2r1f3_the_search_box_keeps_every_typed_character")
        .is_none()
    {
        return;
    }
    let (entity, window) = painted_work_app(cx);
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-k"));
    window.run_until_parked();
    window.simulate_keystrokes("c o n t i n u e");
    window.run_until_parked();
    let typed = |window: &mut gpui::VisualTestContext| {
        entity.read_with(window, |app, cx| {
            app.palette_input.read(cx).text().to_string()
        })
    };
    assert_eq!(typed(window), "continue");
    // RC2-R1G4 (RC2-R1F review P3: this step handed the box each character
    // as the key event's own character, so it could not show what macOS
    // does). macOS types in two steps (gpui_macos): the key press becomes a
    // key-down whose key AND character come from the key code and the
    // keyboard layout (events.rs `parse_keystroke`: key code 0, the test
    // tool's key, is "a" / "a" on a US layout); then AppKit's `insertText:`
    // hands the event's own characters to the window's text-input handler
    // (window.rs `insert_text` → `replace_text_in_range(None, text)`).
    // Replayed for each character: the key-down types nothing, and the text
    // arrives only through the handler the focused search box installed.
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-a backspace"));
    window.run_until_parked();
    assert_eq!(typed(window), "");
    let search_box = entity.read_with(window, |app, _| app.palette_input.entity_id());
    for (n, ch) in "continue".chars().enumerate() {
        window.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: "a".into(),
                key_char: Some("a".into()),
            },
            is_held: false,
            prefer_character_input: false,
        });
        window.run_until_parked();
        assert_eq!(
            typed(window).chars().count(),
            n,
            "the key press types nothing by itself"
        );
        let (bounds, input) = crate::composer::INSTALLED_INPUT
            .with(|i| i.borrow().clone())
            .expect("the focused box installed the window's text input");
        assert_eq!(
            input.entity_id(),
            search_box,
            "the search box holds the window's text input"
        );
        let input = input.upgrade().expect("the search box is alive");
        window.update(|window, cx| {
            gpui::InputHandler::replace_text_in_range(
                &mut gpui::ElementInputHandler::new(bounds, input),
                None,
                &ch.to_string(),
                window,
                cx,
            )
        });
        window.run_until_parked();
    }
    assert_eq!(
        typed(window),
        "continue",
        "the characters macOS hands over, never the key's"
    );
}

// ---- RC2-R2F1: Enter opens the typed path ----------------------------------

/// RC2-R2F1 (usability RC2 round 2, F1 MAJOR: "/…/rc2-round2/k3" typed into
/// ⌘O and Enter opened "/…/rc2-round1/k3", the highlighted recent folder).
/// Through the real keys: a full path to an existing folder, typed and
/// Enter, opens exactly that folder; a suggestion opens only when the owner
/// moved to it with the arrow keys.
#[gpui::test]
fn rc2r2f1_enter_opens_the_typed_path_not_a_suggestion(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("palette_targets::tests::rc2r2f1_enter_opens_the_typed_path_not_a_suggestion")
    else {
        return;
    };
    let old = home.join("sandbox").join("rc2-round1").join("k3");
    let new = home.join("sandbox").join("rc2-round2").join("k3");
    for d in [&old, &new] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (old_s, new_s) = (old.display().to_string(), new.display().to_string());
    let (entity, window) = painted_work_app(cx);
    entity.update(window, |app, _| {
        app.work.recent_folders = vec![old_s.clone()];
    });
    let type_path = |window: &mut gpui::VisualTestContext| {
        window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-o"));
        window.run_until_parked();
        window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-a backspace"));
        window.run_until_parked();
        window.simulate_input(&new_s);
        window.run_until_parked();
    };
    type_path(window);
    entity.read_with(window, |app, cx| {
        assert_eq!(app.palette_input.read(cx).text().to_string(), new_s);
        let cmds = app.palette_cmds();
        assert!(
            cmds.contains(&crate::palette::PaletteCmd::UseFolder(old_s.clone())),
            "the older folder with the same name is still suggested: {cmds:?}"
        );
        assert_eq!(
            cmds.get(app.palette.focused_idx),
            Some(&crate::palette::PaletteCmd::UseFolder(new_s.clone())),
            "the highlighted row is the typed folder"
        );
    });
    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert!(!app.palette.open);
        assert_eq!(
            app.work.folder.as_deref(),
            Some(new.as_path()),
            "Enter opened the typed path, not the suggestion"
        );
    });
    // The same path typed, then ↓ to the suggestion and Enter: the
    // suggestion opens, because the owner moved to it.
    type_path(window);
    window.simulate_keystrokes("down");
    window.run_until_parked();
    entity.read_with(window, |app, _| {
        assert_eq!(
            app.palette_cmds().get(app.palette.focused_idx),
            Some(&crate::palette::PaletteCmd::UseFolder(old_s.clone()))
        );
    });
    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(old.as_path()));
    });
}

fn wait_for_project(
    entity: &gpui::Entity<crate::AwiliApp>,
    cx: &mut gpui::TestAppContext,
    folder: &str,
) {
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    loop {
        cx.run_until_parked();
        if entity.read_with(cx, |app, _| {
            app.work.projects.iter().any(|p| p.folder == folder)
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "project worker did not acknowledge save"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn palette_bounds_inside(
    outer: gpui::Bounds<gpui::Pixels>,
    inner: gpui::Bounds<gpui::Pixels>,
) -> bool {
    inner.left() >= outer.left()
        && inner.top() >= outer.top()
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn palette_bounds_intersection(
    a: gpui::Bounds<gpui::Pixels>,
    b: gpui::Bounds<gpui::Pixels>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (left < right && top < bottom).then(|| {
        gpui::Bounds::new(
            gpui::point(left, top),
            gpui::size(right - left, bottom - top),
        )
    })
}

/// The legacy ⌘K palette uses the painted kit rows and the root capture
/// handler. Filtering to one title and accepting it must dispatch that exact
/// conversation once; reopening starts with a fresh, focused input.
#[gpui::test]
fn legacy_palette_filters_selects_and_reopens_with_fresh_input(cx: &mut gpui::TestAppContext) {
    if isolated(
        "palette_targets::tests::legacy_palette_filters_selects_and_reopens_with_fresh_input",
    )
    .is_none()
    {
        return;
    }
    let (entity, window) = painted_work_app(cx);
    let (target, other, before_count) = entity.update(window, |app, cx| {
        let target = app
            .conversations
            .create_kind(2_000, awili_core::ConvKind::Work)
            .id;
        let other = app
            .conversations
            .create_kind(2_001, awili_core::ConvKind::Work)
            .id;
        app.conversations.rename(&target, "Needle target");
        app.conversations.rename(&other, "Different conversation");
        cx.notify();
        (target, other, app.conversations.len())
    });

    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-k"));
    window.simulate_keystrokes("n t r g");
    window.run_until_parked();
    entity.read_with(window, |app, _| {
        assert!(app.palette.open);
        let cmds = app.palette_cmds();
        assert_eq!(
            cmds,
            vec![crate::palette::PaletteCmd::OpenConversation(target.clone())],
            "the rendered palette resolves only the matching conversation"
        );
        assert_ne!(other, target);
    });

    window.simulate_keystrokes("enter");
    entity.read_with(window, |app, _| {
        assert!(!app.palette.open, "accept closes before dispatch");
        assert_eq!(app.sidebar.active_id(), target);
        assert_eq!(
            app.conversations.len(),
            before_count,
            "one accept creates no duplicate"
        );
    });

    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-k"));
    window.simulate_keystrokes("r e o p e n");
    window.run_until_parked();
    entity.read_with(window, |app, cx| {
        assert!(app.palette.open);
        assert_eq!(app.palette_input.read(cx).text().to_string(), "reopen");
        assert_eq!(app.palette.query, "reopen");
    });
    window.simulate_keystrokes("escape");
    entity.read_with(window, |app, _| assert!(!app.palette.open));
}

/// Compact Work mode keeps the legacy folder completion in the sheet input;
/// Escape and a tap on the sheet scrim each dismiss through the app path.
#[gpui::test]
fn legacy_palette_compact_folder_tab_escape_and_scrim(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("palette_targets::tests::legacy_palette_compact_folder_tab_escape_and_scrim")
    else {
        return;
    };
    let recent = home.join("recent-folder");
    std::fs::create_dir_all(&recent).unwrap();
    let recent = recent.to_string_lossy().to_string();
    let (entity, window) = painted_work_app(cx);
    window.simulate_resize(gpui::size(gpui::px(720.), gpui::px(800.)));
    entity.update(window, |app, cx| {
        app.work.recent_folders = vec![recent.clone()];
        cx.notify();
    });
    let center_before = window
        .debug_bounds("awili-center")
        .expect("the compact conversation center is painted before opening");
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-o"));
    window.run_until_parked();
    let center_after = window
        .debug_bounds("awili-center")
        .expect("the compact conversation center remains painted under the palette");
    assert_eq!(
        (center_after.origin.x, center_after.size.width),
        (center_before.origin.x, center_before.size.width),
        "opening the compact palette overlays the center without changing its flex bounds"
    );
    assert!(window.debug_bounds("ui-sheet-scrim").is_some());
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    entity.read_with(window, |app, cx| {
        assert!(app.palette.open, "Tab completes a folder without closing");
        assert_eq!(
            app.palette_input.read(cx).text().to_string(),
            format!("{recent}/")
        );
    });
    window.simulate_keystrokes("escape");
    assert!(!entity.read_with(window, |app, _| app.palette.open));

    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-o"));
    window.run_until_parked();
    let scrim = window
        .debug_bounds("ui-sheet-scrim")
        .expect("the compact palette paints its Sheet scrim");
    window.simulate_click(
        scrim.origin + gpui::point(gpui::px(4.), gpui::px(4.)),
        gpui::Modifiers::default(),
    );
    assert!(!entity.read_with(window, |app, _| app.palette.open));
}

/// Clicking a painted KIT Row runs its own command exactly once through the
/// same close-before-dispatch path used by keyboard acceptance.
#[gpui::test]
fn legacy_palette_click_dispatches_code_tab_once(cx: &mut gpui::TestAppContext) {
    if isolated("palette_targets::tests::legacy_palette_click_dispatches_code_tab_once").is_none() {
        return;
    }
    let (entity, window) = painted_app(cx, crate::work::Mode::Code);
    let before = entity.read_with(window, |app, _| app.work.code_tabs.len());
    window.update(|window, cx| {
        entity.update(cx, |app, cx| {
            app.open_palette_with("deepseek", window, cx);
        });
    });
    window.run_until_parked();
    entity.read_with(window, |app, _| {
        assert_eq!(
            app.palette_cmds(),
            vec![crate::palette::PaletteCmd::NewCodeTab(
                awili_code::CodeEngine::Dsh
            )]
        );
    });
    let row = window
        .debug_bounds("ui-row-palette-row-0")
        .expect("the actual New Code tab Row is painted");
    window.simulate_click(row.center(), gpui::Modifiers::default());
    window.run_until_parked();
    entity.read_with(window, |app, _| {
        assert!(!app.palette.open, "the click closes before dispatch");
        assert_eq!(app.work.code_tabs.len(), before + 1, "one new tab");
        assert_eq!(
            app.work.code_tabs[app.work.code_active].engine,
            awili_code::CodeEngine::Dsh
        );
    });
}

/// Across desktop and compact zoom thresholds, keyboard navigation keeps the
/// selected legacy palette row inside its actual ScrollHandle viewport and
/// floating face, clipped to the live window.
#[gpui::test]
fn legacy_palette_reveals_selected_row_at_high_zoom(cx: &mut gpui::TestAppContext) {
    if isolated("palette_targets::tests::legacy_palette_reveals_selected_row_at_high_zoom")
        .is_none()
    {
        return;
    }
    let previous_scale = crate::theme::ui_scale();
    let scales = [0.9, 1.0, 1.25, 2.5];
    crate::theme::set_ui_scale(scales[0]);
    let (entity, window) = painted_work_app(cx);
    window.simulate_resize(gpui::size(gpui::px(900.), gpui::px(712.)));
    entity.update(window, |app, cx| {
        for i in 0..40 {
            app.conversations
                .create_kind(3_000 + i, awili_core::ConvKind::Work);
        }
        cx.notify();
    });
    for scale in scales {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(gpui::size(gpui::px(900.), gpui::px(712.)));
        window.run_until_parked();
        window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-k"));
        window.run_until_parked();
        let keys = std::iter::repeat_n("down", 30)
            .collect::<Vec<_>>()
            .join(" ");
        window.simulate_keystrokes(&keys);
        window.run_until_parked();
        let (focused_idx, row_count) = entity.read_with(window, |app, _| {
            (app.palette.focused_idx, app.palette_cmds().len())
        });
        assert!(focused_idx > 0 && focused_idx < row_count, "scale {scale}");
        // The Div has a GPUI element id but no debug selector; the ScrollHandle
        // reports the actual laid-out viewport directly, not row-content bounds.
        let scroll_viewport = entity.read_with(window, |app, _| app.palette_scroll.bounds());
        let compact = window.update(|window, _| crate::ui::sheet::is_compact(window));
        let face_selector = if compact {
            // Panel's full-window wrapper is not the painted sheet face.
            "ui-sheet-palette-card-panel"
        } else {
            "ui-panel-palette-card"
        };
        let face = window
            .debug_bounds(face_selector)
            .expect("the actual floating palette face is painted");
        let window_size = window.update(|window, _| window.viewport_size());
        let physical_window = gpui::Bounds::new(
            gpui::point(gpui::Pixels::ZERO, gpui::Pixels::ZERO),
            window_size,
        );
        assert!(
            palette_bounds_inside(physical_window, face),
            "the actual floating face is inside the live window at {scale}x: {face:?} / {physical_window:?}"
        );
        assert!(
            palette_bounds_inside(face, scroll_viewport),
            "the actual row viewport is inside the floating face at {scale}x: {scroll_viewport:?} / {face:?}"
        );
        assert!(
            palette_bounds_inside(physical_window, scroll_viewport),
            "the actual row viewport is inside the physical window at {scale}x: {scroll_viewport:?} / {physical_window:?}"
        );
        let visible = palette_bounds_intersection(
            scroll_viewport,
            palette_bounds_intersection(face, physical_window)
                .expect("the floating face intersects the physical window"),
        )
        .expect("the actual palette scroll viewport has a visible clipped region");
        let selector: &'static str =
            Box::leak(format!("ui-row-palette-row-{focused_idx}").into_boxed_str());
        let selected = window
            .debug_bounds(selector)
            .expect("the focused kit Row is painted");
        assert!(
            palette_bounds_inside(visible, selected),
            "focused row is revealed within the actual clipped viewport at {scale}x: {selected:?} / {visible:?}"
        );
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        assert!(
            !entity.read_with(window, |app, _| app.palette.open),
            "the palette closes between zoom cases"
        );
    }
    crate::theme::set_ui_scale(previous_scale);
}
