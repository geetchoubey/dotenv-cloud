//! Integration tests for the `dotenv-cloud` binary (spec §21.2).

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dotenv-cloud"))
}

/// Write a `.env` into a fresh temp dir and return the dir.
fn temp_project(env: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), env).unwrap();
    dir
}

#[test]
fn run_injects_env_and_passes_exit_code() {
    let dir = temp_project("FOO=bar\nPORT=3000\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "run", "--", "sh", "-c", "test \"$FOO\" = bar"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn run_propagates_nonzero_child_exit() {
    let dir = temp_project("FOO=bar\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "run", "--", "sh", "-c", "exit 42"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(42));
}

#[test]
fn run_without_separator_is_usage_error() {
    let dir = temp_project("FOO=bar\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["run"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn missing_provider_fails_closed() {
    let dir = temp_project("DB_PASSWORD=aws-sm://prod/db/password\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "run", "--", "true"])
        .output()
        .unwrap();
    // No provider installed -> ProviderUnavailable -> exit 7.
    assert_eq!(out.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("[redacted]"), "reference should be redacted: {stderr}");
    assert!(!stderr.contains("password\n"), "raw secret path leaked: {stderr}");
}

#[test]
fn cli_set_overrides_env_file() {
    let dir = temp_project("PORT=3000\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "--set", "PORT=9999", "run", "--", "printenv", "PORT"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "9999");
}

#[test]
fn malformed_dotenv_reports_parse_error() {
    let dir = temp_project("this is not valid\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "run", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
}
