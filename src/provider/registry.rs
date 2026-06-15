//! Provider registry: fetch an index, download a provider archive, verify its
//! integrity (sha256 + optional ed25519 signature), unpack it, and install it
//! into a provider directory (spec §7.2, §7.3, §13.2).
//!
//! The registry URL may be `https://`, `file://`, or a local path so the flow
//! is testable offline. Downloads are blocking (this runs from CLI install
//! commands, not the resolve hot path).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::manifest::{self, Manifest};

/// The build target triple (e.g. `aarch64-apple-darwin`), captured by build.rs.
/// `DOTENV_CLOUD_TARGET_OVERRIDE` overrides it (used by tests).
pub fn current_target() -> String {
    std::env::var("DOTENV_CLOUD_TARGET_OVERRIDE")
        .unwrap_or_else(|_| env!("DOTENV_CLOUD_TARGET").to_string())
}

/// Where a provider should be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    User,
}

impl Scope {
    /// Resolve the providers root directory for this scope.
    pub fn providers_root(self) -> Result<PathBuf, String> {
        match self {
            Scope::Project => {
                let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
                Ok(cwd.join(".dotenv-cloud").join("providers"))
            }
            Scope::User => manifest::user_provider_dir()
                .ok_or_else(|| "cannot determine user provider directory".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Index schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub schema_version: u32,
    /// Keyed by short provider name (e.g. `aws`).
    pub providers: BTreeMap<String, IndexProvider>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexProvider {
    pub package: String,
    #[serde(default)]
    pub description: Option<String>,
    pub schemes: Vec<String>,
    /// Keyed by version string.
    pub versions: BTreeMap<String, IndexVersion>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexVersion {
    /// Keyed by target triple.
    pub targets: BTreeMap<String, IndexArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexArtifact {
    pub url: String,
    pub sha256: String,
    /// base64 ed25519 signature over the raw archive bytes (optional).
    #[serde(default)]
    pub signature: Option<String>,
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("invalid registry index: {e}"))
    }

    /// Find a provider entry by short name.
    pub fn provider(&self, name: &str) -> Option<(&String, &IndexProvider)> {
        self.providers.get_key_value(name)
    }

    /// Find the short name owning a scheme.
    #[allow(dead_code)] // authoritative scheme->name lookup; used in tests and future wiring.
    pub fn name_for_scheme(&self, scheme: &str) -> Option<&str> {
        self.providers
            .iter()
            .find(|(_, p)| p.schemes.iter().any(|s| s == scheme))
            .map(|(k, _)| k.as_str())
    }
}

impl IndexProvider {
    /// Choose a version: the requested one, or the highest by semver.
    pub fn select_version(&self, want: Option<&str>) -> Result<(String, &IndexVersion), String> {
        if let Some(v) = want {
            return self
                .versions
                .get_key_value(v)
                .map(|(k, val)| (k.clone(), val))
                .ok_or_else(|| format!("version `{v}` not found for `{}`", self.package));
        }
        let mut best: Option<(semver::Version, &String)> = None;
        for k in self.versions.keys() {
            if let Ok(parsed) = semver::Version::parse(k) {
                if best.as_ref().map(|(b, _)| &parsed > b).unwrap_or(true) {
                    best = Some((parsed, k));
                }
            }
        }
        let key = best
            .map(|(_, k)| k.clone())
            .ok_or_else(|| format!("no valid versions for `{}`", self.package))?;
        let ver = &self.versions[&key];
        Ok((key, ver))
    }
}

// ---------------------------------------------------------------------------
// Fetch + verify
// ---------------------------------------------------------------------------

/// Fetch bytes from an `https://`, `file://`, or local-path URL.
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let resp = ureq::get(url)
            .call()
            .map_err(|e| format!("download failed for {url}: {e}"))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed for {url}: {e}"))?;
        return Ok(buf);
    }
    // Treat anything else as a local filesystem path.
    std::fs::read(url).map_err(|e| format!("cannot read {url}: {e}"))
}

/// Lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Verify an ed25519 signature (base64) over `bytes` using a base64 public key.
pub fn verify_signature(
    bytes: &[u8],
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64.trim())
        .map_err(|e| format!("invalid public key base64: {e}"))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "public key must be 32 bytes".to_string())?;
    let vk = VerifyingKey::from_bytes(&key_arr).map_err(|e| format!("invalid public key: {e}"))?;

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .map_err(|e| format!("invalid signature base64: {e}"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let sig = Signature::from_bytes(&sig_arr);

    vk.verify(bytes, &sig)
        .map_err(|_| "signature verification failed".to_string())
}

/// Built-in trusted ed25519 public keys (base64). Artifacts signed by any of
/// these verify with no user configuration, so signature verification is on by
/// default. Populated at release time with the project release key; empty
/// entries are ignored (e.g. before a key is provisioned).
const TRUSTED_PUBLIC_KEYS: &[&str] = &[
    // dotenv-cloud release signing key.
    "",
];

/// All trusted public keys: the built-in release key(s) plus any extra key
/// configured for a custom/private registry.
fn trusted_keys(extra: Option<&str>) -> Vec<String> {
    let mut keys: Vec<String> = TRUSTED_PUBLIC_KEYS
        .iter()
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim().to_string())
        .collect();
    if let Some(k) = extra {
        let k = k.trim();
        if !k.is_empty() {
            keys.push(k.to_string());
        }
    }
    keys
}

/// Generate a fresh ed25519 keypair, returned as `(private_b64, public_b64)`.
/// The private key is the 32-byte seed; the public key is the 32-byte verifying
/// key. Used by `dotenv-cloud keygen`.
pub fn generate_keypair() -> Result<(String, String), String> {
    use ed25519_dalek::SigningKey;
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("rng error: {e}"))?;
    let sk = SigningKey::from_bytes(&seed);
    let vk = sk.verifying_key();
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok((b64.encode(sk.to_bytes()), b64.encode(vk.to_bytes())))
}

