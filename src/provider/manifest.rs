//! Provider manifest parsing and discovery (spec §7.2).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A provider manifest, as stored alongside an installed plugin executable.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
    pub executable: String,
    pub schemes: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub integrity: Option<Integrity>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `signature` verified during install (not in this build).
pub struct Integrity {
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
}

/// A discovered, installed provider: its manifest and the directory it lives in.
#[derive(Debug, Clone)]
pub struct InstalledProvider {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

impl InstalledProvider {
    /// Absolute path to the plugin executable.
    pub fn executable_path(&self) -> PathBuf {
        let exe = &self.manifest.executable;
        let p = Path::new(exe);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.dir.join(exe)
        }
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        toml::from_str(text).map_err(|e| format!("invalid manifest: {e}"))
    }
}

/// Provider discovery directories in order (spec §7.2): project-local first,
/// then the user provider directory. Explicit paths from config are handled by
/// the caller (highest priority).
pub fn discovery_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 2. Project-local provider directory.
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join(".dotenv-cloud").join("providers"));
    }

    // 3. User provider directory (platform-specific).
    if let Some(user) = user_provider_dir() {
        dirs.push(user);
    }

    dirs
}

/// The platform-appropriate user provider directory (spec §7.2).
pub fn user_provider_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        return Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("dotenv-cloud")
                .join("providers"),
        );
    }
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        return Some(PathBuf::from(local).join("dotenv-cloud").join("providers"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })?;
        return Some(base.join("dotenv-cloud").join("providers"));
    }
    #[allow(unreachable_code)]
    None
}

/// Scan a directory for installed providers. Each provider lives in a
/// subdirectory containing a `manifest.toml`.
pub fn scan_dir(dir: &Path) -> Vec<InstalledProvider> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest_path) {
            if let Ok(manifest) = Manifest::parse(&text) {
                out.push(InstalledProvider {
                    manifest,
                    dir: path,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest() {
        let text = r#"
name = "dotenv-cloud-provider-aws"
version = "1.0.0"
protocol_version = "1"
executable = "dotenv-cloud-provider-aws"
schemes = ["aws-sm", "aws-ssm"]
description = "AWS provider"

[integrity]
sha256 = "abc"
"#;
        let m = Manifest::parse(text).unwrap();
        assert_eq!(m.name, "dotenv-cloud-provider-aws");
        assert_eq!(m.schemes, vec!["aws-sm", "aws-ssm"]);
        assert_eq!(m.integrity.unwrap().sha256.as_deref(), Some("abc"));
    }
}
