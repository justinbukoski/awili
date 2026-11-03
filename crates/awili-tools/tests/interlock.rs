//! P4b acceptance for the interlock tool definitions: the four verbs under
//! `connector:interlock` with canonical risk floors, strict argument
//! parsing (freeform vocabulary refused at the tool boundary), and
//! lane-labeled, pairwise-disjoint result shapes.

use std::collections::BTreeSet;

use awili_engine::RiskClass;
use awili_tools::interlock::{
    archive_result, read_result, remember_result, InterlockCall, InterlockParseError,
    ARCHIVE_LIMIT_MAX, ARCHIVE_SEARCH, AUTHORITIES, BOOTSTRAP, EPISTEMIC_STATUSES, EXECUTOR,
    LANE_ARCHIVE_EVIDENCE, LANE_CANONICAL_MEMORY, LANE_MANDATORY_POLICY, LANE_RECALL_ITEMS,
    MAX_PARSE_ERROR_CHARS, ORIGIN, PREDICATES, RECALL, REMEMBER,
};
use awili_tools::{AllowAllValidator, RegistryErrorKind, ToolRegistry};
use serde_json::{json, Value};

#[test]
fn defs_cover_the_four_verbs_with_canonical_risk() {
    let defs = awili_tools::interlock::defs();
    assert_eq!(defs.len(), 4);
    for d in &defs {
        assert_eq!(d.identity.origin.origin, ORIGIN);
        assert_eq!(d.descriptor.executor, EXECUTOR);
    }
    let want = [
        (BOOTSTRAP, "read_only", RiskClass::ReadOnly),
        (RECALL, "read_only", RiskClass::ReadOnly),
        (REMEMBER, "mutating", RiskClass::Mutating),
        (ARCHIVE_SEARCH, "read_only", RiskClass::ReadOnly),
    ];
    for (name, side_effect, risk) in want {
        let d = defs.iter().find(|d| d.name() == name).expect("def present");
        assert_eq!(d.descriptor.side_effect, side_effect, "risk class {name}");
        assert_eq!(d.descriptor.risk_floor(), risk, "risk floor {name}");
        // The schema a provider sees is the strict object shape.
        let schema = &d.descriptor.schema;
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
    }
}

