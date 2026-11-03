//! Loader, selector and preamble tests over synthetic packs in temp dirs.
//!
//! The shipped `skills/fleet` pack has its own acceptance tests
//! (`fleet_pack.rs`); these pin the loader contract on edge cases.

use std::fs;
use std::path::{Path, PathBuf};

use awili_skills::{SkillPack, SkillPackError, MAX_BODY_LINES};

/// Write one well-formed skill under `dir/name/SKILL.md`.
fn write_skill(
    dir: &Path,
    name: &str,
    description: &str,
    when: &str,
    tools: &str,
    body: &str,
) -> PathBuf {
    let skill_dir = dir.join(name);
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    let text = format!(
        "---\nname: {name}\ndescription: {description}\nwhen_to_use: {when}\ntools_needed: {tools}\n---\n{body}"
    );
    fs::write(&file, text).unwrap();
    file
}

fn temp_pack(tag: &str) -> PathBuf {
    let dir = tempfile::Builder::new().prefix(tag).tempdir().unwrap();
    dir.keep()
}

fn body_with_lines(n: usize) -> String {
    (0..n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- load -----------------------------------------------------------------

#[test]
fn load_reads_skills_in_directory_order() {
    let dir = temp_pack("awili-skills-order-");
    write_skill(&dir, "bravo", "b desc", "b when", "[shell]", "b body");
    write_skill(&dir, "alpha", "a desc", "a when", "shell", "a body");

    let pack = SkillPack::load(&dir).unwrap();
    let names: Vec<&str> = pack.skills().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "bravo"]);

    let alpha = &pack.skills()[0];
    assert_eq!(alpha.description, "a desc");
    assert_eq!(alpha.when_to_use, "a when");
    assert_eq!(alpha.body, "a body");
    assert!(alpha.path.ends_with("alpha/SKILL.md"));
    assert_eq!(pack.dir(), &dir);
}

#[test]
fn load_missing_dir_is_an_error() {
    let dir = temp_pack("awili-skills-missing-");
    let ghost = dir.join("does-not-exist");
    match SkillPack::load(&ghost) {
        Err(SkillPackError::DirNotAPath(p)) => assert_eq!(p, ghost),
        other => panic!("expected DirNotAPath, got {other:?}"),
    }
}

#[test]
fn a_file_not_a_dir_is_an_error() {
    let dir = temp_pack("awili-skills-notdir-");
    let file = dir.join("SKILL.md");
    fs::write(&file, "plain file").unwrap();
    assert!(matches!(
        SkillPack::load(&file),
        Err(SkillPackError::DirNotAPath(_))
    ));
}

#[test]
fn missing_front_matter_fails_naming_the_file() {
    let dir = temp_pack("awili-skills-nofm-");
    let skill_dir = dir.join("alpha");
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(&file, "no front matter here\n").unwrap();
    match SkillPack::load(&dir) {
        Err(SkillPackError::FrontMatter { file: f }) => {
            assert_eq!(f, file);
            assert!(f.to_string_lossy().ends_with("alpha/SKILL.md"));
        }
        other => panic!("expected FrontMatter, got {other:?}"),
    }
}

#[test]
fn unclosed_front_matter_fails_naming_the_file() {
    let dir = temp_pack("awili-skills-unclosed-");
    let skill_dir = dir.join("alpha");
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(
        &file,
        "---\nname: alpha\ndescription: d\nbody never closes\n",
    )
    .unwrap();
    assert!(matches!(
        SkillPack::load(&dir),
        Err(SkillPackError::FrontMatter { .. })
    ));
}

#[test]
fn front_matter_line_without_colon_fails() {
    let dir = temp_pack("awili-skills-nocolon-");
    let skill_dir = dir.join("alpha");
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(&file, "---\nname alpha\ndescription: d\n---\nbody\n").unwrap();
    assert!(matches!(
        SkillPack::load(&dir),
        Err(SkillPackError::FrontMatter { .. })
    ));
}

