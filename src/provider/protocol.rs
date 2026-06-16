//! Provider plugin wire protocol (spec §7.4).
//!
//! Messages are newline-delimited JSON over the plugin's stdin/stdout. Stderr
//! is reserved for redacted diagnostics. Stdout must contain only protocol JSON.
//!
//! Some response fields are deserialized for protocol fidelity even though the
//! V1 core does not yet act on all of them.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1";

/// Handshake request (core -> plugin).
#[derive(Debug, Serialize)]
pub struct HandshakeRequest {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub protocol_version: &'static str,
    pub dotenv_cloud_version: String,
}

impl HandshakeRequest {
    pub fn new() -> Self {
        HandshakeRequest {
            kind: "handshake",
            protocol_version: PROTOCOL_VERSION,
            dotenv_cloud_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl Default for HandshakeRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// Handshake response (plugin -> core).
#[derive(Debug, Deserialize)]
pub struct HandshakeResult {
    #[serde(rename = "type")]
    pub kind: String,
    pub protocol_version: String,
    pub plugin: PluginInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub schemes: Vec<String>,
}

/// Describe request (core -> plugin): asks for the plugin's configurable
/// settings. Used by `dotenv-cloud init` to drive interactive configuration.
#[derive(Debug, Serialize)]
pub struct DescribeRequest {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl DescribeRequest {
    pub fn new() -> Self {
        DescribeRequest { kind: "describe" }
    }
}

impl Default for DescribeRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// Describe response (plugin -> core).
#[derive(Debug, Deserialize)]
pub struct DescribeResult {
    #[serde(default)]
    pub config_schema: Vec<ConfigField>,
}

/// One configurable provider setting (mirrors the shared protocol crate).
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub kind: FieldKind,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    #[default]
    String,
    Bool,
    Integer,
}

/// The reference payload sent to the plugin (matches spec §7.4).
#[derive(Debug, Serialize)]
pub struct ReferencePayload {
    pub original: String,
    pub scheme: String,
    pub authority: Option<String>,
    pub path: String,
    pub fragment: Option<String>,
    pub query: BTreeMap<String, String>,
}

/// Resolve request (core -> plugin).
#[derive(Debug, Serialize)]
pub struct ResolveRequest {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request_id: String,
    pub environment: String,
    pub reference: ReferencePayload,
    pub provider_config: serde_json::Value,
}

/// A protocol message received from the plugin on stdout. The `type` field
/// discriminates the variant.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum PluginMessage {
    #[serde(rename = "resolve_result")]
    ResolveResult {
        request_id: String,
        value: String,
        #[serde(default)]
        metadata: ResolveMetadata,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        request_id: Option<String>,
        class: String,
        message: String,
        #[serde(default)]
        reference: Option<String>,
    },
}

#[derive(Debug, Default, Deserialize)]
pub struct ResolveMetadata {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}
