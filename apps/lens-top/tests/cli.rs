#![forbid(unsafe_code)]

use std::{
    fs,
    process::{Command, Stdio},
};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_lens-top")
}

#[test]
fn demo_plain_runs_without_proc() {
    let output = Command::new(binary())
        .args(["--demo", "--plain", "--limit", "3"])
        .output()
        .expect("run lens-top");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("production-gateway-04"));
    assert!(stdout.contains("image-worker"));
}

#[test]
fn demo_json_has_versioned_contract() {
    let output = Command::new(binary())
        .args(["--demo", "--json", "--filter-user", "postgres"])
        .output()
        .expect("run lens-top");
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON output");
    assert_eq!(value["schema_version"], "2");
    assert_eq!(value["host"]["hostname"], "production-gateway-04");
    assert_eq!(value["processes"].as_array().map(Vec::len), Some(1));
}

#[test]
fn invalid_threshold_is_rejected() {
    let output = Command::new(binary())
        .args(["--demo", "--plain", "--min-cpu", "-1"])
        .output()
        .expect("run lens-top");
    assert!(!output.status.success());
}

#[test]
fn config_file_is_loaded() {
    let directory = std::env::temp_dir().join(format!("lens-top-test-{}", std::process::id()));
    fs::create_dir_all(&directory).expect("create temp directory");
    let config = directory.join("config.toml");
    fs::write(
        &config,
        "refresh_interval = \"100ms\"\ndefault_sort = \"memory\"\ndefault_group = \"none\"\nvisible_columns = []\ntheme = \"default\"\ncolour_mode = \"never\"\nhistory_length = 8\nlimit = 2\n",
    )
    .expect("write config");
    let output = Command::new(binary())
        .args(["--demo", "--json", "--config"])
        .arg(&config)
        .output()
        .expect("run lens-top");
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON output");
    assert_eq!(value["processes"].as_array().map(Vec::len), Some(2));
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn broken_pipe_exits_cleanly() {
    let status = Command::new("bash")
        .arg("-o")
        .arg("pipefail")
        .arg("-c")
        .arg(format!(
            "'{}' --demo --json | head -c 1 >/dev/null",
            binary()
        ))
        .stdin(Stdio::null())
        .status()
        .expect("run broken-pipe test");
    assert!(status.success());
}

#[test]
fn no_color_plain_output_has_no_escape_sequences() {
    let output = Command::new(binary())
        .args(["--demo", "--plain", "--no-color"])
        .output()
        .expect("run lens-top");
    assert!(output.status.success());
    assert!(!output.stdout.windows(2).any(|pair| pair == b"\x1b["));
}

#[test]
fn process_actions_require_confirmation_and_support_dry_run() {
    let rejected = Command::new(binary())
        .args(["--demo", "--signal", "term", "--pid", "8421"])
        .output()
        .expect("run unconfirmed action");
    assert!(!rejected.status.success());

    let planned = Command::new(binary())
        .args([
            "--demo",
            "--signal",
            "term",
            "--pid",
            "8421",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("run dry-run action");
    assert!(planned.status.success());
    let value: serde_json::Value = serde_json::from_slice(&planned.stdout).expect("JSON outcome");
    assert_eq!(value["status"], "planned");
    assert_eq!(value["process"], "image-worker");

    let stale = Command::new(binary())
        .args([
            "--demo",
            "--signal",
            "term",
            "--pid",
            "8421",
            "--expect-start-ticks",
            "1",
            "--dry-run",
        ])
        .output()
        .expect("run action for stale process identity");
    assert!(!stale.status.success());
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("changed or exited before confirmation")
    );
}
