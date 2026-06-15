//! Environment build pipeline (spec §12).
//!
//! Loads all sources into the merge engine, applies precedence, then resolves
//! remote URI winners through the provider host. Phase 1 (merge) is sync and
//! provider-free so `validate` can run it offline; phase 2 (resolve) is async.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::dotenv;
use crate::error::{CliError, CliResult};
use crate::merge::{MergeEngine, MergedValue, Precedence, Source};
use crate::provider::{ProviderRegistry, ResolverHost};
use crate::report::Reporter;
use crate::uri;

/// Options controlling which sources are loaded.
#[derive(Debug, Default, Clone)]
pub struct LoadOptions {
    pub env_file: Option<PathBuf>,
    pub env_local_file: Option<PathBuf>,
    pub no_env_file: bool,
    pub no_env_local: bool,
    /// CLI `--set KEY=VALUE` overrides, in order.
    pub sets: Vec<(String, String)>,
}

/// Per-key diagnostics for the final environment.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `origin` retained for conflict diagnostics (spec §17).
pub struct KeyInfo {
    pub winning_source: Source,
    pub origin: Source,
    pub shadowed: Vec<Source>,
    pub from_remote: bool,
    /// Redacted reference if the value was a remote URI.
    pub reference_redacted: Option<String>,
}

/// The merged-but-unresolved environment (phase 1 output).
pub struct MergedEnv {
    pub winners: Vec<MergedValue>,
    pub precedence: Precedence,
}

/// The fully resolved environment (phase 2 output).
pub struct ResolvedEnv {
    pub map: BTreeMap<String, String>,
    pub info: BTreeMap<String, KeyInfo>,
}

/// Phase 1: load all sources and apply precedence, including remote promotion
/// (spec §5.1). Does not contact any provider.
pub fn merge(
    config: &Config,
    profile_name: &str,
    opts: &LoadOptions,
) -> CliResult<MergedEnv> {
    let precedence = config.precedence()?;
    let profile = config.profile(profile_name);
    let mut engine = MergeEngine::new();

    // Lowest precedence first is not required (sort is stable on rank), but we
    // add high-to-low so equally ranked remote candidates resolve their tie in
    // favor of the higher-origin file.

    // CLI overrides (literal, never promoted to remote).
    for (k, v) in &opts.sets {
        engine.add(k.clone(), Source::Cli, v.clone());
    }

    // System environment (literal).
    for (k, v) in std::env::vars() {
        engine.add(k, Source::System, v);
    }

    // .env.local
    if !opts.no_env_local {
        let path = opts
            .env_local_file
            .clone()
            .unwrap_or_else(|| PathBuf::from(profile.env_local_file()));
        load_dotenv_into(&mut engine, &path, Source::EnvLocal, false)?;
    }

    // .env
    if !opts.no_env_file {
        let path = opts
            .env_file
            .clone()
            .unwrap_or_else(|| PathBuf::from(profile.env_file()));
        load_dotenv_into(&mut engine, &path, Source::Env, false)?;
    }

    // Config defaults (lowest).
    for (k, v) in &config.defaults {
        let effective = effective_source(v, Source::Defaults);
        engine.add_with_origin(k.clone(), effective, Source::Defaults, v.clone());
    }

    let winners = engine.resolve(&precedence);
    Ok(MergedEnv { winners, precedence })
}

/// A `.env` value that is a remote URI is promoted to the `remote` precedence
/// level (spec §5.1); otherwise it keeps its file source.
fn effective_source(value: &str, file_source: Source) -> Source {
    if uri::is_reference(value) {
        Source::Remote
    } else {
        file_source
    }
}

fn load_dotenv_into(
    engine: &mut MergeEngine,
    path: &Path,
    source: Source,
    required: bool,
) -> CliResult<()> {
    if !path.exists() {
        if required {
            return Err(CliError::DotenvParse(format!("{} not found", path.display())));
        }
        return Ok(());
    }
    let entries = dotenv::parse_file(path).map_err(|e| CliError::DotenvParse(e.to_string()))?;
    for entry in entries {
        let effective = effective_source(&entry.value, source);
        engine.add_with_origin(entry.key, effective, source, entry.value);
    }
    Ok(())
}

