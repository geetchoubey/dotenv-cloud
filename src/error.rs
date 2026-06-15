//! Error types and process exit codes.
//!
//! Exit codes follow TECHNICAL_SPEC.md §11.4.

use std::fmt;

/// Process exit codes defined by the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Success` documents code 0; constructed implicitly via Ok paths.
pub enum ExitCode {
    Success = 0,
    Runtime = 1,
    Usage = 2,
    Config = 3,
    DotenvParse = 4,
    SecretResolution = 5,
    ProviderAuth = 6,
    ProviderNetwork = 7,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Top-level error for the CLI. Each variant maps to an [`ExitCode`].
///
/// Errors must never carry resolved secret values. References embedded in
/// messages are expected to already be redacted by the caller.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Runtime(String),

    #[error("usage error: {0}")]
    Usage(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("dotenv parse error: {0}")]
    DotenvParse(String),

    #[error("secret resolution error: {0}")]
    SecretResolution(String),

    #[error("provider authentication/permission error: {0}")]
    ProviderAuth(String),

    #[error("provider timeout/network error: {0}")]
    ProviderNetwork(String),
}

impl CliError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CliError::Runtime(_) => ExitCode::Runtime,
            CliError::Usage(_) => ExitCode::Usage,
            CliError::Config(_) => ExitCode::Config,
            CliError::DotenvParse(_) => ExitCode::DotenvParse,
            CliError::SecretResolution(_) => ExitCode::SecretResolution,
            CliError::ProviderAuth(_) => ExitCode::ProviderAuth,
            CliError::ProviderNetwork(_) => ExitCode::ProviderNetwork,
        }
    }
}

/// Provider error classes from spec §7.7. Used to map plugin protocol errors
/// onto the appropriate [`ExitCode`] and human-readable diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProviderErrorClass {
    AuthenticationFailed,
    PermissionDenied,
    NotFound,
    InvalidReference,
    InvalidSecretPayload,
    Timeout,
    RateLimited,
    Network,
    ProviderUnavailable,
    Internal,
}

impl ProviderErrorClass {
    /// Map a provider error class onto the CLI exit code semantics.
    #[allow(dead_code)] // mirrored by pipeline::map_provider_error; kept as canonical mapping.
    pub fn exit_code(self) -> ExitCode {
        match self {
            ProviderErrorClass::AuthenticationFailed
            | ProviderErrorClass::PermissionDenied => ExitCode::ProviderAuth,
            ProviderErrorClass::Timeout
            | ProviderErrorClass::RateLimited
            | ProviderErrorClass::Network
            | ProviderErrorClass::ProviderUnavailable => ExitCode::ProviderNetwork,
            _ => ExitCode::SecretResolution,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "AuthenticationFailed" => Self::AuthenticationFailed,
            "PermissionDenied" => Self::PermissionDenied,
            "NotFound" => Self::NotFound,
            "InvalidReference" => Self::InvalidReference,
            "InvalidSecretPayload" => Self::InvalidSecretPayload,
            "Timeout" => Self::Timeout,
            "RateLimited" => Self::RateLimited,
            "Network" => Self::Network,
            "ProviderUnavailable" => Self::ProviderUnavailable,
            "Internal" => Self::Internal,
            _ => return None,
        })
    }
}

impl fmt::Display for ProviderErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::AuthenticationFailed => "AuthenticationFailed",
            Self::PermissionDenied => "PermissionDenied",
            Self::NotFound => "NotFound",
            Self::InvalidReference => "InvalidReference",
            Self::InvalidSecretPayload => "InvalidSecretPayload",
            Self::Timeout => "Timeout",
            Self::RateLimited => "RateLimited",
            Self::Network => "Network",
            Self::ProviderUnavailable => "ProviderUnavailable",
            Self::Internal => "Internal",
        };
        f.write_str(s)
    }
}

pub type CliResult<T> = Result<T, CliError>;
