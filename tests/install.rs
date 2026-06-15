//! Integration tests for registry-backed provider installation (spec §7.3).
//!
//! Builds a local `.tar.gz` provider archive and a `file://` registry index,
//! then drives `dotenv-cloud providers install` against them. The target triple
//! is pinned via `DOTENV_CLOUD_TARGET_OVERRIDE` so the test is host-independent.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

const TARGET: &str = "test-target";

/// Render a path for embedding in a `file://` URL / JSON index.
///
/// On Windows `Path::display` yields backslashes, which are invalid escape
/// sequences inside the JSON index string and would fail to parse. Forward
/// slashes are valid JSON and are accepted by `std::fs::read` on all platforms.
fn url_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dotenv-cloud"));
    c.env("DOTENV_CLOUD_TARGET_OVERRIDE", TARGET);
    c
}

/// Build a `.tar.gz` containing `<top>/manifest.toml` and `<top>/<exe>`.
fn build_archive(dir: &Path) -> (std::path::PathBuf, String) {
    let top = "dotenv-cloud-provider-fake-0.1.0";
    let manifest = r#"name = "dotenv-cloud-provider-fake"
version = "0.1.0"
protocol_version = "1"
executable = "dotenv-cloud-provider-fake"
schemes = ["aws-sm", "aws-ssm"]
description = "fake"
"#;

    let archive_path = dir.join("fake.tar.gz");
    let file = std::fs::File::create(&archive_path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);

    let mut add = |name: &str, bytes: &[u8], mode: u32| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, format!("{top}/{name}"), bytes)
            .unwrap();
    };
    add("manifest.toml", manifest.as_bytes(), 0o644);
    add("dotenv-cloud-provider-fake", b"#!/bin/sh\nexit 0\n", 0o755);
    tar.into_inner().unwrap().finish().unwrap();

    let bytes = std::fs::read(&archive_path).unwrap();
    let mut h = Sha256::new();
    h.update(&bytes);
    let sha = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    (archive_path, sha)
}

fn write_index(dir: &Path, archive: &Path, sha: &str) -> std::path::PathBuf {
    let index = format!(
        r#"{{
          "schema_version": 1,
          "providers": {{
            "aws": {{
              "package": "dotenv-cloud-provider-fake",
              "schemes": ["aws-sm", "aws-ssm"],
              "versions": {{
                "0.1.0": {{ "targets": {{
                  "{TARGET}": {{ "url": "file://{archive}", "sha256": "{sha}" }}
                }} }}
              }}
            }}
          }}
        }}"#,
        archive = url_path(archive),
    );
    let path = dir.join("index.json");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(index.as_bytes())
        .unwrap();
    path
}

#[test]
fn install_verifies_and_places_provider() {
    let reg = tempfile::tempdir().unwrap();
    let (archive, sha) = build_archive(reg.path());
    let index = write_index(reg.path(), &archive, &sha);
    let index_url = format!("file://{}", url_path(&index));

    let proj = tempfile::tempdir().unwrap();

    // Unsigned install is refused without opt-in.
    let refused = bin()
        .current_dir(proj.path())
        .args(["providers", "install", "aws", "--registry", &index_url])
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "unsigned install should be refused"
    );

    // With --allow-unsigned it installs.
    let ok = bin()
        .current_dir(proj.path())
        .args([
            "providers",
            "install",
            "aws",
            "--registry",
            &index_url,
            "--allow-unsigned",
        ])
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    assert!(proj
        .path()
        .join(".dotenv-cloud/providers/aws/manifest.toml")
        .is_file());
    assert!(proj
        .path()
        .join(".dotenv-cloud/providers/aws/dotenv-cloud-provider-fake")
        .is_file());

    let lock = std::fs::read_to_string(proj.path().join("dotenv-cloud.lock")).unwrap();
    assert!(lock.contains("dotenv-cloud-provider-fake"));
    assert!(lock.contains(&sha));
}

#[test]
fn install_rejects_sha_mismatch() {
    let reg = tempfile::tempdir().unwrap();
    let (archive, _sha) = build_archive(reg.path());
    let index = write_index(reg.path(), &archive, &"00".repeat(32));
    let index_url = format!("file://{}", url_path(&index));

    let proj = tempfile::tempdir().unwrap();
    let out = bin()
        .current_dir(proj.path())
        .args([
            "providers",
            "install",
            "aws",
            "--registry",
            &index_url,
            "--allow-unsigned",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("sha256 mismatch"));
}