/// Phase 2: resolve remote winners through provider plugins. `required` filters
/// to only the keys that must be present (for `run --require`); when empty, all
/// remote winners are resolved.
pub async fn resolve(
    merged: MergedEnv,
    config: &Config,
    profile_name: &str,
    registry: &ProviderRegistry,
    timeout: Duration,
    reporter: &Reporter,
) -> CliResult<ResolvedEnv> {
    let mut map = BTreeMap::new();
    let mut info = BTreeMap::new();
    let missing_policy = config.resolution.missing.clone();

    let mut host = ResolverHost::new(registry, config, profile_name.to_string(), timeout);

    for winner in &merged.winners {
        let from_remote = winner.winning_source == Source::Remote;
        if !from_remote {
            map.insert(winner.key.clone(), winner.value.clone());
            info.insert(
                winner.key.clone(),
                KeyInfo {
                    winning_source: winner.winning_source,
                    origin: winner.origin,
                    shadowed: winner.shadowed.clone(),
                    from_remote: false,
                    reference_redacted: None,
                },
            );
            continue;
        }

        // Remote winner: parse + resolve.
        let reference = match uri::parse(&winner.value) {
            Ok(r) => r,
            Err(e) => {
                host.shutdown().await;
                return Err(CliError::SecretResolution(format!(
                    "{}: {e}",
                    winner.key
                )));
            }
        };
        let redacted = reference.redacted();

        match host.resolve(&reference).await {
            Ok(resolved) => {
                map.insert(winner.key.clone(), resolved.expose().to_string());
                info.insert(
                    winner.key.clone(),
                    KeyInfo {
                        winning_source: Source::Remote,
                        origin: winner.origin,
                        shadowed: winner.shadowed.clone(),
                        from_remote: true,
                        reference_redacted: Some(redacted),
                    },
                );
            }
            Err(e) => {
                use crate::error::ProviderErrorClass::*;
                let is_missing = matches!(e.class, NotFound);
                if is_missing && missing_policy != "error" {
                    if missing_policy == "warn" {
                        reporter.warn(&format!(
                            "{}: secret not found ({})",
                            winner.key,
                            e.reference_redacted.as_deref().unwrap_or(&redacted)
                        ));
                    }
                    continue; // skip this key
                }
                host.shutdown().await;
                let detail = format!(
                    "failed to resolve {}\nprovider: {}\nreference: {}\nclass: {}\nreason: {}",
                    winner.key,
                    reference.scheme,
                    e.reference_redacted.as_deref().unwrap_or(&redacted),
                    e.class,
                    e.message
                );
                return Err(map_provider_error(e.class, detail));
            }
        }
    }

    host.shutdown().await;
    Ok(ResolvedEnv { map, info })
}

/// Resolve a single key from already-merged winners (for `resolve <KEY>`).
/// Returns `None` if the key is absent. Only contacts a provider if the winning
/// value is a remote reference.
pub async fn resolve_one(
    merged: &MergedEnv,
    key: &str,
    config: &Config,
    profile_name: &str,
    registry: &ProviderRegistry,
    timeout: Duration,
) -> CliResult<Option<(String, KeyInfo)>> {
    let Some(winner) = merged.winners.iter().find(|w| w.key == key) else {
        return Ok(None);
    };

    if winner.winning_source != Source::Remote {
        return Ok(Some((
            winner.value.clone(),
            KeyInfo {
                winning_source: winner.winning_source,
                origin: winner.origin,
                shadowed: winner.shadowed.clone(),
                from_remote: false,
                reference_redacted: None,
            },
        )));
    }

    let reference = uri::parse(&winner.value)
        .map_err(|e| CliError::SecretResolution(format!("{key}: {e}")))?;
    let redacted = reference.redacted();
    let mut host = ResolverHost::new(registry, config, profile_name.to_string(), timeout);
    let result = host.resolve(&reference).await;
    host.shutdown().await;

    match result {
        Ok(resolved) => Ok(Some((
            resolved.expose().to_string(),
            KeyInfo {
                winning_source: Source::Remote,
                origin: winner.origin,
                shadowed: winner.shadowed.clone(),
                from_remote: true,
                reference_redacted: Some(redacted),
            },
        ))),
        Err(e) => {
            let detail = format!(
                "failed to resolve {key}\nprovider: {}\nreference: {}\nclass: {}\nreason: {}",
                reference.scheme,
                e.reference_redacted.as_deref().unwrap_or(&redacted),
                e.class,
                e.message
            );
            Err(map_provider_error(e.class, detail))
        }
    }
}

fn map_provider_error(class: crate::error::ProviderErrorClass, detail: String) -> CliError {
    use crate::error::ProviderErrorClass::*;
    match class {
        AuthenticationFailed | PermissionDenied => CliError::ProviderAuth(detail),
        Timeout | RateLimited | Network | ProviderUnavailable => CliError::ProviderNetwork(detail),
        _ => CliError::SecretResolution(detail),
    }
}
