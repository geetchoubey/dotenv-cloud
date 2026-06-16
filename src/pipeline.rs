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
    /// Precedence in effect, so callers can decide (per key) whether a project
    /// value should override an inherited `system` variable.
    pub precedence: Precedence,
}

/// Phase 1: load all sources and apply precedence, including remote promotion
/// (spec §5.1). Does not contact any provider.
pub fn merge(config: &Config, environment_name: &str, opts: &LoadOptions) -> CliResult<MergedEnv> {
    let precedence = config.precedence()?;
    let environment = config.environment(environment_name);
    let mut engine = MergeEngine::new();

    // Lowest precedence first is not required (sort is stable on rank), but we
    // add high-to-low so equally ranked remote candidates resolve their tie in
    // favor of the higher-origin file.

    // CLI overrides (literal, never promoted to remote).
    for (k, v) in &opts.sets {
        engine.add(k.clone(), Source::Cli, v.clone());
    }

    // The process environment (`system`) is intentionally NOT loaded as a value
    // source here. It is ambient context: a child launched by `run` inherits it
    // automatically, and `export`/`build` must emit only the project's own
    // environment (`.env`, `.env.local`, remote, defaults, `--set`) — not the
    // entire shell. The `system` precedence rank still governs, at exec time,
    // whether a project value overrides an inherited system variable.

    // .env.local
    if !opts.no_env_local {
        let path = opts
            .env_local_file
            .clone()
            .unwrap_or_else(|| PathBuf::from(environment.env_local_file()));
        load_dotenv_into(&mut engine, &path, Source::EnvLocal, false)?;
    }

    // .env
    if !opts.no_env_file {
        let path = opts
            .env_file
            .clone()
            .unwrap_or_else(|| PathBuf::from(environment.env_file()));
        load_dotenv_into(&mut engine, &path, Source::Env, false)?;
    }

    // Per-environment defaults (lowest precedence).
    for (k, v) in &environment.defaults {
        let effective = effective_source(v, Source::Defaults);
        engine.add_with_origin(k.clone(), effective, Source::Defaults, v.clone());
    }

    let winners = engine.resolve(&precedence);
    Ok(MergedEnv {
        winners,
        precedence,
    })
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
            return Err(CliError::DotenvParse(format!(
                "{} not found",
                path.display()
            )));
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
    environment_name: &str,
    registry: &ProviderRegistry,
    timeout: Duration,
    reporter: &Reporter,
) -> CliResult<ResolvedEnv> {
    let mut map = BTreeMap::new();
    let mut info = BTreeMap::new();
    let precedence = merged.precedence.clone();
    let missing_policy = config.resolution.missing.clone();
    let defaults = config.defaults_for(environment_name);

    let mut host = ResolverHost::new(registry, config, environment_name.to_string(), timeout);

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
                return Err(CliError::SecretResolution(format!("{}: {e}", winner.key)));
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

                // Fallback (spec §12): on any resolution failure, if the active
                // environment defines a default for this key, use it instead of
                // failing or dropping the key. Only when no default exists does
                // the `resolution.missing` policy apply.
                if let Some(default) = defaults.get(&winner.key) {
                    reporter.warn(&format!(
                        "{}: remote resolution failed ({}); using environment default",
                        winner.key, e.class
                    ));
                    map.insert(winner.key.clone(), default.clone());
                    info.insert(
                        winner.key.clone(),
                        KeyInfo {
                            winning_source: Source::Defaults,
                            origin: Source::Defaults,
                            shadowed: winner.shadowed.clone(),
                            from_remote: false,
                            reference_redacted: Some(redacted),
                        },
                    );
                    continue;
                }

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
                let detail = format_resolution_failure(
                    &winner.key,
                    &reference.scheme,
                    e.reference_redacted.as_deref().unwrap_or(&redacted),
                    e.class,
                    &e.message,
                );
                return Err(map_provider_error(e.class, detail));
            }
        }
    }

    host.shutdown().await;
    Ok(ResolvedEnv {
        map,
        info,
        precedence,
    })
}

