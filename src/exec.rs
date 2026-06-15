//! Subprocess execution (spec §11).
//!
//! Spawns the child command with the resolved environment using argv-based APIs
//! (never a shell string). Mirrors the child's exit code; on Unix, a signal
//! death is reported as `128 + signal`.

use std::collections::BTreeMap;
use std::process::Command;

use crate::error::{CliError, CliResult};

/// How the child's environment is constructed.
pub struct ExecOptions {
    /// Start from an empty environment instead of inheriting the parent's.
    pub clear_env: bool,
    /// When `clear_env` is set, system variables to preserve.
    pub preserve: Vec<String>,
}

/// Spawn `argv[0]` with `argv[1..]` and the merged `env`, returning the exit
/// code to propagate.
pub fn run(argv: &[String], env: &BTreeMap<String, String>, opts: &ExecOptions) -> CliResult<i32> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| CliError::Usage("no command provided after `--`".into()))?;

    let mut cmd = Command::new(program);
    cmd.args(args);

    if opts.clear_env {
        cmd.env_clear();
        for key in &opts.preserve {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
    }

    // Apply the resolved environment on top.
    for (k, v) in env {
        cmd.env(k, v);
    }

    let status = cmd
        .status()
        .map_err(|e| CliError::Runtime(format!("failed to execute `{program}`: {e}")))?;

    if let Some(code) = status.code() {
        return Ok(code);
    }

    // No exit code: terminated by signal on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Ok(128 + sig);
        }
    }

    Ok(1)
}
