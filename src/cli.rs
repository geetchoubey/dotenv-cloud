//! CLI surface (spec §10). Global flags precede the subcommand.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::error::{CliError, CliResult};

#[derive(Debug, Parser)]
#[command(
    name = "dotenv-cloud",
    version,
    about = "Load dotenv files and resolve remote secret references via external provider plugins",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Use an explicit config file.
    #[arg(long, global = true, value_name = "path")]
    pub config: Option<PathBuf>,

    /// Select a named profile.
    #[arg(long, global = true, value_name = "name")]
    pub profile: Option<String>,

    /// Override profile `.env` path.
    #[arg(long, global = true, value_name = "path")]
    pub env_file: Option<PathBuf>,

    /// Override profile `.env.local` path.
    #[arg(long, global = true, value_name = "path")]
    pub env_local_file: Option<PathBuf>,

    /// Do not load `.env`.
    #[arg(long, global = true)]
    pub no_env_file: bool,

    /// Do not load `.env.local`.
    #[arg(long, global = true)]
    pub no_env_local: bool,

    /// Add or override an environment value at CLI precedence. May be repeated.
    #[arg(long = "set", global = true, value_name = "KEY=VALUE")]
    pub set: Vec<String>,

    /// Treat warnings as errors.
    #[arg(long, global = true)]
    pub strict: bool,

    /// Override provider request timeout, e.g. `2s`, `500ms`.
    #[arg(long, global = true, value_name = "duration")]
    pub timeout: Option<String>,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Emit diagnostic metadata without secrets.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Suppress non-error output.
    #[arg(long, global = true)]
    pub quiet: bool,
}

impl GlobalArgs {
    /// Parse `--set KEY=VALUE` overrides.
    pub fn parsed_sets(&self) -> CliResult<Vec<(String, String)>> {
        self.set
            .iter()
            .map(|s| {
                s.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .ok_or_else(|| {
                        CliError::Usage(format!("invalid --set `{s}`, expected KEY=VALUE"))
                    })
            })
            .collect()
    }

    /// Resolve the provider timeout (default 2s, spec §18.7).
    pub fn timeout(&self) -> CliResult<Duration> {
        match &self.timeout {
            None => Ok(Duration::from_secs(2)),
            Some(s) => parse_duration(s),
        }
    }
}

/// Parse durations like `2s`, `500ms`, `1500` (ms assumed).
fn parse_duration(s: &str) -> CliResult<Duration> {
    let s = s.trim();
    let invalid = || CliError::Usage(format!("invalid duration `{s}`"));
    if let Some(num) = s.strip_suffix("ms") {
        return num
            .trim()
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| invalid());
    }
    if let Some(num) = s.strip_suffix('s') {
        return num
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| invalid());
    }
    s.parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|_| invalid())
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scan config and dotenv files, install required providers, write the lockfile.
    Init(InitArgs),
    /// Resolve environment and execute a child process.
    Run(RunArgs),
    /// Print shell-compatible environment assignments.
    Export(ExportArgs),
    /// Materialize the resolved environment to a file or stdout.
    Build(BuildArgs),
    /// Resolve and print one key.
    Resolve(ResolveArgs),
    /// Parse config, check references, optionally contact providers.
    Validate(ValidateArgs),
    /// Diagnose local setup, credentials, config, and provider connectivity.
    Doctor,
    /// Manage externally installed provider plugins.
    Providers(ProvidersArgs),
    /// Print a shell completion script (bash, zsh, fish, powershell, elvish).
    Completions(CompletionsArgs),
    /// Generate an ed25519 release signing keypair (maintainer tool).
    #[command(hide = true)]
    Keygen,
    /// Sign a file with an ed25519 private key, printing a base64 signature
    /// (maintainer/CI tool).
    #[command(hide = true)]
    Sign(SignArgs),
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    pub shell: clap_complete::Shell,
}

