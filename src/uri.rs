//! Secret reference detection and parsing (spec §6).
//!
//! Detection is exact and case-sensitive on the scheme. Parsing uses a
//! structured URI parser (the `url` crate); component access only happens after
//! parsing, never via raw string splitting.

use std::collections::BTreeMap;

use crate::redact;

/// Provider URI schemes recognized by the core (spec §6.1).
pub const SUPPORTED_SCHEMES: &[&str] = &["aws-sm", "aws-ssm", "vault"];

/// A parsed secret reference (mirrors `SecretReference` in spec §7.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SecretReference {
    pub original: String,
    pub scheme: String,
    pub authority: Option<String>,
    pub path: String,
    pub fragment: Option<String>,
    pub query: BTreeMap<String, String>,
}

impl SecretReference {
    /// Redacted form of the original reference for diagnostics (spec §13.3).
    pub fn redacted(&self) -> String {
        redact::redact_uri(&self.original)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UriError {
    #[error("invalid URI `{0}`: {1}")]
    Invalid(String, String),
}

/// Returns the scheme if `value` looks like a supported secret reference.
/// Detection is exact and case-sensitive (spec §6.1).
pub fn detect_scheme(value: &str) -> Option<&'static str> {
    SUPPORTED_SCHEMES
        .iter()
        .copied()
        .find(|scheme| value.starts_with(&format!("{scheme}://")))
}

/// Whether `value` is a recognized secret reference.
pub fn is_reference(value: &str) -> bool {
    detect_scheme(value).is_some()
}

/// Parse a supported secret reference. Returns an error for unknown schemes or
/// structurally invalid references (spec §18.4).
pub fn parse(value: &str) -> Result<SecretReference, UriError> {
    let scheme = detect_scheme(value)
        .ok_or_else(|| UriError::Invalid(value.to_string(), "unsupported scheme".into()))?;

    let parsed = url::Url::parse(value)
        .map_err(|e| UriError::Invalid(value.to_string(), e.to_string()))?;

    // `url` lowercases hosts for special schemes only; our schemes are not
    // special, so the host is preserved as the authority.
    let authority = match parsed.host_str() {
        Some(h) if !h.is_empty() => Some(h.to_string()),
        _ => None,
    };

    let path = parsed.path().to_string();
    let fragment = parsed.fragment().map(|s| s.to_string());

    let query: BTreeMap<String, String> = parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    // Structural validation: a reference needs a non-empty path or authority.
    let has_path = !path.is_empty() && path != "/";
    let has_authority = authority.is_some();
    if !has_path && !has_authority {
        return Err(UriError::Invalid(
            value.to_string(),
            "missing secret path".into(),
        ));
    }
    // A fragment-only reference (e.g. `vault://#key`) is invalid.
    if !has_path && !has_authority && fragment.is_some() {
        return Err(UriError::Invalid(
            value.to_string(),
            "fragment without a secret path".into(),
        ));
    }

    Ok(SecretReference {
        original: value.to_string(),
        scheme: scheme.to_string(),
        authority,
        path,
        fragment,
        query,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_schemes() {
        assert_eq!(detect_scheme("aws-sm://x/y"), Some("aws-sm"));
        assert_eq!(detect_scheme("aws-ssm:///x"), Some("aws-ssm"));
        assert_eq!(detect_scheme("vault://kv/app"), Some("vault"));
        assert_eq!(detect_scheme("REGION=us-east-1"), None);
        assert_eq!(detect_scheme("AWS-SM://x"), None); // case-sensitive
    }

    #[test]
    fn parses_aws_sm() {
        let r = parse("aws-sm://prod/db/password").unwrap();
        assert_eq!(r.scheme, "aws-sm");
        assert_eq!(r.authority.as_deref(), Some("prod"));
        assert_eq!(r.path, "/db/password");
        assert!(r.fragment.is_none());
    }

    #[test]
    fn parses_fragment_and_query() {
        let r = parse("aws-sm://prod/app/config#api_key").unwrap();
        assert_eq!(r.fragment.as_deref(), Some("api_key"));

        let r = parse("aws-sm://prod/db/password?version_stage=AWSCURRENT").unwrap();
        assert_eq!(r.query.get("version_stage").map(String::as_str), Some("AWSCURRENT"));
    }

    #[test]
    fn parses_ssm_triple_slash() {
        let r = parse("aws-ssm:///prod/app/api_token").unwrap();
        assert!(r.authority.is_none());
        assert_eq!(r.path, "/prod/app/api_token");
    }

    #[test]
    fn parses_vault() {
        let r = parse("vault://kv/data/app#api_key").unwrap();
        assert_eq!(r.authority.as_deref(), Some("kv"));
        assert_eq!(r.path, "/data/app");
        assert_eq!(r.fragment.as_deref(), Some("api_key"));
    }

    #[test]
    fn rejects_invalid_references() {
        assert!(parse("aws-sm://").is_err());
        assert!(parse("aws-ssm://").is_err());
        assert!(parse("vault://#key").is_err());
    }

    #[test]
    fn redacts_reference() {
        let r = parse("aws-sm://prod/db/password").unwrap();
        assert_eq!(r.redacted(), "aws-sm://prod/db/[redacted]");
    }
}
