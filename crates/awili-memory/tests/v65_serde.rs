//! P4b acceptance 4: the interlock vocabulary is strict at the type level.
//! Unknown authority / epistemic / predicate strings are rejected by serde
//! (and by the strict `from_wire` parsers); `remember` serializes exactly
//! the nine provenance fields with the exact wire strings; the budget-floor
//! parsing is pinned.

use awili_memory::v65::{
    minimum_budget, Authority, EpistemicStatus, Predicate, RememberRequest, ServiceCode,
    V65ArchiveResults, V65Bootstrap, V65Item, V65Recall,
};

/// All seven P1a authorities parse from their wire strings — and nothing
/// else.
#[test]
fn authority_vocabulary_is_strict() {
    let all: [(Authority, &str); 7] = [
        (Authority::OwnerInstruction, "owner_instruction"),
        (Authority::MechanicallyVerified, "mechanically_verified"),
        (Authority::CanonicalDocumentation, "canonical_documentation"),
        (Authority::RepositoryState, "repository_state"),
        (Authority::TrustedAgentReport, "trusted_agent_report"),
        (Authority::Inference, "inference"),
        (Authority::RawHistory, "raw_history"),
    ];
    for (variant, wire) in &all {
        assert_eq!(Authority::from_wire(wire), Some(*variant));
        assert!(serde_json::from_str::<Authority>(format!("\"{wire}\"").as_str()).is_ok());
    }
    // Unknown strings are rejected — at the parser AND at the serde level.
    assert_eq!(Authority::from_wire("self_issued_mandate"), None);
    assert!(serde_json::from_str::<Authority>("\"self_issued_mandate\"").is_err());
    assert!(serde_json::from_str::<Authority>("\"\"").is_err());
}

/// Only `asserted` is a confirmed epistemic status.
#[test]
fn epistemic_vocabulary_is_strict() {
    assert_eq!(
        EpistemicStatus::from_wire("asserted"),
        Some(EpistemicStatus::Asserted)
    );
    assert!(serde_json::from_str::<EpistemicStatus>("\"asserted\"").is_ok());
    assert_eq!(EpistemicStatus::from_wire("guessed"), None);
    assert!(serde_json::from_str::<EpistemicStatus>("\"guessed\"").is_err());
    assert_eq!(EpistemicStatus::from_wire("asserted-ly"), None);
}

/// Only server-supported predicates parse; a freeform predicate is the
/// guard that routes to the non-retryable 4xx at the wire.
#[test]
fn predicate_vocabulary_is_strict() {
    assert_eq!(
        Predicate::from_wire("legacy.note"),
        Some(Predicate::LegacyNote)
    );
    assert!(serde_json::from_str::<Predicate>("\"legacy.note\"").is_ok());
    assert_eq!(Predicate::from_wire("freeform.text"), None);
    assert!(serde_json::from_str::<Predicate>("\"freeform.text\"").is_err());
    assert_eq!(Predicate::from_wire("legacy.note.x"), None);
}

/// `remember` serializes EXACTLY the nine provenance fields, with the exact
/// wire vocabulary, and round-trips losslessly (acceptance 3 + 4).
#[test]
fn remember_serializes_all_nine_provenance_fields() {
    let req = RememberRequest {
        request_id: "req-1".to_string(),
        subject: "awili.p4b".to_string(),
        predicate: Predicate::LegacyNote,
        object: serde_json::json!({"content": "note body", "importance": 0.9}),
        authority: Authority::OwnerInstruction,
        epistemic_status: EpistemicStatus::Asserted,
        source_type: "awili-app".to_string(),
        source_ref: "awili-desktop".to_string(),
        reason: "acceptance".to_string(),
    };
    let v = serde_json::to_value(&req).unwrap();
    let expected = serde_json::json!({
        "request_id": "req-1",
        "subject": "awili.p4b",
        "predicate": "legacy.note",
        "object": { "content": "note body", "importance": 0.9 },
        "authority": "owner_instruction",
        "epistemic_status": "asserted",
        "source_type": "awili-app",
        "source_ref": "awili-desktop",
        "reason": "acceptance"
    });
    assert_eq!(v, expected);
    let round: RememberRequest = serde_json::from_value(v).unwrap();
    assert_eq!(round, req);
}

/// The service-code classifier is total and stable.
#[test]
fn service_codes_classify() {
    assert_eq!(
        ServiceCode::from_wire("budget_too_small"),
        ServiceCode::BudgetTooSmall
    );
    assert_eq!(
        ServiceCode::from_wire("predicate_not_allowed"),
        ServiceCode::PredicateNotAllowed
    );
    assert_eq!(ServiceCode::from_wire("pii_guard"), ServiceCode::PiiGuard);
    assert_eq!(
        ServiceCode::from_wire("something_else"),
        ServiceCode::Unknown
    );
}

/// The budget floor comes out of the 422 body — and only from it.
#[test]
fn minimum_budget_parsing() {
    assert_eq!(
        minimum_budget(r#"{"error":{"code":"budget_too_small","minimum_token_budget":11080}}"#),
        Some(11080)
    );
    assert_eq!(minimum_budget("not json"), None);
    assert_eq!(minimum_budget(r#"{"error":{"code":"other"}}"#), None);
}

/// Response envelopes tolerate extra envelope fields, but the `mandatory_policy`
/// lane is REQUIRED (S5(a)): a missing lane is a parse failure.
#[test]
fn envelopes_are_tolerant() {
    assert!(serde_json::from_str::<V65Recall>("{}").is_err());
    assert!(serde_json::from_str::<V65Bootstrap>("{}").is_err());
    let empty: V65Recall = serde_json::from_str(r#"{"mandatory_policy": []}"#).unwrap();
    assert!(empty.mandatory_policy.is_empty());
    assert!(empty.items.is_empty());
    let empty_boot: V65Bootstrap = serde_json::from_str(r#"{"mandatory_policy": []}"#).unwrap();
    assert!(empty_boot.items.is_empty());

    let full: V65Recall = serde_json::from_str(
        r#"{
            "mandatory_policy": [
                {"id": "p1", "subject": "s", "object": "text", "unknown_field": 1}
            ],
            "items": [{"id": "i1", "object": {"content": "c"}}],
            "envelope_version": 2,
            "debug": {"trace": true}
        }"#,
    )
    .unwrap();
    assert_eq!(full.mandatory_policy[0].object, serde_json::json!("text"));
    assert_eq!(full.items[0].object["content"], "c");

    let item: V65Item = serde_json::from_str(r#"{"object": {"content": "x"}}"#).unwrap();
    assert_eq!(item.id, None);
    assert_eq!(item.kind, None);

    let hits: V65ArchiveResults = serde_json::from_str(r#"{"results": []}"#).unwrap();
    assert!(hits.results.is_empty());
}
