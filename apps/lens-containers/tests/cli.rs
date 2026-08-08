use std::process::{Command, Stdio};

#[test]
fn deterministic_demo_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args(["--demo", "--json"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("\"schema_version\": \"2\""));
    assert!(text.contains("edge-mqtt"));
    assert!(text.contains("\"runtime\": \"docker\""));
    assert!(text.contains("metrics-agent"));
}

#[test]
fn broken_pipe_exits_cleanly() {
    let status = Command::new("bash")
        .args(["-o", "pipefail", "-c"])
        .arg(format!(
            "'{}' --demo --json | head -c 1 >/dev/null",
            env!("CARGO_BIN_EXE_lens-containers")
        ))
        .stdin(Stdio::null())
        .status()
        .expect("run broken-pipe test");
    assert!(status.success());
}

#[test]
fn container_actions_require_confirmation_and_support_dry_run() {
    let rejected = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args(["--demo", "--action", "restart", "--target", "edge-mqtt"])
        .output()
        .expect("run unconfirmed action");
    assert!(!rejected.status.success());

    let planned = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args([
            "--demo",
            "--action",
            "restart",
            "--target",
            "edge-mqtt",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("run dry-run action");
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let text = String::from_utf8_lossy(&planned.stdout);
    assert!(text.contains("\"status\": \"planned\""));
    assert!(text.contains("edge-mqtt") || text.contains("a1b2c3d4e5f6789012345678abcdef01"));
    assert!(text.contains("\"runtime\": \"docker\""));
}

#[test]
fn scripting_assert_and_fields_work_on_demo() {
    let empty = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args([
            "--demo",
            "--name",
            "does-not-exist",
            "--fail-if-empty",
            "--quiet",
        ])
        .output()
        .expect("assert missing container");
    assert_eq!(empty.status.code(), Some(3));

    let projected = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args(["--demo", "--json", "--fields", "containers"])
        .output()
        .expect("fields projection");
    assert!(projected.status.success());
    let value: serde_json::Value = serde_json::from_slice(&projected.stdout).expect("json");
    assert!(value.get("containers").is_some());
    assert!(value.get("services").is_none());
    assert!(value.get("schema_version").is_some());
    assert!(value.get("host").is_some());
}

#[test]
fn container_action_resolves_unique_name_and_rejects_ambiguous() {
    let planned = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args([
            "--demo",
            "--action",
            "restart",
            "--name",
            "edge-mqtt",
            "--match",
            "exact",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("unique name action");
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let text = String::from_utf8_lossy(&planned.stdout);
    assert!(text.contains("\"status\": \"planned\""));

    let ambiguous = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args([
            "--demo",
            "--action",
            "restart",
            "--filter",
            "e",
            "--dry-run",
        ])
        .output()
        .expect("ambiguous action");
    assert_eq!(ambiguous.status.code(), Some(2));
}

#[test]
fn enable_action_rejected_for_containers() {
    let rejected = Command::new(env!("CARGO_BIN_EXE_lens-containers"))
        .args([
            "--demo",
            "--action",
            "enable",
            "--target",
            "edge-mqtt",
            "--dry-run",
        ])
        .output()
        .expect("enable rejected");
    assert_eq!(rejected.status.code(), Some(2));
}