#[test]
fn each_required_field_is_enforced() {
    for missing in ["name", "description", "when_to_use", "tools_needed"] {
        let dir = temp_pack("awili-skills-field-");
        let lines = ["name", "description", "when_to_use", "tools_needed"]
            .iter()
            .filter(|k| **k != missing)
            .map(|k| format!("{k}: {k}"))
            .collect::<Vec<_>>()
            .join("\n");
        let skill_dir = dir.join("alpha");
        fs::create_dir_all(&skill_dir).unwrap();
        let file = skill_dir.join("SKILL.md");
        fs::write(&file, format!("---\n{lines}\n---\nbody\n")).unwrap();
        match SkillPack::load(&dir) {
            Err(SkillPackError::MissingField { file: f, field }) => {
                assert_eq!(field, missing);
                assert_eq!(f, file);
            }
            other => panic!("missing '{missing}': expected MissingField, got {other:?}"),
        }
    }
}

#[test]
fn an_empty_field_value_counts_as_missing() {
    let dir = temp_pack("awili-skills-empty-");
    let skill_dir = dir.join("alpha");
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(
        &file,
        "---\nname: alpha\ndescription: \nwhen_to_use: w\ntools_needed: [x]\n---\nbody\n",
    )
    .unwrap();
    match SkillPack::load(&dir) {
        Err(SkillPackError::MissingField { field, .. }) => assert_eq!(field, "description"),
        other => panic!("expected MissingField(description), got {other:?}"),
    }
}

#[test]
fn name_must_match_the_directory() {
    let dir = temp_pack("awili-skills-mismatch-");
    write_skill(&dir, "alpha", "d", "w", "[x]", "body");
    let file = dir.join("alpha").join("SKILL.md");
    fs::write(
        &file,
        "---\nname: bravo\ndescription: d\nwhen_to_use: w\ntools_needed: [x]\n---\nbody\n",
    )
    .unwrap();
    match SkillPack::load(&dir) {
        Err(SkillPackError::NameMismatch {
            file: f,
            name,
            dir: d,
        }) => {
            assert_eq!(name, "bravo");
            assert_eq!(d, "alpha");
            assert_eq!(f, file);
        }
        other => panic!("expected NameMismatch, got {other:?}"),
    }
}

#[test]
fn body_over_120_lines_fails_naming_the_file() {
    let dir = temp_pack("awili-skills-long-");
    let file = write_skill(
        &dir,
        "alpha",
        "d",
        "w",
        "[x]",
        &body_with_lines(MAX_BODY_LINES + 1),
    );
    match SkillPack::load(&dir) {
        Err(SkillPackError::BodyTooLong { file: f, lines }) => {
            assert_eq!(f, file);
            assert_eq!(lines, MAX_BODY_LINES + 1);
            // The acceptance wording: the failure names the file.
            let msg = format!("{f:?}");
            assert!(msg.contains("alpha/SKILL.md"), "{msg}");
        }
        other => panic!("expected BodyTooLong, got {other:?}"),
    }
}

#[test]
fn body_at_exactly_120_lines_passes() {
    let dir = temp_pack("awili-skills-exact-");
    write_skill(
        &dir,
        "alpha",
        "d",
        "w",
        "[x]",
        &body_with_lines(MAX_BODY_LINES),
    );
    let pack = SkillPack::load(&dir).unwrap();
    assert_eq!(pack.skills().len(), 1);
}

#[test]
fn directories_without_skill_md_and_loose_files_are_ignored() {
    let dir = temp_pack("awili-skills-skip-");
    write_skill(&dir, "alpha", "d", "w", "[x]", "body");
    fs::create_dir_all(dir.join("empty-dir")).unwrap();
    fs::write(dir.join("README.md"), "loose file").unwrap();
    fs::write(dir.join("notes.txt"), "another loose file").unwrap();

    let pack = SkillPack::load(&dir).unwrap();
    let names: Vec<&str> = pack.skills().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["alpha"]);
}

