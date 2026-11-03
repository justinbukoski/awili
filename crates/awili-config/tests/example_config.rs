#[test]
fn example_config_parses_with_new_sections() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config.example.toml"
    ))
    .expect("config.example.toml readable");
    let cfg = awili_config::Config::parse(&raw).expect("example config must parse");
    assert!(cfg.models.claude_aliases.contains(&"opus".to_string()));
    assert!(cfg.models.non_chat_filters.iter().any(|f| f == "whisper"));
    // RC1: the example REPLACES the default list, so it must keep ~/.local/bin
    // (vendor CLIs, dsh and awili-connectors-mcp live there on this fleet).
    assert_eq!(cfg.work.agents.search_paths.len(), 4);
    assert!(cfg
        .work
        .agents
        .search_paths
        .iter()
        .any(|p| p == "~/.local/bin"));
    assert!(
        cfg.work.agents.claude_bin.is_none(),
        "example must ship no explicit bin"
    );
}
