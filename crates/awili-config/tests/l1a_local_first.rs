//! L1a proof tests: `[engine].label`, `[work].cloud_escalation` and the
//! `EngineConfig` display/endpoint helpers.

use awili_config::{Config, EngineConfig};

#[test]
fn l1a_display_label_prefers_label_then_model_tail() {
    let eng = EngineConfig {
        label: "Gemma 4 12B (Studio)".to_string(),
        model: "gemma-4-12b-it-qat-mlx-4bit".to_string(),
        ..Default::default()
    };
    assert_eq!(eng.display_label(), "Gemma 4 12B (Studio)");

    let blank = EngineConfig {
        model: "/Users/x/models/gemma-4-12b-it-qat-mlx-4bit".to_string(),
        ..Default::default()
    };
    assert_eq!(blank.display_label(), "gemma-4-12b-it-qat-mlx-4bit");

    let empty = EngineConfig::default();
    assert_eq!(empty.display_label(), "no model set");
}

#[test]
fn l1a_is_configured_matrix() {
    let blank_base = EngineConfig {
        model: "m".to_string(),
        ..Default::default()
    };
    assert!(
        !blank_base.is_configured(),
        "blank base_url is not configured"
    );

    let blank_model = EngineConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        model: "   ".to_string(),
        ..Default::default()
    };
    assert!(
        !blank_model.is_configured(),
        "whitespace model is not configured"
    );

    let both = EngineConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        model: "m".to_string(),
        ..Default::default()
    };
    assert!(both.is_configured());
}

#[test]
fn l1a_endpoint_host() {
    let ip = EngineConfig {
        base_url: "http://192.0.2.10:8851".to_string(),
        ..Default::default()
    };
    assert_eq!(ip.endpoint_host().as_deref(), Some("192.0.2.10"));

    let name = EngineConfig {
        base_url: "http://studio.internal/x".to_string(),
        ..Default::default()
    };
    assert_eq!(name.endpoint_host().as_deref(), Some("studio.internal"));

    let empty = EngineConfig::default();
    assert_eq!(empty.endpoint_host(), None);

    let nonsense = EngineConfig {
        base_url: "nonsense".to_string(),
        ..Default::default()
    };
    assert_eq!(nonsense.endpoint_host(), None);
}

#[test]
fn l1a_label_absent_does_not_serialize() {
    let cfg = Config::parse("[engine]\nbase_url = \"http://h:1\"\n").unwrap();
    let s = toml::to_string(&cfg.engine).unwrap();
    assert!(!s.contains("label"), "absent label must not serialize: {s}");

    let cfg = Config::parse(
        "[engine]\nbase_url = \"http://h:1\"\nmodel = \"m\"\nlabel = \"Gemma 4 12B (Studio)\"\n",
    )
    .unwrap();
    assert_eq!(cfg.engine.label, "Gemma 4 12B (Studio)");
    let s = toml::to_string(&cfg.engine).unwrap();
    let again = toml::from_str::<EngineConfig>(&s).unwrap();
    assert_eq!(again.label, "Gemma 4 12B (Studio)");
}

#[test]
fn l1a_cloud_escalation_is_optional() {
    let cfg = Config::parse("").unwrap();
    assert_eq!(cfg.work.cloud_escalation, None);

    let cfg = Config::parse(
        r#"
        [work]
        cloud_escalation = "agent:claude:sonnet"
        "#,
    )
    .unwrap();
    assert_eq!(
        cfg.work.cloud_escalation.as_deref(),
        Some("agent:claude:sonnet")
    );
    let s = toml::to_string(&cfg.work).unwrap();
    let again = toml::from_str::<awili_config::WorkConfig>(&s).unwrap();
    assert_eq!(
        again.cloud_escalation.as_deref(),
        Some("agent:claude:sonnet")
    );
}

#[test]
fn l1a_debug_shows_label_redacts_key() {
    let eng = EngineConfig {
        label: "Gemma 4 12B (Studio)".to_string(),
        api_key: "sk-a-real-looking-secret-value".to_string(),
        ..Default::default()
    };
    let d = format!("{eng:?}");
    assert!(
        d.contains("Gemma 4 12B (Studio)"),
        "Debug must show the label: {d}"
    );
    assert!(
        !d.contains("sk-a-real-looking-secret-value"),
        "Debug must not leak the api_key: {d}"
    );
}
