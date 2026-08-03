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
