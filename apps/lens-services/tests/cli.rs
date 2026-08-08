use std::process::{Command, Stdio};

#[test]
fn deterministic_demo_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args(["--demo", "--json"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("\"schema_version\": \"2\""));
    assert!(text.contains("mosquitto.service"));
}

#[test]
fn broken_pipe_exits_cleanly() {
    let status = Command::new("bash")
        .args(["-o", "pipefail", "-c"])
        .arg(format!(
            "'{}' --demo --json | head -c 1 >/dev/null",
            env!("CARGO_BIN_EXE_lens-services")
        ))
        .stdin(Stdio::null())
        .status()
        .expect("run broken-pipe test");
    assert!(status.success());
}

#[test]
fn service_actions_require_confirmation_and_support_dry_run() {
    let rejected = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args(["--action", "restart", "--target", "ssh.service"])
        .output()
        .expect("run unconfirmed action");
    assert!(!rejected.status.success());

    let planned = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args([
            "--action",
            "restart",
            "--target",
            "ssh.service",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("run dry-run action");
    assert!(planned.status.success());
    let text = String::from_utf8_lossy(&planned.stdout);
    assert!(text.contains("\"status\": \"planned\""));
    assert!(text.contains("ssh.service"));
}

#[test]
fn scripting_assert_and_fields_work_on_demo() {
    let failed = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args(["--demo", "--active", "failed", "--fail-if-empty", "--quiet"])
        .output()
        .expect("assert failed services");
    assert_eq!(failed.status.code(), Some(0));

    let empty = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args([
            "--demo",
            "--name",
            "does-not-exist.service",
            "--fail-if-empty",
            "--quiet",
        ])
        .output()
        .expect("assert missing service");
    assert_eq!(empty.status.code(), Some(3));

    let projected = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args(["--demo", "--json", "--fields", "services"])
        .output()
        .expect("fields projection");
    assert!(projected.status.success());
    let value: serde_json::Value = serde_json::from_slice(&projected.stdout).expect("json");
    assert!(value.get("services").is_some());
    assert!(value.get("logs").is_none());
    assert!(value.get("schema_version").is_some());
    assert!(value.get("host").is_some());
}

#[test]
fn service_action_resolves_unique_name_and_rejects_ambiguous() {
    let planned = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args([
            "--demo",
            "--action",
            "restart",
            "--name",
            "mosquitto.service",
            "--match",
            "exact",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("unique name action");
    assert!(planned.status.success());
    let text = String::from_utf8_lossy(&planned.stdout);
    assert!(text.contains("mosquitto.service"));
    assert!(text.contains("\"status\": \"planned\""));

    let ambiguous = Command::new(env!("CARGO_BIN_EXE_lens-services"))
        .args([
            "--demo",
            "--action",
            "restart",
            "--filter",
            "service",
            "--dry-run",
        ])
        .output()
        .expect("ambiguous action");
    assert_eq!(ambiguous.status.code(), Some(2));
}
