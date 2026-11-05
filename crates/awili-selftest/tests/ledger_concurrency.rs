use awili_selftest::graduation::Ledger;
use std::process::Command;

#[test]
fn simultaneous_cli_appends_are_readable() {
    let root = std::env::temp_dir().join(format!("awili-ledger-process-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let ledger = root.join("ledger.jsonl");
    let mut children = Vec::new();
    for n in 0..24 {
        children.push(Command::new(env!("CARGO_BIN_EXE_awili-selftest"))
            .args(["graduation", "append", "--ledger"]).arg(&ledger)
            .args(["--task-id", &format!("task-{n}"), "--executor", "local", "--model", "fixture", "--outcome", "success", "--frontier-tokens", "1", "--local-tokens", "1"])
            .spawn().unwrap());
    }
    for mut child in children { assert!(child.wait().unwrap().success()); }
    assert_eq!(Ledger::read(&ledger).unwrap().len(), 24);
    std::fs::remove_dir_all(root).unwrap();
}