/// Sign `bytes` with a base64 ed25519 private key (32-byte seed), returning a
/// base64 signature compatible with [`verify_signature`]. Used by
/// `dotenv-cloud sign`.
pub fn sign_bytes(private_key_b64: &str, bytes: &[u8]) -> Result<String, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(private_key_b64.trim())
        .map_err(|e| format!("invalid private key base64: {e}"))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "private key must be 32 bytes".to_string())?;
    let sk = SigningKey::from_bytes(&key_arr);
    let sig = sk.sign(bytes);
    Ok(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()))
}

// ---------------------------------------------------------------------------
// Installer
// ---------------------------------------------------------------------------

/// A record of an installed provider, for the lockfile.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `name` (short) retained for diagnostics; lockfile keys on `package`.
pub struct InstalledRecord {
    pub name: String,
    pub package: String,
    pub version: String,
    pub schemes: Vec<String>,
    pub source: String,
    pub sha256: String,
}

/// Drives provider installation against a registry index.
pub struct Installer {
    pub index: Index,
    pub registry_url: String,
    pub allow_unsigned: bool,
    pub public_key: Option<String>,
}

impl Installer {
    /// Load the index from a registry URL.
    pub fn load(
        registry_url: &str,
        allow_unsigned: bool,
        public_key: Option<String>,
    ) -> Result<Installer, String> {
        let bytes = fetch_bytes(registry_url)?;
        let index = Index::parse(&bytes)?;
        // schema_version 0 means "unspecified" (older index); 1 is current.
        if index.schema_version > 1 {
            return Err(format!(
                "registry index schema_version {} is newer than supported (1); upgrade dotenv-cloud",
                index.schema_version
            ));
        }
        Ok(Installer {
            index,
            registry_url: registry_url.to_string(),
            allow_unsigned,
            public_key,
        })
    }

