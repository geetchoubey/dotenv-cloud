//! Redaction policy (spec §13.3).
//!
//! Decides which keys are sensitive and renders redacted values and redacted
//! remote URI references for diagnostics. This module never emits secret
//! material.

pub const REDACTED: &str = "[redacted]";

/// Key-name substrings that mark a value as sensitive (spec §13.3).
const SENSITIVE_SUBSTRINGS: &[&str] = &[
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASS",
    "PRIVATE_KEY",
    "API_KEY",
    "ACCESS_KEY",
    "SESSION",
];

/// Policy for deciding whether a value should be redacted in diagnostics.
#[derive(Debug, Default, Clone)]
pub struct RedactionPolicy {
    /// Keys explicitly marked sensitive in config.
    pub sensitive_keys: Vec<String>,
    /// Glob-ish patterns (`*_TOKEN`, `*_SECRET`) from config.
    pub sensitive_patterns: Vec<String>,
}

impl RedactionPolicy {
    /// A value is redacted when (a) the key matches a built-in sensitive
    /// substring, (b) it was sourced from a remote provider, or (c) the user
    /// marked the key/pattern sensitive in config.
    pub fn is_sensitive(&self, key: &str, from_remote: bool) -> bool {
        if from_remote {
            return true;
        }
        let upper = key.to_ascii_uppercase();
        if SENSITIVE_SUBSTRINGS.iter().any(|s| upper.contains(s)) {
            return true;
        }
        if self.sensitive_keys.iter().any(|k| k == key) {
            return true;
        }
        self.sensitive_patterns.iter().any(|p| glob_match(p, key))
    }

    /// Render a value for diagnostics, redacting if sensitive.
    pub fn render(&self, key: &str, value: &str, from_remote: bool) -> String {
        if self.is_sensitive(key, from_remote) {
            REDACTED.to_string()
        } else {
            value.to_string()
        }
    }
}

/// Minimal `*` glob matcher supporting a single leading and/or trailing `*`.
/// Sufficient for the `*_TOKEN` / `*_SECRET` patterns in the spec.
fn glob_match(pattern: &str, value: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        (Some(suffix), _) if !pattern.ends_with('*') => value.ends_with(suffix),
        (_, Some(prefix)) if !pattern.starts_with('*') => value.starts_with(prefix),
        (Some(_), Some(_)) => {
            let inner = pattern.trim_matches('*');
            value.contains(inner)
        }
        _ => pattern == value,
    }
}

/// Redact a remote URI reference conservatively (spec §13.3):
///
/// * `aws-sm://prod/db/password` -> `aws-sm://prod/db/[redacted]`
/// * `vault://kv/data/app#api_key` -> `vault://kv/data/app#[redacted]`
pub fn redact_uri(original: &str) -> String {
    // When a fragment is present, redact only the fragment and keep the path
    // (the fragment is the sensitive field selector). Otherwise redact the
    // final path segment (spec §13.3).
    if let Some((base, _frag)) = original.split_once('#') {
        return format!("{base}#{REDACTED}");
    }
    match original.rsplit_once('/') {
        Some((head, tail)) if !tail.is_empty() => format!("{head}/{REDACTED}"),
        _ => original.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_keys_are_sensitive() {
        let p = RedactionPolicy::default();
        assert!(p.is_sensitive("DB_PASSWORD", false));
        assert!(p.is_sensitive("MY_API_KEY", false));
        assert!(p.is_sensitive("AWS_SESSION_TOKEN", false));
        assert!(!p.is_sensitive("REGION", false));
        assert!(!p.is_sensitive("PORT", false));
    }

    #[test]
    fn remote_values_always_redacted() {
        let p = RedactionPolicy::default();
        assert!(p.is_sensitive("REGION", true));
    }

    #[test]
    fn config_patterns_match() {
        let p = RedactionPolicy {
            sensitive_keys: vec!["DB_PASSWORD".into()],
            sensitive_patterns: vec!["*_PRIVATE".into(), "INTERNAL_*".into()],
        };
        assert!(p.is_sensitive("FOO_PRIVATE", false));
        assert!(p.is_sensitive("INTERNAL_THING", false));
        assert!(!p.is_sensitive("PUBLIC", false));
    }

    #[test]
    fn uri_redaction_path_and_fragment() {
        assert_eq!(
            redact_uri("aws-sm://prod/db/password"),
            "aws-sm://prod/db/[redacted]"
        );
        assert_eq!(
            redact_uri("vault://kv/data/app#api_key"),
            "vault://kv/data/app#[redacted]"
        );
    }
}
