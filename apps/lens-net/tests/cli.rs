use std::process::Command;

#[test]
fn deterministic_demo_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-net"))
        .args(["--demo", "--plain"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("INTERFACES"));
    assert!(text.contains("LISTENERS"));
}

#[test]
fn listening_port_assert_works_on_demo() {
    let ok = Command::new(env!("CARGO_BIN_EXE_lens-net"))
        .args([
            "--demo",
            "--listening",
            "--port",
            "1883",
            "--expect-count-min",
            "1",
            "--quiet",
        ])
        .output()
        .expect("port assert");
    assert_eq!(ok.status.code(), Some(0));

    let missing = Command::new(env!("CARGO_BIN_EXE_lens-net"))
        .args([
            "--demo",
            "--listening",
            "--port",
            "65530",
            "--fail-if-empty",
            "--quiet",
        ])
        .output()
        .expect("missing port");
    assert_eq!(missing.status.code(), Some(3));
}
