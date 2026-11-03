use super::*;

fn skill(name: &str, description: &str, when: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        when_to_use: when.to_string(),
        body: "body".to_string(),
        path: std::path::PathBuf::from(format!("{name}/SKILL.md")),
    }
}

#[test]
fn tokens_are_whole_alphanumeric_and_lowercased() {
    assert_eq!(
        tokens("SSH to hostd and run uptime!"),
        vec!["ssh", "to", "hostd", "and", "run", "uptime"]
    );
    // Unicode: non-ASCII letters count as alphanumeric, not separators.
    assert_eq!(tokens("Über-sicht 42"), vec!["über", "sicht", "42"]);
}

#[test]
fn meaningful_drops_stopwords_and_fragments_but_dedupes() {
    assert_eq!(meaningful_tokens("the the and a dsh dsh"), vec!["dsh"]);
    // "id" (2 chars) is a fragment; "run" (3 chars) is not a stopword.
    assert_eq!(
        meaningful_tokens("outline document id run"),
        vec!["outline", "document", "run"]
    );
    // Short tool identifiers survive despite their length class.
    for t in TOOL_IDENTITIES {
        assert_eq!(meaningful_tokens(t), vec![t.to_string()]);
    }
}

#[test]
fn explicit_name_token_selects_alone() {
    let s = skill("dsh-dispatch", "dispatch work", "use for dsh runs");
    assert!(score(&s, "dsh").selected(), "a name token is explicit");
}

#[test]
fn one_metadata_token_is_not_enough() {
    let s = skill("alpha", "reads the alpha store", "use for alpha lookups");
    assert!(
        !score(&s, "store").selected(),
        "a single metadata token must not select"
    );
}

#[test]
fn two_distinct_metadata_tokens_select() {
    let s = skill(
        "bravo",
        "catalogues the beta store",
        "use when a beta search is needed",
    );
    assert!(score(&s, "store search").selected());
}

#[test]
fn substring_never_matches() {
    let s = skill(
        "outline",
        "Outline wiki documents",
        "use for outline documents",
    );
    assert!(!score(&s, "outlining").selected());
    assert!(!score(&s, "outlines").selected());
    // But the exact token does.
    assert!(score(&s, "outline").selected());
}

#[test]
fn stopword_only_query_scores_nothing() {
    let s = skill("anything", "the fleet the store", "the the and");
    assert_eq!(
        score(&s, "what is the weather"),
        Score {
            name_hits: 0,
            meta_hits: 0
        }
    );
}

#[test]
fn ordering_is_name_hits_then_meta_then_index() {
    let skills = vec![
        // meta-only (2 hits)
        skill("bravo", "shared keyword here", "x"),
        // name hit (1)
        skill("alpha", "shared keyword here", "x"),
        // meta-only (2 hits), loaded AFTER bravo
        skill("charlie", "shared keyword here", "x"),
    ];
    let got = selected_indices(&skills, "alpha shared keyword");
    let order: Vec<usize> = got.iter().map(|(i, _)| *i).collect();
    assert_eq!(order, vec![1, 0, 2], "name hit first, then load order");
    assert_eq!(
        got[0].1,
        Score {
            name_hits: 1,
            meta_hits: 2
        }
    );
}

#[test]
fn duplicate_query_tokens_count_once() {
    let s = skill("bravo", "a shared keyword here", "x");
    // "shared shared keyword" — "shared" twice must still be ONE token.
    assert_eq!(
        score(&s, "shared shared keyword"),
        Score {
            name_hits: 0,
            meta_hits: 2
        }
    );
}