#[test]
fn unknown_front_matter_keys_are_tolerated() {
    let dir = temp_pack("awili-skills-extra-");
    let skill_dir = dir.join("alpha");
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(
        &file,
        "---\nname: alpha\ndescription: d\nwhen_to_use: w\ntools_needed: [x]\nlicense: MIT\n---\nbody\n",
    )
    .unwrap();
    let pack = SkillPack::load(&dir).unwrap();
    assert_eq!(pack.skills()[0].body, "body");
}

#[test]
fn tools_needed_accepts_a_list_or_a_scalar() {
    let dir = temp_pack("awili-skills-tools-");
    write_skill(&dir, "alpha", "d", "w", "[ssh, curl]", "a");
    write_skill(&dir, "bravo", "d", "w", "git", "b");
    let pack = SkillPack::load(&dir).unwrap();
    assert_eq!(pack.skills().len(), 2);
}

#[test]
fn front_matter_comments_and_blank_lines_are_skipped() {
    let dir = temp_pack("awili-skills-comments-");
    let skill_dir = dir.join("alpha");
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(
        &file,
        "---\n# a comment\nname: alpha\n\ndescription: d\nwhen_to_use: w\ntools_needed: [x]\n---\nbody\n",
    )
    .unwrap();
    let pack = SkillPack::load(&dir).unwrap();
    assert_eq!(pack.skills()[0].description, "d");
}

#[test]
fn body_is_preserved_verbatim() {
    let dir = temp_pack("awili-skills-body-");
    let body = "```\ncurl -s base\n```\n\n- bullet one\n- bullet two";
    write_skill(&dir, "alpha", "d", "w", "[x]", body);
    let pack = SkillPack::load(&dir).unwrap();
    assert_eq!(pack.skills()[0].body, body);
}

// ---- select -----------------------------------------------------------------

fn two_skill_pack() -> (PathBuf, SkillPack) {
    let dir = temp_pack("awili-skills-select-");
    // alpha: keyword only in the name.
    write_skill(
        &dir,
        "alpha",
        "reads the alpha store",
        "use for alpha lookups",
        "[shell]",
        "alpha body",
    );
    // bravo: the shared keyword "store" only in description/when_to_use.
    write_skill(
        &dir,
        "bravo",
        "catalogues the beta store",
        "use when a beta search is needed",
        "[shell]",
        "bravo body",
    );
    let pack = SkillPack::load(&dir).unwrap();
    (dir, pack)
}

#[test]
fn select_matches_case_insensitive_keywords() {
    let (_, pack) = two_skill_pack();
    // Mixed case must not matter: ALPHA tokenizes to "alpha", the explicit
    // name hit for the alpha skill. Bravo sees only ONE meaningful token of
    // the query in its metadata ("store") — one is not enough.
    let sel = pack.select("READ the ALPHA store");
    let names: Vec<&str> = sel.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["alpha"]);
}

#[test]
fn select_ranks_name_hits_above_field_hits() {
    let (_, pack) = two_skill_pack();
    // Both skills match: alpha by NAME ("alpha") and by two metadata tokens
    // ("alpha", "store"); bravo by metadata only ("store", "search"). The
    // explicit name hit must rank first despite the equal metadata count.
    let sel = pack.select("alpha store search");
    let names: Vec<&str> = sel.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "bravo"]);
}

#[test]
fn select_empty_query_selects_nothing() {
    let (_, pack) = two_skill_pack();
    assert!(pack.select("").is_empty());
    assert!(pack.select("   ").is_empty());
}

#[test]
fn select_without_any_match_selects_nothing() {
    let (_, pack) = two_skill_pack();
    assert!(pack.select("zzz").is_empty());
}

#[test]
fn load_order_breaks_score_ties() {
    let dir = temp_pack("awili-skills-tie-");
    write_skill(&dir, "alpha", "shared keyword here", "x", "[t]", "a");
    write_skill(&dir, "bravo", "shared keyword here", "x", "[t]", "b");
    let pack = SkillPack::load(&dir).unwrap();
    // Both skills match by two distinct metadata tokens, neither by name:
    // the scores tie, so load order decides.
    let sel = pack.select("shared keyword");
    let names: Vec<&str> = sel.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "bravo"]);
}
