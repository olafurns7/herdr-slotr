#[test]
#[cfg(target_os = "linux")]
fn binary_integration() {
    let status = std::process::Command::new("python3")
        .arg("tests/integration.py")
        .env("SLOTR_BINARY", env!("CARGO_BIN_EXE_slotr"))
        .status()
        .expect("python3 (stdlib only) is required for the fake-manager integration checks");
    assert!(status.success(), "slotr binary integration checks failed");
}