    /// Install a provider by short name (optionally `name@version`) into `scope`.
    pub fn install(&self, name_spec: &str, scope: Scope) -> Result<InstalledRecord, String> {
        let (name, want_version) = match name_spec.split_once('@') {
            Some((n, v)) => (n, Some(v)),
            None => (name_spec, None),
        };

        let (short, provider) = self
            .index
            .provider(name)
            .ok_or_else(|| format!("provider `{name}` not found in registry"))?;
        let (version, ver) = provider.select_version(want_version)?;

        let target = current_target();
        let artifact = ver.targets.get(&target).ok_or_else(|| {
            format!(
                "no `{}` build for target `{target}` (available: {})",
                provider.package,
                ver.targets.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;

        // Download + verify integrity.
        let bytes = fetch_bytes(&artifact.url)?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(&artifact.sha256) {
            return Err(format!(
                "sha256 mismatch for {}: expected {}, got {actual}",
                provider.package, artifact.sha256
            ));
        }

        // Signature policy. By default every artifact must carry a signature
        // that verifies against a trusted key (built-in release key or a key
        // configured for a custom registry). `--allow-unsigned` relaxes this.
        let keys = trusted_keys(self.public_key.as_deref());
        match &artifact.signature {
            Some(sig) => {
                if keys.is_empty() {
                    if !self.allow_unsigned {
                        return Err(format!(
                            "{} is signed but no trusted public key is available to verify it; \
                             set [provider_registry].public_key or pass --allow-unsigned",
                            provider.package
                        ));
                    }
                } else if !keys
                    .iter()
                    .any(|k| verify_signature(&bytes, sig, k).is_ok())
                {
                    return Err(format!(
                        "signature verification failed for {} (not signed by a trusted key)",
                        provider.package
                    ));
                }
            }
            None => {
                if !self.allow_unsigned {
                    return Err(format!(
                        "{} has no signature; pass --allow-unsigned or set allow_unsigned=true to install unsigned providers",
                        provider.package
                    ));
                }
            }
        }

        // Unpack and install.
        let dest = scope.providers_root()?.join(short);
        install_archive(&bytes, &dest)?;

        Ok(InstalledRecord {
            name: short.clone(),
            package: provider.package.clone(),
            version,
            schemes: provider.schemes.clone(),
            source: format!("registry:{}", self.registry_url),
            sha256: artifact.sha256.clone(),
        })
    }
}

/// Unpack a `.tar.gz` archive and install its `manifest.toml` + executable into
/// `dest`. The archive is expected to contain a single top-level directory.
fn install_archive(gz_bytes: &[u8], dest: &Path) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("dotenv-cloud-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;

    let decoder = flate2::read::GzDecoder::new(gz_bytes);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&tmp)
        .map_err(|e| format!("failed to unpack archive: {e}"))?;

    let manifest_path = find_file(&tmp, "manifest.toml")
        .ok_or_else(|| "archive does not contain manifest.toml".to_string())?;
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(&manifest_text)?;

    let exe_src = manifest_path.parent().unwrap().join(&manifest.executable);
    if !exe_src.is_file() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "archive manifest names executable `{}` which is missing",
            manifest.executable
        ));
    }

    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    std::fs::copy(&manifest_path, dest.join("manifest.toml")).map_err(|e| e.to_string())?;
    let exe_dest = dest.join(&manifest.executable);
    std::fs::copy(&exe_src, &exe_dest).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe_dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Recursively search `dir` for a file named `name`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

