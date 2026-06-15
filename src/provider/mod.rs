//! Provider plugin subsystem: discovery, scheme registry, and the resolver host.

pub mod host;
pub mod manifest;
pub mod protocol;

use std::collections::HashMap;
use std::time::Duration;

use crate::config::Config;
use crate::error::ProviderErrorClass;
use crate::secret::{ResolvedSecret, SecretCache};
use crate::uri::SecretReference;

use host::{HostError, PluginProcess};
use manifest::InstalledProvider;

/// Maps schemes to installed providers (spec §7.6). Rejects duplicate scheme
/// ownership unless config explicitly selects a provider for the scheme.
#[derive(Default)]
pub struct ProviderRegistry {
    by_scheme: HashMap<String, InstalledProvider>,
    all: Vec<InstalledProvider>,
}

impl ProviderRegistry {
    /// Discover installed providers from project-local and user directories,
    /// plus any explicit paths from config (spec §7.2).
    pub fn discover(config: &Config) -> Result<ProviderRegistry, String> {
        let mut all = Vec::new();

        // Explicit config paths take priority.
        for (name, path) in &config.providers.paths {
            let dir = std::path::Path::new(path);
            let manifest_path = dir.join("manifest.toml");
            let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
                format!(
                    "provider `{name}` manifest at {}: {e}",
                    manifest_path.display()
                )
            })?;
            let manifest = manifest::Manifest::parse(&text)?;
            all.push(InstalledProvider {
                manifest,
                dir: dir.to_path_buf(),
            });
        }

        for dir in manifest::discovery_dirs() {
            all.extend(manifest::scan_dir(&dir));
        }

        let mut by_scheme: HashMap<String, InstalledProvider> = HashMap::new();
        for provider in &all {
            for scheme in &provider.manifest.schemes {
                if let Some(existing) = by_scheme.get(scheme) {
                    if existing.manifest.name != provider.manifest.name {
                        return Err(format!(
                            "scheme `{scheme}` is claimed by both `{}` and `{}`; select one in config",
                            existing.manifest.name, provider.manifest.name
                        ));
                    }
                    continue;
                }
                by_scheme.insert(scheme.clone(), provider.clone());
            }
        }

        Ok(ProviderRegistry { by_scheme, all })
    }

    pub fn provider_for_scheme(&self, scheme: &str) -> Option<&InstalledProvider> {
        self.by_scheme.get(scheme)
    }

    pub fn installed(&self) -> &[InstalledProvider] {
        &self.all
    }

    pub fn has_scheme(&self, scheme: &str) -> bool {
        self.by_scheme.contains_key(scheme)
    }
}

/// Resolves secret references by lazily launching plugin processes, with an
/// in-memory per-execution cache (spec §6.5).
pub struct ResolverHost<'a> {
    registry: &'a ProviderRegistry,
    config: &'a Config,
    profile: String,
    timeout: Duration,
    cache: SecretCache,
    /// One live process per provider name, launched on first use.
    processes: HashMap<String, PluginProcess>,
}

impl<'a> ResolverHost<'a> {
    pub fn new(
        registry: &'a ProviderRegistry,
        config: &'a Config,
        profile: String,
        timeout: Duration,
    ) -> Self {
        ResolverHost {
            registry,
            config,
            profile,
            timeout,
            cache: SecretCache::new(),
            processes: HashMap::new(),
        }
    }

    /// Resolve one reference, using the cache and launching the plugin if
    /// needed. Returns a redaction-aware [`ResolvedSecret`].
    pub async fn resolve(
        &mut self,
        reference: &SecretReference,
    ) -> Result<ResolvedSecret, HostError> {
        let provider = self
            .registry
            .provider_for_scheme(&reference.scheme)
            .ok_or_else(|| HostError {
                class: ProviderErrorClass::ProviderUnavailable,
                message: format!(
                    "no provider installed for scheme {}\nhint: run `dotenv-cloud init` or `dotenv-cloud providers install {}`",
                    reference.scheme,
                    suggest_package(&reference.scheme)
                ),
                reference_redacted: Some(reference.redacted()),
            })?
            .clone();

        let cache_key =
            SecretCache::key(&provider.manifest.name, &reference.original, &self.profile);
        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(ResolvedSecret {
                value: cached,
                metadata: crate::secret::SecretMetadata {
                    provider: reference.scheme.clone(),
                    source_uri_redacted: reference.redacted(),
                    version: None,
                    cache_hit: true,
                },
            });
        }

        // Launch the plugin process lazily; reuse it for later references.
        if !self.processes.contains_key(&provider.manifest.name) {
            let proc = PluginProcess::launch(&provider, self.timeout).await?;
            self.processes.insert(provider.manifest.name.clone(), proc);
        }
        let proc = self.processes.get_mut(&provider.manifest.name).unwrap();

        let provider_config =
            toml_to_json(self.config.providers.config_for_scheme(&reference.scheme));
        let resolved = proc
            .resolve(reference, &self.profile, provider_config, self.timeout)
            .await?;

        self.cache.insert(cache_key, resolved.value.clone());
        Ok(resolved)
    }

    /// Shut down all launched plugin processes.
    pub async fn shutdown(self) {
        for (_, proc) in self.processes {
            proc.shutdown().await;
        }
    }
}

/// Map a scheme to its suggested provider package name for install hints.
pub fn suggest_package(scheme: &str) -> &'static str {
    match scheme {
        "aws-sm" | "aws-ssm" => "aws",
        "vault" => "vault",
        "azure-keyvault" | "az-kv" => "azure-keyvault",
        _ => scheme_static(scheme),
    }
}

fn scheme_static(_scheme: &str) -> &'static str {
    "<provider>"
}

/// Convert a TOML value (provider config table) into JSON for the wire protocol.
pub fn toml_to_json(value: toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s),
        toml::Value::Integer(i) => serde_json::Value::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(a) => {
            serde_json::Value::Array(a.into_iter().map(toml_to_json).collect())
        }
        toml::Value::Table(t) => {
            serde_json::Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_table_to_json() {
        let v: toml::Value = toml::from_str("region = \"us-east-1\"\ntimeout_ms = 2000").unwrap();
        let j = toml_to_json(v);
        assert_eq!(j["region"], serde_json::json!("us-east-1"));
        assert_eq!(j["timeout_ms"], serde_json::json!(2000));
    }

    #[test]
    fn package_suggestions() {
        assert_eq!(suggest_package("aws-sm"), "aws");
        assert_eq!(suggest_package("vault"), "vault");
    }
}