#[derive(Debug, Args)]
pub struct SignArgs {
    /// File to sign (its raw bytes are signed).
    pub file: PathBuf,
    /// base64 ed25519 private key. If omitted, read from
    /// `DOTENV_CLOUD_SIGNING_KEY`.
    #[arg(long, value_name = "b64", env = "DOTENV_CLOUD_SIGNING_KEY")]
    pub key: Option<String>,
    /// Write the signature to this path instead of stdout.
    #[arg(long, value_name = "path")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub project: bool,
    #[arg(long)]
    pub user: bool,
    #[arg(long, value_name = "url")]
    pub registry: Option<String>,
    #[arg(long, value_name = "path")]
    pub lockfile: Option<PathBuf>,
    #[arg(long)]
    pub upgrade: bool,
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Start child with only resolved environment, not inherited system env.
    #[arg(long)]
    pub clear_env: bool,
    /// Preserve listed system variables when `--clear-env` is used.
    #[arg(long, value_name = "VARS", value_delimiter = ',')]
    pub preserve: Vec<String>,
    /// Require key to be present after resolution. May be repeated.
    #[arg(long = "require", value_name = "KEY")]
    pub require: Vec<String>,
    /// Resolve and print a redacted summary without executing the child.
    #[arg(long)]
    pub dry_run: bool,
    /// Print a redacted source summary.
    #[arg(long)]
    pub redact_summary: bool,
    /// Install missing provider plugins before resolution.
    #[arg(long)]
    pub install_missing_providers: bool,
    /// The child command, after `--`.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(long, value_name = "shell")]
    pub shell: Option<String>,
    #[arg(long, value_name = "format")]
    pub format: Option<String>,
    #[arg(long = "include", value_name = "KEY")]
    pub include: Vec<String>,
    #[arg(long = "exclude", value_name = "KEY")]
    pub exclude: Vec<String>,
    #[arg(long)]
    pub no_comments: bool,
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    #[arg(long, value_name = "path")]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub force: bool,
    #[arg(long, value_name = "dotenv|json", default_value = "dotenv")]
    pub mode: String,
    #[arg(long, value_name = "mode")]
    pub chmod: Option<String>,
    #[arg(long = "include", value_name = "KEY")]
    pub include: Vec<String>,
    #[arg(long = "exclude", value_name = "KEY")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ResolveArgs {
    pub key: String,
    #[arg(long)]
    pub show: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub source: bool,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    #[arg(long)]
    pub providers: bool,
    #[arg(long)]
    pub no_providers: bool,
    #[arg(long)]
    pub all_profiles: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ProvidersArgs {
    #[command(subcommand)]
    pub command: ProvidersCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProvidersCommand {
    /// List installed providers and configured schemes.
    List(ProvidersCommonArgs),
    /// Search configured provider registries.
    Search {
        query: String,
        #[arg(long, value_name = "url")]
        registry: Option<String>,
    },
    /// Download, verify, and install a provider plugin.
    Install(ProvidersTargetArgs),
    /// Update one or all installed providers.
    Update(ProvidersOptionalTargetArgs),
    /// Remove an installed provider plugin.
    Remove(ProvidersTargetArgs),
    /// Show provider metadata, schemes, version, and integrity info.
    Info(ProvidersTargetArgs),
}

#[derive(Debug, Args)]
pub struct ProvidersCommonArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ProvidersTargetArgs {
    pub name: String,
    #[arg(long)]
    pub json: bool,
    #[arg(long, value_name = "url")]
    pub registry: Option<String>,
    #[arg(long)]
    pub project: bool,
    #[arg(long)]
    pub user: bool,
    #[arg(long)]
    pub yes: bool,
    /// Allow installing providers without a verified signature.
    #[arg(long)]
    pub allow_unsigned: bool,
}

#[derive(Debug, Args)]
pub struct ProvidersOptionalTargetArgs {
    pub name: Option<String>,
    #[arg(long, value_name = "url")]
    pub registry: Option<String>,
    #[arg(long)]
    pub project: bool,
    #[arg(long)]
    pub user: bool,
    #[arg(long)]
    pub yes: bool,
    /// Allow installing providers without a verified signature.
    #[arg(long)]
    pub allow_unsigned: bool,
}
