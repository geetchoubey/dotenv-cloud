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
        .args([
            "--no-env-local",
            "run",
            "--",
            "sh",
            "-c",
            "test \"$FOO\" = bar",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let dir = temp_project("DB_PASSWORD=aws-secrets://prod/db/password\n");
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "run", "--", "true"])
        .output()
        .unwrap();
    // No provider installed -> ProviderUnavailable -> exit 7.
    assert_eq!(out.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[redacted]"),
        "reference should be redacted: {stderr}"
    );
    assert!(
        !stderr.contains("password\n"),
        "raw secret path leaked: {stderr}"
    );
}

#[test]
fn remote_failure_falls_back_to_environment_default() {
    let dir = temp_project("DB_PASSWORD=aws-secrets://prod/db/password\n");
    // No provider is installed, so resolution fails — but an environment default
    // exists for the same key, so it should be used instead of failing closed.
    std::fs::write(
        dir.path().join("dotenv-cloud.toml"),
        "default_environment = \"dev\"\n\n[environment.dev.defaults]\nDB_PASSWORD = \"local-default\"\n",
    )
    .unwrap();
    let out = bin()
        .current_dir(dir.path())
        .args(["--no-env-local", "run", "--", "printenv", "DB_PASSWORD"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "should succeed via fallback; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "local-default");
    // The fallback is announced on stderr, and the reference stays redacted.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("using environment default"),
        "expected fallback warning: {stderr}"
    );
}

#[test]
fn cli_set_overrides_env_file() {
    let dir = temp_project("PORT=3000\n");
    let out = bin()
        .current_dir(dir.path())
        .args([
            "--no-env-local",
            "--set",
            "PORT=9999",
            "run",
            "--",
            "printenv",
            "PORT",
        ])
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

#[test]
fn export_emits_only_project_env_not_system() {
    let dir = temp_project("FOO=bar\n");
    let out = bin()
        .current_dir(dir.path())
        .env("DC_SYSTEM_ONLY", "leak")
        .args(["--no-env-local", "export"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("FOO="), "project key should be present");
    assert!(
        !stdout.contains("DC_SYSTEM_ONLY"),
        "system-only var must not leak into export: {stdout}"
    );
}

#[test]
fn clear_env_drops_system_but_keeps_project() {
    let dir = temp_project("FOO=bar\n");
    let out = bin()
        .current_dir(dir.path())
        .env("DC_SYSTEM_ONLY", "leak")
        .args([
            "--no-env-local",
            "run",
            "--clear-env",
            "--",
            "sh",
            "-c",
            "echo FOO=$FOO SYS=[$DC_SYSTEM_ONLY]",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "FOO=bar SYS=[]"
    );
}

#[test]
fn clear_env_preserve_keeps_named_system_var() {
    let dir = temp_project("FOO=bar\n");
    let out = bin()
        .current_dir(dir.path())
        .env("DC_SYSTEM_ONLY", "kept")
        .args([
            "--no-env-local",
            "run",
            "--clear-env",
            "--preserve",
            "DC_SYSTEM_ONLY",
            "--",
            "sh",
            "-c",
            "echo $DC_SYSTEM_ONLY",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "kept");
}

#[test]
fn completions_emit_script_per_shell() {
    for (shell, needle) in [("bash", "dotenv-cloud"), ("zsh", "#compdef")] {
        let out = bin().args(["completions", shell]).output().unwrap();
        assert!(out.status.success(), "completions {shell} failed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(needle),
            "completions {shell} missing `{needle}`: {stdout}"
        );
    }
}

#[test]
fn system_overrides_env_by_default_precedence() {
    // With the default order (system > env), a process env var wins over .env.
    let dir = temp_project("FOO=fromfile\n");
    let out = bin()
        .current_dir(dir.path())
        .env("FOO", "fromsystem")
        .args(["--no-env-local", "run", "--", "printenv", "FOO"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "fromsystem");
}
