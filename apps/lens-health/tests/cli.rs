use std::process::Command;

#[test]
fn deterministic_demo_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-health"))
        .args(["--demo", "--json"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("services.failed"));
    assert!(text.contains("net.unexpected-listeners"));
}

#[test]
fn fail_on_critical_exits_three_without_jq() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-health"))
        .args(["--demo", "--fail-on", "critical", "--quiet"])
        .output()
        .expect("fail-on critical");
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn jsonl_emits_finding_records() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-health"))
        .args(["--demo", "--jsonl"])
        .output()
        .expect("jsonl");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("\"record_type\":\"finding\"")
            || text.contains("\"record_type\": \"finding\"")
    );
}