/// Resolve a single key from already-merged winners (for `resolve <KEY>`).
/// Returns `None` if the key is absent. Only contacts a provider if the winning
/// value is a remote reference.
pub async fn resolve_one(
    merged: &MergedEnv,
    key: &str,
    config: &Config,
    environment_name: &str,
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

    let reference =
        uri::parse(&winner.value).map_err(|e| CliError::SecretResolution(format!("{key}: {e}")))?;
    let redacted = reference.redacted();
    let mut host = ResolverHost::new(registry, config, environment_name.to_string(), timeout);
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
            // Fallback to the environment default for this key, if any
            // (spec §12), mirroring `resolve`.
            if let Some(default) = config.defaults_for(environment_name).get(key) {
                return Ok(Some((
                    default.clone(),
                    KeyInfo {
                        winning_source: Source::Defaults,
                        origin: Source::Defaults,
                        shadowed: winner.shadowed.clone(),
                        from_remote: false,
                        reference_redacted: Some(redacted),
                    },
                )));
            }
            let detail = format_resolution_failure(
                key,
                &reference.scheme,
                e.reference_redacted.as_deref().unwrap_or(&redacted),
                e.class,
                &e.message,
            );
            Err(map_provider_error(e.class, detail))
        }
    }
}

/// Build a rich, aligned failure message that puts the unresolved key front and
/// center and adds a class-specific hint.
fn format_resolution_failure(
    key: &str,
    scheme: &str,
    reference_redacted: &str,
    class: crate::error::ProviderErrorClass,
    message: &str,
) -> String {
    use crate::error::ProviderErrorClass::*;
    let hint = match class {
        NotFound => Some(
            "no secret or parameter exists at that path — check the name, AWS region, and account",
        ),
        AuthenticationFailed => Some(
            "authentication failed — check your provider credentials (env, shared config, or role)",
        ),
        PermissionDenied => {
            Some("access was denied — check the IAM/policy permissions for this identity")
        }
        Timeout => Some("the request timed out — check connectivity or raise --timeout"),
        Network | ProviderUnavailable => {
            Some("could not reach the provider — check your network and endpoint")
        }
        RateLimited => Some("the provider is rate-limiting requests — retry shortly"),
        InvalidReference => Some("the reference URI is malformed — check its syntax"),
        InvalidSecretPayload => {
            Some("the secret value could not be parsed — check the field/format selector")
        }
        Internal => None,
    };

    let mut lines = vec![
        format!("could not resolve key `{key}`"),
        format!("  {:<11}{scheme}", "provider:"),
        format!("  {:<11}{reference_redacted}", "reference:"),
        format!("  {:<11}{class}", "class:"),
        format!("  {:<11}{message}", "reason:"),
    ];
    if let Some(h) = hint {
        lines.push(format!("  {:<11}{h}", "hint:"));
    }
    lines.join("\n")
}

fn map_provider_error(class: crate::error::ProviderErrorClass, detail: String) -> CliError {
    use crate::error::ProviderErrorClass::*;
    match class {
        AuthenticationFailed | PermissionDenied => CliError::ProviderAuth(detail),
        Timeout | RateLimited | Network | ProviderUnavailable => CliError::ProviderNetwork(detail),
        _ => CliError::SecretResolution(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorClass;

    #[test]
    fn resolution_failure_highlights_key_and_hint() {
        let msg = format_resolution_failure(
            "name",
            "aws-ssm",
            "aws-ssm://prod/test/[redacted]",
            ProviderErrorClass::NotFound,
            "AWS error: ParameterNotFound",
        );
        // Key is prominent on the first line, in backticks.
        assert_eq!(msg.lines().next().unwrap(), "could not resolve key `name`");
        // Aligned, labeled fields and a class-specific hint are present.
        assert!(msg.contains("reference: aws-ssm://prod/test/[redacted]"));
        assert!(msg.contains("class:     NotFound"));
        assert!(msg.contains("hint:"));
        // The redacted reference is preserved (no raw secret path).
        assert!(!msg.contains("prod/test/name"));
    }

    #[test]
    fn resolution_failure_internal_has_no_hint() {
        let msg = format_resolution_failure(
            "X",
            "vault",
            "vault://x/[redacted]",
            ProviderErrorClass::Internal,
            "boom",
        );
        assert!(!msg.contains("hint:"));
    }
}
