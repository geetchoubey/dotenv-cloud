//! Secret-bearing types and the per-execution in-memory cache (spec §6.5, §7.5).
//!
//! Resolved secret material is wrapped in a redacting type and is never written
//! to disk. The cache lives only for the duration of a single command.

use std::collections::HashMap;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

/// Metadata about a resolved secret (spec §7.5). Contains no secret material.
/// Carried end-to-end for diagnostics; not every field is surfaced in V1 output.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SecretMetadata {
    pub provider: String,
    pub source_uri_redacted: String,
    pub version: Option<String>,
    pub cache_hit: bool,
}

/// A resolved secret value plus metadata. `value` redacts on `Debug`.
#[derive(Clone)]
pub struct ResolvedSecret {
    pub value: SecretString,
    pub metadata: SecretMetadata,
}

impl std::fmt::Debug for ResolvedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSecret")
            .field("value", &"[redacted]")
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl ResolvedSecret {
    pub fn expose(&self) -> &str {
        self.value.expose_secret()
    }
}

/// In-memory cache keyed by `provider_id + normalized_uri + profile_name`
/// (spec §6.5). Destroyed when the process exits; never persisted.
#[derive(Default)]
pub struct SecretCache {
    inner: Mutex<HashMap<String, SecretString>>,
}

impl SecretCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn key(provider_id: &str, normalized_uri: &str, profile: &str) -> String {
        format!("{provider_id}\u{1f}{normalized_uri}\u{1f}{profile}")
    }

    pub fn get(&self, key: &str) -> Option<SecretString> {
        self.inner.lock().unwrap().get(key).cloned()
    }

    pub fn insert(&self, key: String, value: SecretString) {
        self.inner.lock().unwrap().insert(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip() {
        let c = SecretCache::new();
        let k = SecretCache::key("aws-sm", "aws-sm://prod/db/password", "dev");
        assert!(c.get(&k).is_none());
        c.insert(k.clone(), SecretString::new("s3cr3t".into()));
        assert_eq!(c.get(&k).unwrap().expose_secret(), "s3cr3t");
    }

    #[test]
    fn debug_redacts() {
        let r = ResolvedSecret {
            value: SecretString::new("topsecret".into()),
            metadata: SecretMetadata {
                provider: "aws-sm".into(),
                source_uri_redacted: "aws-sm://p/[redacted]".into(),
                version: None,
                cache_hit: false,
            },
        };
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("topsecret"));
        assert!(dbg.contains("[redacted]"));
    }
}
