use std::process::Command;

#[test]
fn demo_plain_and_json_are_stable() {
    let binary = env!("CARGO_BIN_EXE_lens");
    let plain = Command::new(binary)
        .args(["--demo", "--plain"])
        .output()
        .expect("plain");
    assert!(plain.status.success());
    assert!(String::from_utf8_lossy(&plain.stdout).contains("production-gateway-04"));
    let json = Command::new(binary)
        .args(["--demo", "--json"])
        .output()
        .expect("json");
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(value["schema_version"], "2");
}

#[test]
fn ai_focus_renders_authoritative_diagnostics_and_divergence() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args([
            "--demo",
            "--focus",
            "ai",
            "--model",
            "vision-v4",
            "--runtime",
            "inference-main",
            "--accelerator",
            "pci-0000:01:00.0",
            "--source",
            "camera/front",
        ])
        .output()
        .expect("ai focus");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("AI / Accelerator diagnostics"));
    assert!(text.contains("pci-0000:01:00.0"));
    assert!(text.contains("cache pressure"));
    assert!(text.contains("DIVERGED"));
    assert!(text.contains("permission denied"));
    assert!(text.contains("stale"));
}

#[test]
fn ai_focus_json_is_queryable_and_preserves_stable_ids() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["--demo", "--focus", "ai", "--json", "--fields", "ai"])
        .output()
        .expect("ai JSON");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(
        value["ai"]["accelerators"][0]["stable_id"],
        "pci-0000:01:00.0"
    );
    assert_eq!(value["ai"]["runtimes"][0]["active_model"], "vision-v4");
    assert_eq!(value["ai"]["runtimes"][0]["loaded_model"], "vision-v3");
}

#[test]
fn ai_state_override_rejects_relative_paths() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["--focus", "ai", "--agent-state", "relative.json"])
        .output()
        .expect("relative override");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("absolute path"));
}
