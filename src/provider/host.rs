//! Plugin process host (spec §7.4).
//!
//! Launches a provider plugin as a child process and speaks newline-delimited
//! JSON over its stdin/stdout. Stdout is parsed strictly as protocol JSON; any
//! non-protocol line is an error. Stderr is drained and treated as untrusted
//! redacted diagnostics.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

use crate::error::ProviderErrorClass;
use crate::secret::{ResolvedSecret, SecretMetadata};
use crate::uri::SecretReference;

use super::manifest::InstalledProvider;
use super::protocol::{
    HandshakeRequest, HandshakeResult, PluginMessage, ReferencePayload, ResolveRequest,
    PROTOCOL_VERSION,
};

/// An error from talking to a plugin. Carries a provider error class for exit
/// code mapping. Never contains secret material.
#[derive(Debug)]
pub struct HostError {
    pub class: ProviderErrorClass,
    pub message: String,
    pub reference_redacted: Option<String>,
}

impl HostError {
    fn internal(msg: impl Into<String>) -> Self {
        HostError {
            class: ProviderErrorClass::Internal,
            message: msg.into(),
            reference_redacted: None,
        }
    }
}

/// A running plugin process and its protocol streams.
pub struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub info_name: String,
    request_counter: u64,
}

impl PluginProcess {
    /// Launch the plugin executable and perform the handshake.
    pub async fn launch(
        provider: &InstalledProvider,
        handshake_timeout: Duration,
    ) -> Result<Self, HostError> {
        let exe = provider.executable_path();
        let mut child = tokio::process::Command::new(&exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                HostError::internal(format!(
                    "failed to launch provider `{}` ({}): {e}",
                    provider.manifest.name,
                    exe.display()
                ))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostError::internal("no plugin stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::internal("no plugin stdout"))?;

        // Drain stderr in the background so the plugin never blocks on a full
        // pipe. Diagnostics are untrusted; we discard them here (spec §13.2).
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(_line)) = lines.next_line().await {
                    // Intentionally discarded; never echoed without redaction.
                }
            });
        }

        let mut proc = PluginProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            info_name: provider.manifest.name.clone(),
            request_counter: 0,
        };

        proc.handshake(handshake_timeout).await?;
        Ok(proc)
    }

    async fn handshake(&mut self, t: Duration) -> Result<(), HostError> {
        let req = HandshakeRequest::new();
        self.write_json(&req).await?;

        let line = self.read_line(t).await?;
        let resp: HandshakeResult = serde_json::from_str(&line).map_err(|e| {
            HostError::internal(format!(
                "invalid handshake response from `{}`: {e}",
                self.info_name
            ))
        })?;
        if resp.protocol_version != PROTOCOL_VERSION {
            return Err(HostError::internal(format!(
                "provider `{}` speaks protocol {} but core requires {}",
                self.info_name, resp.protocol_version, PROTOCOL_VERSION
            )));
        }
        Ok(())
    }

    /// Resolve a single reference. `provider_config` is a JSON object passed
    /// through to the plugin.
    pub async fn resolve(
        &mut self,
        reference: &SecretReference,
        profile: &str,
        provider_config: serde_json::Value,
        t: Duration,
    ) -> Result<ResolvedSecret, HostError> {
        self.request_counter += 1;
        let request_id = format!("req-{}", self.request_counter);

        let req = ResolveRequest {
            kind: "resolve",
            request_id: request_id.clone(),
            profile: profile.to_string(),
            reference: ReferencePayload {
                original: reference.original.clone(),
                scheme: reference.scheme.clone(),
                authority: reference.authority.clone(),
                path: reference.path.clone(),
                fragment: reference.fragment.clone(),
                query: reference.query.clone(),
            },
            provider_config,
        };
        self.write_json(&req).await?;

        let line = self.read_line(t).await.map_err(|mut e| {
            if e.class == ProviderErrorClass::Internal {
                e.class = ProviderErrorClass::Timeout;
            }
            e.reference_redacted = Some(reference.redacted());
            e
        })?;

        let msg: PluginMessage = serde_json::from_str(&line).map_err(|e| HostError {
            class: ProviderErrorClass::Internal,
            message: format!("non-protocol output from `{}`: {e}", self.info_name),
            reference_redacted: Some(reference.redacted()),
        })?;

        match msg {
            PluginMessage::ResolveResult {
                value, metadata, ..
            } => Ok(ResolvedSecret {
                value: secrecy::SecretString::new(value),
                metadata: SecretMetadata {
                    provider: metadata
                        .provider
                        .unwrap_or_else(|| reference.scheme.clone()),
                    source_uri_redacted: reference.redacted(),
                    version: metadata.version,
                    cache_hit: false,
                },
            }),
            PluginMessage::Error {
                class,
                message,
                reference: r,
                ..
            } => Err(HostError {
                class: ProviderErrorClass::parse(&class).unwrap_or(ProviderErrorClass::Internal),
                message,
                reference_redacted: r.or_else(|| Some(reference.redacted())),
            }),
        }
    }

    async fn write_json<T: serde::Serialize>(&mut self, value: &T) -> Result<(), HostError> {
        let mut buf = serde_json::to_vec(value).map_err(|e| HostError::internal(e.to_string()))?;
        buf.push(b'\n');
        self.stdin
            .write_all(&buf)
            .await
            .map_err(|e| HostError::internal(format!("write to `{}`: {e}", self.info_name)))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| HostError::internal(format!("flush to `{}`: {e}", self.info_name)))?;
        Ok(())
    }

    async fn read_line(&mut self, t: Duration) -> Result<String, HostError> {
        let mut line = String::new();
        let read = timeout(t, self.stdout.read_line(&mut line)).await;
        match read {
            Err(_) => Err(HostError {
                class: ProviderErrorClass::Timeout,
                message: format!("provider `{}` timed out after {:?}", self.info_name, t),
                reference_redacted: None,
            }),
            Ok(Err(e)) => Err(HostError::internal(format!(
                "read from `{}`: {e}",
                self.info_name
            ))),
            Ok(Ok(0)) => Err(HostError::internal(format!(
                "provider `{}` closed its output unexpectedly",
                self.info_name
            ))),
            Ok(Ok(_)) => Ok(line.trim_end().to_string()),
        }
    }

    /// Best-effort shutdown of the plugin process.
    pub async fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.kill().await;
    }
}