/// Write or update a provider entry in the lockfile.
pub fn upsert_lockfile(path: &Path, record: &InstalledRecord) -> Result<(), String> {
    let mut doc: toml::Value = if path.exists() {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        toml::from_str(&text).map_err(|e| format!("invalid lockfile: {e}"))?
    } else {
        let mut t = toml::value::Table::new();
        t.insert("version".into(), toml::Value::Integer(1));
        toml::Value::Table(t)
    };

    let table = doc.as_table_mut().ok_or("lockfile is not a table")?;
    let providers = table
        .entry("provider")
        .or_insert_with(|| toml::Value::Array(vec![]))
        .as_array_mut()
        .ok_or("lockfile `provider` is not an array")?;

    let mut entry = toml::value::Table::new();
    entry.insert("name".into(), toml::Value::String(record.package.clone()));
    entry.insert(
        "version".into(),
        toml::Value::String(record.version.clone()),
    );
    entry.insert(
        "schemes".into(),
        toml::Value::Array(
            record
                .schemes
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    entry.insert("source".into(), toml::Value::String(record.source.clone()));
    entry.insert("sha256".into(), toml::Value::String(record.sha256.clone()));

    // Replace an existing entry with the same package name, else append.
    if let Some(slot) = providers
        .iter_mut()
        .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(record.package.as_str()))
    {
        *slot = toml::Value::Table(entry);
    } else {
        providers.push(toml::Value::Table(entry));
    }

    let serialized = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(path, serialized).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_index_and_select_latest() {
        let json = r#"{
            "schema_version": 1,
            "providers": {
                "aws": {
                    "package": "dotenv-cloud-provider-aws",
                    "schemes": ["aws-sm", "aws-ssm"],
                    "versions": {
                        "0.1.0-beta.1": {"targets": {}},
                        "0.1.0": {"targets": {}}
                    }
                }
            }
        }"#;
        let index = Index::parse(json.as_bytes()).unwrap();
        assert_eq!(index.name_for_scheme("aws-sm"), Some("aws"));
        let (_, p) = index.provider("aws").unwrap();
        let (v, _) = p.select_version(None).unwrap();
        assert_eq!(v, "0.1.0"); // stable outranks beta
        let (v2, _) = p.select_version(Some("0.1.0-beta.1")).unwrap();
        assert_eq!(v2, "0.1.0-beta.1");
    }

    #[test]
    fn sha256_matches_known_vector() {
        // sha256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn ed25519_sign_then_verify_roundtrip() {
        use ed25519_dalek::{Signer, SigningKey};
        let seed = [7u8; 32];
        let sk = SigningKey::from_bytes(&seed);
        let vk = sk.verifying_key();
        let data = b"archive-bytes";
        let sig = sk.sign(data);

        let pk_b64 = base64::engine::general_purpose::STANDARD.encode(vk.to_bytes());
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

        assert!(verify_signature(data, &sig_b64, &pk_b64).is_ok());
        assert!(verify_signature(b"tampered", &sig_b64, &pk_b64).is_err());
    }

    #[test]
    fn keygen_then_sign_then_verify() {
        let (priv_b64, pub_b64) = generate_keypair().unwrap();
        let data = b"provider-archive.tar.gz";
        let sig = sign_bytes(&priv_b64, data).unwrap();
        assert!(verify_signature(data, &sig, &pub_b64).is_ok());
        assert!(verify_signature(b"other", &sig, &pub_b64).is_err());
    }

    #[test]
    fn generated_keys_are_unique() {
        let (a, _) = generate_keypair().unwrap();
        let (b, _) = generate_keypair().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn lockfile_upsert_replaces_same_package() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("dotenv-cloud.lock");
        let rec = InstalledRecord {
            name: "aws".into(),
            package: "dotenv-cloud-provider-aws".into(),
            version: "0.1.0-beta.1".into(),
            schemes: vec!["aws-sm".into(), "aws-ssm".into()],
            source: "registry:file://x".into(),
            sha256: "deadbeef".into(),
        };
        upsert_lockfile(&lock, &rec).unwrap();
        let mut rec2 = rec.clone();
        rec2.version = "0.1.0".into();
        upsert_lockfile(&lock, &rec2).unwrap();

        let text = std::fs::read_to_string(&lock).unwrap();
        assert_eq!(text.matches("dotenv-cloud-provider-aws").count(), 1);
        assert!(text.contains("0.1.0"));
        assert!(!text.contains("0.1.0-beta.1"));
    }
}
