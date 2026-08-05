use std::process::Command;

#[test]
fn deterministic_demo_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_lens-hardware"))
        .args(["--demo", "--plain"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Raspberry Pi 4 Model B"));
    assert!(text.contains("TEMPERATURES"));
    assert!(text.contains("Quectel LTE modem"));
}