#[test]
fn schemas_carry_the_exact_wire_vocabulary() {
    let defs = awili_tools::interlock::defs();
    let remember = defs
        .iter()
        .find(|d| d.name() == REMEMBER)
        .expect("remember def");
    let props = &remember.descriptor.schema["properties"];
    let single_enum = |name: &str| {
        props[name]["enum"].as_array().unwrap()[0]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(single_enum("predicate"), PREDICATES[0]);
    let auth_enum: Vec<&str> = props["authority"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(auth_enum, AUTHORITIES);
    assert_eq!(single_enum("epistemic_status"), EPISTEMIC_STATUSES[0]);
    assert_eq!(props["object"]["type"], "object");
    let archive = defs
        .iter()
        .find(|d| d.name() == ARCHIVE_SEARCH)
        .expect("archive def");
    let limit = &archive.descriptor.schema["properties"]["limit"];
    assert_eq!(limit["minimum"].as_u64().unwrap(), 1);
    assert_eq!(limit["maximum"].as_u64().unwrap(), ARCHIVE_LIMIT_MAX as u64);
    let required: Vec<&str> = remember.descriptor.schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required.len(), 9, "every provenance field is required");
}

#[test]
fn install_registers_all_four_and_duplicate_is_refused() {
    let mut reg = ToolRegistry::with_validator(AllowAllValidator);
    awili_tools::interlock::install(&mut reg).expect("first install");
    assert_eq!(reg.len(), 4);
    assert_eq!(
        awili_tools::interlock::install(&mut reg).unwrap_err().kind,
        RegistryErrorKind::Duplicate
    );
}

#[test]
fn bootstrap_parses_only_an_empty_object() {
    let parse = awili_tools::interlock::parse;
    assert_eq!(parse(BOOTSTRAP, &json!({})), Ok(InterlockCall::Bootstrap));
    for bad in [json!({"query": "x"}), Value::Null] {
        assert!(
            matches!(parse(BOOTSTRAP, &bad), Err(InterlockParseError::Invalid(_))),
            "bootstrap must refuse {bad:?}"
        );
    }
}

fn remember_args() -> Value {
    json!({
        "request_id": "req-1", "subject": "awili.p4b", "predicate": "legacy.note",
        "object": {"content": "shipped"}, "authority": "owner_instruction",
        "epistemic_status": "asserted", "source_type": "awili-app",
        "source_ref": "awili-desktop", "reason": "P4b",
    })
}

#[test]
fn remember_parses_full_provenance_and_rejects_foreign_vocab() {
    let parse = awili_tools::interlock::parse;
    let Ok(InterlockCall::Remember(call)) = parse(REMEMBER, &remember_args()) else {
        panic!("a valid remember must parse");
    };
    assert_eq!(call.request_id, "req-1");
    assert_eq!(call.object, json!({"content": "shipped"}));
    let reject = |mutate: fn(&mut Value)| {
        let mut v = remember_args();
        mutate(&mut v);
        assert!(
            matches!(parse(REMEMBER, &v), Err(InterlockParseError::Invalid(_))),
            "remember must refuse the mutated shape"
        );
    };
    reject(|v| v["predicate"] = json!("freeform.text"));
    reject(|v| v["authority"] = json!("guessed"));
    reject(|v| v["epistemic_status"] = json!("guessed"));
    reject(|v| v["object"] = json!(["not", "an", "object"]));
    reject(|v| v["extra"] = json!("nope"));
}

#[test]
fn archive_search_bounds_the_limit() {
    let parse = awili_tools::interlock::parse;
    assert!(parse(ARCHIVE_SEARCH, &json!({"query": "q", "limit": 1})).is_ok());
    assert!(parse(
        ARCHIVE_SEARCH,
        &json!({"query": "q", "limit": ARCHIVE_LIMIT_MAX})
    )
    .is_ok());
    for lim in [0usize, ARCHIVE_LIMIT_MAX + 1] {
        assert!(
            matches!(
                parse(ARCHIVE_SEARCH, &json!({"query": "q", "limit": lim})),
                Err(InterlockParseError::Invalid(_))
            ),
            "limit {lim} must be refused"
        );
    }
}

#[test]
fn unknown_tool_names_are_classified() {
    assert_eq!(
        awili_tools::interlock::parse("interlock.wipe", &json!({})),
        Err(InterlockParseError::UnknownTool("interlock.wipe".into()))
    );
}

/// Errors render the stable class only — bounded, no tokens, no freeform
/// prose beyond the parse detail.
#[test]
fn parse_errors_are_stable_and_bounded() {
    let e = awili_tools::interlock::parse(REMEMBER, &json!({})).unwrap_err();
    let s = format!("{e}");
    assert!(s.starts_with("invalid interlock arguments:"));
    assert!(s.len() < MAX_PARSE_ERROR_CHARS);
    let u = InterlockParseError::UnknownTool("x".into());
    assert!(format!("{u}").starts_with("unknown interlock tool:"));
}

#[test]
fn result_lanes_are_labeled_and_disjoint() {
    let read = read_result(&json!(["policy"]), &json!([{"id": "it-1"}]));
    let remember = remember_result(Some("id-1"));
    let archive = archive_result(&json!([{"id": "arch-1"}]));
    let labels = |v: &Value| -> BTreeSet<String> {
        v["lanes"]
            .as_array()
            .expect("lanes array")
            .iter()
            .map(|l| l.as_str().expect("lane label").to_string())
            .collect()
    };
    let (r, w, a) = (labels(&read), labels(&remember), labels(&archive));
    assert_eq!(r.intersection(&w).count(), 0, "read vs remember lanes");
    assert_eq!(r.intersection(&a).count(), 0, "read vs archive lanes");
    assert_eq!(w.intersection(&a).count(), 0, "remember vs archive lanes");
    assert_eq!(
        r,
        BTreeSet::from([
            LANE_MANDATORY_POLICY.to_string(),
            LANE_RECALL_ITEMS.to_string()
        ])
    );
    assert!(read.get(LANE_MANDATORY_POLICY).is_some());
    assert!(read.get(LANE_RECALL_ITEMS).is_some());
    assert_eq!(remember[LANE_CANONICAL_MEMORY]["stored_id"], json!("id-1"));
    assert!(archive.get(LANE_ARCHIVE_EVIDENCE).is_some());
}
