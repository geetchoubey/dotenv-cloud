//! Configuration loading and the config schema (spec §9, §19).
//!
//! TOML is the primary format. Discovery walks up from the current directory.
//! Profiles and precedence are resolved here; provider config is passed through
//! to the plugin host.
//!
//! Several config sections (registry, conflicts, resolution.concurrency) are
//! part of the spec schema and parsed/validated here; not all are wired to
//! behavior in the V1 core yet.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{CliError, CliResult};
use crate::merge::{Precedence, Source};
use crate::redact::RedactionPolicy;

/// The on-disk config schema (spec §19). Unknown keys are rejected so typos
/// surface during `validate`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub default_profile: Option<String>,

    #[serde(default)]
    pub profile: BTreeMap<String, ProfileConfig>,

    #[serde(default)]
    pub defaults: BTreeMap<String, String>,

    #[serde(default)]
    pub precedence: Option<PrecedenceConfig>,

    #[serde(default)]
    pub providers: ProvidersConfig,

    #[serde(default)]
    pub provider_registry: Option<ProviderRegistryConfig>,

    #[serde(default)]
    pub resolution: ResolutionConfig,

    #[serde(default)]
    pub conflicts: ConflictsConfig,

    #[serde(default)]
    pub sensitive: SensitiveConfig,

    /// Path the config was loaded from, if any. Set after loading.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    #[serde(default)]
    pub env_file: Option<String>,
    #[serde(default)]
    pub env_local_file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedenceConfig {
    pub order: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub aws: Option<toml::Value>,
    #[serde(default)]
    pub vault: Option<toml::Value>,
    /// Explicit plugin paths keyed by provider name (spec §7.2).
    #[serde(default)]
    pub paths: BTreeMap<String, String>,
}

impl ProvidersConfig {
    /// Provider config table for a scheme, passed to the plugin as
    /// `provider_config` in the resolve protocol.
    pub fn config_for_scheme(&self, scheme: &str) -> toml::Value {
        let key = match scheme {
            "aws-sm" | "aws-ssm" => &self.aws,
            "vault" => &self.vault,
            _ => &None,
        };
        key.clone()
            .unwrap_or(toml::Value::Table(Default::default()))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRegistryConfig {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub allow_unsigned: bool,
    #[serde(default)]
    pub install_scope: Option<String>,
    /// base64 ed25519 public key used to verify signed provider archives.
    #[serde(default)]
    pub public_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionConfig {
    #[serde(default = "default_missing")]
    pub missing: String,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

fn default_missing() -> String {
    "error".to_string()
}
fn default_concurrency() -> usize {
    8
}

impl Default for ResolutionConfig {
    fn default() -> Self {
        ResolutionConfig {
            missing: default_missing(),
            concurrency: default_concurrency(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictsConfig {
    #[serde(default)]
    pub warn_on_any_shadow: bool,
    #[serde(default)]
    pub error_on_shadow: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitiveConfig {
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub patterns: Vec<String>,
}

impl Config {
    /// Resolve config discovery (spec §9.1) and load it. Returns the default
    /// config when none is found.
    pub fn discover_and_load(
        explicit: Option<&Path>,
        env_var: Option<String>,
    ) -> CliResult<Config> {
        if let Some(path) = explicit {
            return Self::load_file(path);
        }
        if let Some(p) = env_var {
            return Self::load_file(Path::new(&p));
        }
        let cwd = std::env::current_dir()
            .map_err(|e| CliError::Config(format!("cannot determine current dir: {e}")))?;
        if let Some(found) = find_upwards(&cwd) {
            return Self::load_file(&found);
        }
        Ok(Config::default())
    }

    pub fn load_file(path: &Path) -> CliResult<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CliError::Config(format!("cannot read config {}: {e}", path.display())))?;
        let mut cfg: Config = toml::from_str(&text)
            .map_err(|e| CliError::Config(format!("invalid config {}: {e}", path.display())))?;
        cfg.source_path = Some(path.to_path_buf());
        cfg.validate_semantics()?;
        Ok(cfg)
    }

    fn validate_semantics(&self) -> CliResult<()> {
        if let Some(p) = &self.precedence {
            for id in &p.order {
                if Source::parse(id).is_none() {
                    return Err(CliError::Config(format!(
                        "unknown precedence source `{id}` (allowed: cli, system, remote, env.local, env, defaults)"
                    )));
                }
            }
        }
        match self.resolution.missing.as_str() {
            "error" | "warn" | "ignore" => {}
            other => {
                return Err(CliError::Config(format!(
                    "invalid resolution.missing `{other}` (allowed: error, warn, ignore)"
                )))
            }
        }
        self.precedence()?; // surface duplicate-order errors
        Ok(())
    }

    /// Build the precedence order from config or the default.
    pub fn precedence(&self) -> CliResult<Precedence> {
        match &self.precedence {
            None => Ok(Precedence::default()),
            Some(p) => {
                let sources: Vec<Source> =
                    p.order.iter().filter_map(|s| Source::parse(s)).collect();
                Precedence::from_order(&sources).map_err(CliError::Config)
            }
        }
    }

    /// Resolve the active profile name (spec §9.3).
    pub fn resolve_profile(&self, cli_profile: Option<&str>) -> String {
        if let Some(p) = cli_profile {
            return p.to_string();
        }
        if let Ok(p) = std::env::var("DOTENV_CLOUD_PROFILE") {
            if !p.is_empty() {
                return p;
            }
        }
        if let Some(p) = &self.default_profile {
            return p.clone();
        }
        "dev".to_string()
    }

    /// Profile config for `name`, or an empty profile using `.env`/`.env.local`.
    pub fn profile(&self, name: &str) -> ProfileConfig {
        self.profile.get(name).cloned().unwrap_or_default()
    }

    pub fn redaction_policy(&self) -> RedactionPolicy {
        RedactionPolicy {
            sensitive_keys: self.sensitive.keys.clone(),
            sensitive_patterns: self.sensitive.patterns.clone(),
        }
    }
}

impl ProfileConfig {
    pub fn env_file(&self) -> &str {
        self.env_file.as_deref().unwrap_or(".env")
    }
    pub fn env_local_file(&self) -> &str {
        self.env_local_file.as_deref().unwrap_or(".env.local")
    }
}

/// Search the current directory and parents for `dotenv-cloud.toml`
/// (spec §9.1). TOML wins over YAML if both exist.
fn find_upwards(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let toml_path = d.join("dotenv-cloud.toml");
        if toml_path.is_file() {
            return Some(toml_path);
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let text = r#"
default_profile = "dev"

[profile.dev]
env_file = ".env"
env_local_file = ".env.local"

[defaults]
LOG_LEVEL = "info"
PORT = "3000"

[precedence]
order = ["cli", "system", "remote", "env.local", "env", "defaults"]

[providers.aws]
region = "us-east-1"

[providers.vault]
address = "https://vault.example.com"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("dev"));
        assert_eq!(cfg.defaults.get("PORT").map(String::as_str), Some("3000"));
        let p = cfg.precedence().unwrap();
        assert_eq!(p.order()[0], Source::Cli);
    }

    #[test]
    fn profile_resolution_falls_back_to_dev() {
        let cfg = Config::default();
        std::env::remove_var("DOTENV_CLOUD_PROFILE");
        assert_eq!(cfg.resolve_profile(None), "dev");
        assert_eq!(cfg.resolve_profile(Some("prod")), "prod");
    }

    #[test]
    fn invalid_precedence_source_rejected() {
        let text = "[precedence]\norder = [\"cli\", \"bogus\"]\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate_semantics().is_err());
    }
}
