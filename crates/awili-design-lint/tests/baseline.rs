//! Baseline semantics: pass; a new finding fails; a duplicate line gets a
//! distinct index; `--update-baseline` drops removed findings and refuses to
//! add.

use std::fs;
use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("awili-design-lint-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A src dir with one file holding exactly one finding, plus a matching
/// baseline (written with the one-shot `init_baseline`).
fn setup(name: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(name);
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.rs"),
        "pub fn f() { let _ = gpui::rgb(0x111111); }\n",
    )
    .unwrap();
    let baseline = dir.join("baseline.toml");
    assert_eq!(
        awili_design_lint::init_baseline(&src, &baseline).unwrap(),
        1
    );
    (src, baseline)
}

#[test]
fn check_passes_on_matching_baseline() {
    let (src, baseline) = setup("check-pass");
    let outcome = awili_design_lint::check(&src, &baseline).unwrap();
    assert!(outcome.new.is_empty(), "no new findings: {:?}", outcome.new);
    assert!(
        outcome.stale.is_empty(),
        "no stale entries: {:?}",
        outcome.stale
    );
}

#[test]
fn a_new_finding_is_reported_and_fails() {
    let (src, baseline) = setup("check-new");
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();
    let outcome = awili_design_lint::check(&src, &baseline).unwrap();
    assert_eq!(outcome.new.len(), 1);
    let f = &outcome.new[0];
    assert_eq!(f.path, "b.rs");
    assert_eq!(f.kind, "raw_color_fn");
    assert_eq!(f.line, 1);
    assert!(
        f.fingerprint.starts_with("b.rs|raw_color_fn|"),
        "fingerprint shape: {}",
        f.fingerprint
    );
    assert!(outcome.stale.is_empty());
}

#[test]
fn a_duplicate_line_gets_distinct_indices() {
    let (src, _) = setup("check-dup");
    fs::write(
        src.join("c.rs"),
        "pub fn d() {\n    let _x = gpui::rgba(0x00000000);\n    let _x = gpui::rgba(0x00000000);\n    _x\n}\n",
    )
    .unwrap();
    let findings = awili_design_lint::scan(&src).unwrap();
    let dup: Vec<&awili_design_lint::Finding> =
        findings.iter().filter(|f| f.path == "c.rs").collect();
    assert_eq!(dup.len(), 2);
    assert_eq!(dup[0].text, dup[1].text);
    assert_ne!(dup[0].fingerprint, dup[1].fingerprint);
    assert!(dup[0].fingerprint.ends_with("|0"));
    assert!(dup[1].fingerprint.ends_with("|1"));
}

#[test]
fn update_baseline_drops_removed_findings() {
    let (src, baseline) = setup("update-drop");
    // Grow the tree to a second finding and record it, so the baseline holds
    // two entries.
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();
    fs::write(
        &baseline,
        awili_design_lint::baseline_to_toml(&awili_design_lint::Baseline {
            version: awili_design_lint::BASELINE_VERSION,
            findings: {
                let mut v = awili_design_lint::scan(&src)
                    .unwrap()
                    .into_iter()
                    .map(|f| f.fingerprint)
                    .collect::<Vec<_>>();
                v.sort();
                v
            },
        }),
    )
    .unwrap();
    let before = fs::read_to_string(&baseline).unwrap();
    assert_eq!(
        before.matches('\"').count() / 2,
        2,
        "baseline starts with two entries"
    );

    // Remove the finding (the file goes away) and update: the entry is
    // dropped, the file is rewritten.
    fs::remove_file(src.join("b.rs")).unwrap();
    let kept = awili_design_lint::update_baseline(&src, &baseline).unwrap();
    assert_eq!(kept, 1);
    let after = fs::read_to_string(&baseline).unwrap();
    assert_ne!(before, after, "the baseline was rewritten");
    let b: awili_design_lint::Baseline = toml::from_str(&after).unwrap();
    assert_eq!(b.findings.len(), 1);
    assert!(b.findings[0].starts_with("a.rs|"));
}

