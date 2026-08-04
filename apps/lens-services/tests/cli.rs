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
