use std::process::Command;

#[test]
fn version_prints_exact_release() {
    let output = Command::new(env!("CARGO_BIN_EXE_loupe-term"))
        .arg("--version")
        .env_remove("LOUPE_NO_THEMES")
        .output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, format!("loupe {}\n", env!("CARGO_PKG_VERSION")).as_bytes());
    assert!(output.stderr.is_empty());
}