#[test]
fn update_baseline_refuses_to_add_and_writes_nothing() {
    let (src, baseline) = setup("update-refuse");
    let before = fs::read_to_string(&baseline).unwrap();
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();

    let err = awili_design_lint::update_baseline(&src, &baseline).unwrap_err();
    assert!(
        err.to_string().contains("refusing to update baseline"),
        "{err:?}"
    );
    assert_eq!(
        before,
        fs::read_to_string(&baseline).unwrap(),
        "no write on refusal"
    );
}

#[test]
fn init_is_one_shot() {
    let (src, baseline) = setup("init-once");
    let err = awili_design_lint::init_baseline(&src, &baseline).unwrap_err();
    assert!(err.to_string().contains("--init is one-shot"), "{err:?}");
}

#[test]
fn an_unknown_baseline_version_is_rejected() {
    let (src, baseline) = setup("version");
    fs::write(&baseline, "version = 99\nfindings = []\n").unwrap();
    let err = awili_design_lint::check(&src, &baseline).unwrap_err();
    assert!(
        err.to_string().contains("unsupported baseline version"),
        "{err:?}"
    );
}

// --- ratchet safety (docs/v2/slices/02b-lint-fixes.md item 9): the working
// baseline can only SHRINK against the committed one (`--reference`).

/// Grow a baseline by hand: add a finding to the tree and append its
/// fingerprint to the baseline (bypassing `--update-baseline`).
fn grow_baseline_by_hand(src: &Path, baseline: &Path) {
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();
    let mut fps: Vec<String> = awili_design_lint::scan(src)
        .unwrap()
        .into_iter()
        .map(|f| f.fingerprint)
        .collect();
    fps.sort();
    fs::write(
        baseline,
        awili_design_lint::baseline_to_toml(&awili_design_lint::Baseline {
            version: awili_design_lint::BASELINE_VERSION,
            findings: fps,
        }),
    )
    .unwrap();
}

#[test]
fn a_baseline_entry_not_in_the_reference_is_a_violation() {
    let (src, baseline) = setup("ref-violations");
    let reference = baseline.parent().unwrap().join("reference.toml");
    // The "committed" reference holds exactly the current (one) entry.
    fs::copy(&baseline, &reference).unwrap();
    // The working baseline grows: a new finding plus a hand-added entry.
    grow_baseline_by_hand(&src, &baseline);

    let base: awili_design_lint::Baseline =
        toml::from_str(&fs::read_to_string(&baseline).unwrap()).unwrap();
    let refb: awili_design_lint::Baseline =
        toml::from_str(&fs::read_to_string(&reference).unwrap()).unwrap();
    let v = awili_design_lint::reference_violations(&base, &refb);
    assert_eq!(v.len(), 1, "exactly the hand-added entry: {v:?}");
    assert!(
        v[0].starts_with("b.rs|raw_color_fn|"),
        "the violating entry is the grown one: {v:?}"
    );

    // Shrink-only: the reference (or anything with a superset of the entries)
    // has no violation against the smaller working baseline.
    let none = awili_design_lint::reference_violations(&refb, &base);
    assert!(none.is_empty(), "shrink is never a violation: {none:?}");
}

#[test]
fn kind_totals_cover_every_finding() {
    let dir = temp_dir("totals");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("x.rs"),
        "pub fn f() { let _ = gpui::rgb(0x111111); let _ = px(2.); }\n",
    )
    .unwrap();
    let findings = awili_design_lint::scan(&dir).unwrap();
    let totals: usize = awili_design_lint::kind_totals(&findings)
        .iter()
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(totals, findings.len());
    let t = awili_design_lint::kind_totals(&findings);
    assert_eq!(t.iter().find(|(k, _)| *k == "raw_color_fn").unwrap().1, 1);
    assert_eq!(t.iter().find(|(k, _)| *k == "raw_px").unwrap().1, 1);
}
