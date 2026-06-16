# dotenv-cloud Technical Specification

## 1. Purpose and Scope

`dotenv-cloud` is a CLI-only runtime tool that extends traditional dotenv behavior with secure remote secret resolution. It loads local dotenv files, resolves remote secret references through externally installed provider plugins, merges all values into a deterministic runtime environment, and injects that environment into a child process.

The primary workflow is:

```sh
dotenv-cloud run -- npm start
dotenv-cloud run -- python app.py
dotenv-cloud run -- java -jar app.jar
```

`dotenv-cloud` is stateless and ephemeral during secret resolution. It does not run a daemon, host a backend service, mutate the parent shell, or persist resolved secrets in V1. Provider plugins may be installed on disk, but resolved secret values and provider credentials are never persisted by `dotenv-cloud`.

## 2. Engineering Assumptions

1. The initial implementation targets Linux, macOS, and Windows.
2. The primary implementation language is Rust.
3. `.env` parsing must be compatible with commonly accepted dotenv behavior: comments, quoted values, escaped characters, blank lines, and optional `export` prefixes.
4. Variable interpolation is supported only for local values by default. Remote reference resolution is explicit through URI schemes.
5. V1 does not provide persistent secret storage, encrypted local caches, sync, teams, policy management, or hosted control plane features.
6. Remote providers are contacted only during a command execution unless a command explicitly validates or resolves values.
7. All cloud and vault integrations are external provider plugins in V1, including AWS and Vault.
8. All provider credentials are obtained from provider-native mechanisms and are never managed by `dotenv-cloud` itself.
9. Provider plugin binaries are installation artifacts, not secret caches.

## 3. Goals and Non-Goals

### 3.1 Goals

- Load `.env` files with dotenv-compatible parsing.
- Resolve remote secret URI references at runtime.
- Support AWS Secrets Manager, AWS SSM Parameter Store, and HashiCorp Vault KV through externally installed provider plugins.
- Merge default config values, local files, remote values, system environment, and CLI overrides.
- Spawn child processes with the merged environment.
- Provide shell export output for supported shells.
- Validate configuration and provider reachability.
- Avoid logging or persisting secrets.
- Ship a small core binary where platform constraints allow, with provider integrations installed separately.

### 3.2 Non-Goals

- Modifying the parent shell environment directly.
- Providing a background agent, daemon, or local secret service.
- Acting as a general-purpose secrets manager.
- Replacing IAM, Vault policy, audit, rotation, or provider-native access control.
- Persisting decrypted or resolved secret values in V1.
- Bundling provider SDKs into the core binary.
- Providing telemetry by default.

## 4. Architecture

```text
                         +-----------------------+
                         |       CLI Parser      |
                         |  run/export/resolve   |
                         +-----------+-----------+
                                     |
                                     v
                         +-----------------------+
                         |    Config Loader      |
                         | dotenv-cloud.toml     |
                         | optional YAML         |
                         +-----------+-----------+
                                     |
                                     v
       +-----------------------------+-----------------------------+
       |                                                           |
       v                                                           v
+--------------+                                      +----------------------+
| Dotenv Parser|                                      | System Env Reader    |
| .env files   |                                      | process environment  |
+------+-------+                                      +----------+-----------+
       |                                                     |
       v                                                     v
+----------------------+                         +--------------------------+
| Source Normalization |                         | CLI Override Normalizer  |
| keys, raw values     |                         | flags and --env values   |
+----------+-----------+                         +-------------+------------+
           |                                                       |
           +-------------------------+-----------------------------+
                                     |
                                     v
                         +-----------------------+
                         |    Merge Engine       |
                         | precedence + policy   |
                         +-----------+-----------+
                                     |
                                     v
                         +-----------------------+
                         | Secret Reference Scan |
                         | aws-secrets:// vault://    |
                         +-----------+-----------+
                                     |
                                     v
                         +-----------------------+
                         | Provider Plugin Host  |
                         | manifest + protocol   |
                         +-----------+-----------+
                                     |
             +-----------------------+-----------------------+
             |                       |                       |
             v                       v                       v
   +-------------------+   +-------------------+   +-------------------+
   | aws plugin        |   | vault plugin      |   | other plugins     |
   | external process  |   | external process  |   | external process  |
   +---------+---------+   +---------+---------+   +---------+---------+
             |                       |                       |
             +-----------------------+-----------------------+
                                     |
                                     v
                         +-----------------------+
                         | In-Memory Secret Cache|
                         | per execution only    |
                         +-----------+-----------+
                                     |
                                     v
                         +-----------------------+
                         | Final Env Map         |
                         | redaction metadata    |
                         +-----------+-----------+
                                     |
              +----------------------+----------------------+
              |                                             |
              v                                             v
    +---------------------+                      +----------------------+
    | Subprocess Executor |                      | Export Renderer      |
    | run -- <command>    |                      | bash/zsh/fish/ps1    |
    +---------------------+                      +----------------------+
```

## 5. Source Precedence

The default precedence order, from highest to lowest, is:

1. CLI flags
2. System environment variables
3. Resolved remote secrets
4. `.env.local`
5. `.env`
6. Default config values

When the same key appears in multiple sources, the highest-precedence source wins. The merge engine must record the source of the winning value and optionally the shadowed sources for diagnostics.

### 5.1 Important Precedence Detail

Remote secrets are not a separate file source. They are resolved values originating from lower-precedence local/config inputs. By default, once a value is recognized as a remote URI and successfully resolved, the resolved value is placed at the `resolved remote secrets` precedence level.

Example:

```dotenv
# .env
DB_PASSWORD=aws-secrets://prod/db/password
```

If `DB_PASSWORD` is not provided by CLI flags or the system environment, `dotenv-cloud` resolves the AWS Secrets Manager URI and uses the resolved secret.

If the parent process already has `DB_PASSWORD` set, the system environment value wins and the remote value is not required unless validation mode asks for full resolution.

### 5.2 Configurable Precedence

Configuration may override precedence:

```toml
[precedence]
order = ["cli", "remote", "system", "env.local", "env", "defaults"]
```

Allowed source identifiers:

- `cli`
- `system`
- `remote`
- `env.local`
- `env`
- `defaults`

The configured order must contain each source at most once. Omitted sources keep their default relative order below all explicitly listed sources.

For safety, `dotenv-cloud validate` must warn when `remote` is configured above `cli` or when `system` is configured below `.env`, because both choices can surprise operators.

## 6. Secret Reference Model

### 6.1 URI Detection

A value is considered a secret reference if it begins with a supported provider URI scheme:

- `aws-secrets://`
- `aws-ssm://`
- `vault://`

Detection is exact and case-sensitive. Plain values such as `REGION=us-east-1` are not modified.

### 6.2 Supported URI Forms

AWS Secrets Manager:

```dotenv
DB_PASSWORD=aws-secrets://prod/db/password
DB_PASSWORD=aws-secrets://prod/db/password#password
DB_PASSWORD=aws-secrets://prod/db/password?version_id=abc
DB_PASSWORD=aws-secrets://prod/db/password?version_stage=AWSCURRENT
```

AWS SSM Parameter Store:

```dotenv
API_TOKEN=aws-ssm:///prod/app/api_token
API_TOKEN=aws-ssm:///prod/app/api_token?with_decryption=true
REGION=aws-ssm:///prod/app/region
```

Vault KV:

```dotenv
API_KEY=vault://kv/data/app#api_key
API_KEY=vault://kv/app#api_key
API_KEY=vault://secret/data/app#api_key
```

### 6.3 URI Semantics

The URI parser must preserve the scheme, authority, path, fragment, and query parameters using a structured URI parser. String splitting is not acceptable except after parsed components are available.

General rules:

- The URI scheme selects the provider.
- The URI path identifies the provider-side secret path or parameter name.
- A URI fragment identifies a field inside a structured secret payload.
- Query parameters provide provider-specific options.
- Unknown query parameters cause validation warnings by default and hard errors in `--strict` mode.

### 6.4 Structured Secret Payloads

If a provider returns a JSON object and the URI contains a fragment, the fragment is used as a top-level object key unless JSON Pointer mode is explicitly enabled.

Example:

```dotenv
API_KEY=aws-secrets://prod/app/config#api_key
```

If the secret string is:

```json
{"api_key":"abc","db_password":"xyz"}
```

the resolved value is `abc`.

If no fragment is present:

- Plain string secrets resolve to the full string.
- JSON object secrets resolve to the original serialized string unless provider configuration requires a fragment.

### 6.5 In-Memory Cache

Resolved secrets are cached in memory for the duration of a single command execution. Cache key:

```text
provider_id + normalized_uri + environment_name
```

The cache prevents duplicate network calls when multiple environment keys reference the same remote secret. The cache is destroyed when the process exits.

V1 must not write resolved secrets to disk.

## 7. Provider Plugin System

V1 providers are external plugins. The core `dotenv-cloud` binary must not link AWS, Vault, Azure, GCP, or other provider SDKs. The core binary is responsible for parsing dotenv/config files, detecting secret references, enforcing precedence, managing process execution, loading provider manifests, launching provider plugins, and enforcing redaction.

Provider plugins are separate executables installed into a local provider directory. They communicate with the core process through a stable JSON protocol over stdin/stdout. This avoids dynamic library ABI instability, keeps the core binary small, and allows providers to be implemented and released independently.

### 7.1 Provider Plugin Responsibilities

A provider plugin must:

- Advertise supported URI schemes.
- Authenticate through provider-native mechanisms.
- Resolve a parsed secret reference.
- Validate provider configuration and credential availability.
- Return structured errors.
- Never print secrets to stderr or logs.
- Exit after the current command unless the core process keeps it alive for the current execution.

The official V1 provider plugins are:

| Plugin | Schemes | Provider APIs |
| --- | --- | --- |
| `dotenv-cloud-provider-aws` | `aws-secrets`, `aws-ssm` | AWS Secrets Manager, AWS SSM Parameter Store |
| `dotenv-cloud-provider-vault` | `vault` | HashiCorp Vault KV v1/v2 |

Azure Key Vault should use the same external plugin model if added:

| Plugin | Schemes | Provider APIs |
| --- | --- | --- |
| `dotenv-cloud-provider-azure-keyvault` | `azure-keyvault`, `az-kv` | Azure Key Vault Secrets |

### 7.2 Plugin Discovery

Provider discovery order:

1. Explicit plugin paths in `dotenv-cloud.toml`.
2. Project-local provider directory: `.dotenv-cloud/providers`.
3. User provider directory:
   - Unix: `${XDG_DATA_HOME:-~/.local/share}/dotenv-cloud/providers`
   - macOS: `~/Library/Application Support/dotenv-cloud/providers`
   - Windows: `%LOCALAPPDATA%\dotenv-cloud\providers`
4. Built-in registry metadata for install suggestions only.

The core binary may include provider registry metadata such as names, schemes, and download locations. It must not include provider implementation code or provider SDKs.

Installed providers must include a manifest:

```toml
name = "dotenv-cloud-provider-aws"
version = "1.0.0"
protocol_version = "1"
executable = "dotenv-cloud-provider-aws"
schemes = ["aws-secrets", "aws-ssm"]
description = "AWS Secrets Manager and SSM provider for dotenv-cloud"

[integrity]
sha256 = "..."
signature = "..."
```

The registry must reject duplicate scheme ownership unless the user explicitly selects a provider for that scheme in config.

### 7.3 Provider Installation and Locking

Provider installation is a persistent operation, but it stores provider binaries only. It must not store resolved secrets.

`dotenv-cloud init` may fetch required providers after scanning config and dotenv files:

```sh
dotenv-cloud init
```

Initialization flow:

1. Locate config and dotenv files.
2. Scan values for provider URI schemes.
3. Map required schemes to provider packages.
4. Ask for confirmation unless `--yes` is supplied.
5. Download provider archives from the configured registry.
6. Verify checksum and signature.
7. Install provider executables and manifests.
8. Write or update `dotenv-cloud.lock`.

Explicit install is also supported:

```sh
dotenv-cloud providers install aws
dotenv-cloud providers install vault
dotenv-cloud providers install azure-keyvault
```

Lockfile example:

```toml
version = 1

[[provider]]
name = "dotenv-cloud-provider-aws"
version = "1.0.0"
schemes = ["aws-secrets", "aws-ssm"]
source = "registry:official/aws"
sha256 = "..."

[[provider]]
name = "dotenv-cloud-provider-vault"
version = "1.0.0"
schemes = ["vault"]
source = "registry:official/vault"
sha256 = "..."
```

`dotenv-cloud run` must not auto-install providers by default. If a required provider is missing, it should fail with an actionable message:

```text
error: no provider installed for scheme aws-secrets
hint: run `dotenv-cloud init` or `dotenv-cloud providers install aws`
```

An explicit opt-in flag may install missing providers:

```sh
dotenv-cloud run --install-missing-providers -- npm start
```

This flag must verify signatures and must not install unsigned providers unless the user also passes an explicit unsafe development flag.

### 7.4 Provider Runtime Protocol

The core process launches provider plugins as child processes and communicates using newline-delimited JSON messages over stdin/stdout. Stderr is reserved for redacted diagnostics only.

Handshake request:

```json
{
  "type": "handshake",
  "protocol_version": "1",
  "dotenv_cloud_version": "1.0.0"
}
```

Handshake response:

```json
{
  "type": "handshake_result",
  "protocol_version": "1",
  "plugin": {
    "name": "dotenv-cloud-provider-aws",
    "version": "1.0.0",
    "schemes": ["aws-secrets", "aws-ssm"]
  }
}
```

Describe request/response (optional; used by `init` to drive interactive
provider configuration):

```json
{ "type": "describe" }
```

```json
{
  "type": "describe_result",
  "config_schema": [
    { "key": "region", "label": "AWS region", "kind": "string", "required": false, "secret": false },
    { "key": "ssm.with_decryption", "label": "Decrypt SSM SecureString parameters", "kind": "bool", "default": "true", "required": false, "secret": false }
  ]
}
```

Each field has a `key` (dotted keys nest into sub-tables), a `label` prompt, a
`kind` (`string` | `bool` | `integer`), an optional string `default`, and
`required` / `secret` flags. Plugins with no settings return an empty
`config_schema`.

Resolve request (`environment` was named `profile` before the environment
rename):

```json
{
  "type": "resolve",
  "request_id": "01J...",
  "environment": "dev",
  "reference": {
    "original": "aws-secrets://prod/db/password",
    "scheme": "aws-secrets",
    "authority": "prod",
    "path": "/db/password",
    "fragment": null,
    "query": {}
  },
  "provider_config": {
    "region": "us-east-1",
    "profile": "dev",
    "timeout_ms": 2000
  }
}
```

Resolve response:

```json
{
  "type": "resolve_result",
  "request_id": "01J...",
  "value": "secret-value",
  "metadata": {
    "provider": "aws-secrets",
    "version": "AWSCURRENT"
  }
}
```

Error response:

```json
{
  "type": "error",
  "request_id": "01J...",
  "class": "PermissionDenied",
  "message": "access denied",
  "reference": "aws-secrets://prod/db/[redacted]"
}
```

The core process must treat `value` as secret material immediately. It must store it only in redacting in-memory structures.

### 7.5 Core Provider Host Interface

The core implementation should use an internal host interface similar to:

```rust
#[async_trait]
pub trait ProviderHost: Send + Sync {
    fn schemes(&self) -> &[String];

    async fn handshake(&self) -> Result<PluginInfo, ProviderError>;

    async fn resolve(
        &self,
        reference: &SecretReference,
        ctx: &ProviderContext,
        cache: &SecretCache,
    ) -> Result<ResolvedSecret, ProviderError>;

    async fn validate(
        &self,
        ctx: &ProviderContext,
    ) -> Result<ProviderHealth, ProviderError>;
}
```

Core data types:

```rust
pub struct SecretReference {
    pub original: String,
    pub scheme: String,
    pub authority: Option<String>,
    pub path: String,
    pub fragment: Option<String>,
    pub query: BTreeMap<String, String>,
}

pub struct ResolvedSecret {
    pub value: SecretString,
    pub metadata: SecretMetadata,
}

pub struct SecretMetadata {
    pub provider: String,
    pub source_uri_redacted: String,
    pub version: Option<String>,
    pub cache_hit: bool,
}
```

`SecretString` must use a secrecy-aware type or equivalent wrapper that redacts debug output.

### 7.6 Provider Registration

Providers are registered by scheme from installed manifests:

```text
aws-secrets  -> dotenv-cloud-provider-aws
aws-ssm -> dotenv-cloud-provider-aws
vault   -> dotenv-cloud-provider-vault
```

The registry must reject duplicate schemes at startup.

### 7.7 Provider Error Classes

Provider errors must be classified for actionable output:

- `AuthenticationFailed`
- `PermissionDenied`
- `NotFound`
- `InvalidReference`
- `InvalidSecretPayload`
- `Timeout`
- `RateLimited`
- `Network`
- `ProviderUnavailable`
- `Internal`

Error messages must include key name, provider name, and redacted reference. They must not include resolved values, access tokens, raw request headers, or provider credential material.

## 8. Required Providers

The following providers are required for V1 as official external plugins. They are not compiled into the core `dotenv-cloud` binary.

### 8.1 AWS Secrets Manager

Implemented by:

```text
dotenv-cloud-provider-aws
```

Scheme:

```text
aws-secrets://
```

Authentication:

- AWS default credential chain.
- Supports IAM roles, environment credentials, shared config profiles, SSO, web identity, and instance/task role credentials through the AWS SDK.
- Optional profile selection through config or CLI flag.

Configuration:

```toml
[providers.aws]
region = "us-east-1"
profile = "dev"
timeout_ms = 2000
```

Resolution:

- Calls `GetSecretValue`.
- Supports `SecretString`.
- Supports `SecretBinary` only if `binary = "base64"` is supplied, otherwise returns `InvalidSecretPayload`.
- Supports `version_id` and `version_stage` query parameters.

Example:

```dotenv
DB_PASSWORD=aws-secrets://prod/db/password
API_KEY=aws-secrets://prod/app/config#api_key
```

Error behavior:

- Missing secret: `NotFound`.
- Access denied: `PermissionDenied`.
- Missing region: config error unless AWS SDK can infer one.
- JSON fragment missing: `InvalidSecretPayload`.

### 8.2 AWS SSM Parameter Store

Implemented by:

```text
dotenv-cloud-provider-aws
```

Scheme:

```text
aws-ssm://
```

Authentication:

- Same AWS default credential chain as AWS Secrets Manager.

Configuration:

```toml
[providers.aws]
region = "us-east-1"
profile = "dev"

[providers.aws.ssm]
with_decryption = true
```

Resolution:

- Calls `GetParameter`.
- Default `with_decryption = true`.
- Supports query parameter override:

```dotenv
API_TOKEN=aws-ssm:///prod/app/api_token?with_decryption=true
```

URI path rules:

- Parameter names are absolute paths.
- Because parameter names commonly start with `/`, the recommended URI form has three slashes:

```dotenv
KEY=aws-ssm:///prod/app/key
```

Error behavior:

- Parameter missing: `NotFound`.
- KMS decryption denied: `PermissionDenied`.
- Invalid path: `InvalidReference`.

### 8.3 HashiCorp Vault KV

Implemented by:

```text
dotenv-cloud-provider-vault
```

Scheme:

```text
vault://
```

Authentication:

- Token authentication through `VAULT_TOKEN`.
- Optional token file support using Vault-compatible defaults.
- AppRole support through config or environment variables.

Configuration:

```toml
[providers.vault]
address = "https://vault.example.com"
namespace = "team-a"
auth_method = "token"
timeout_ms = 2000

[providers.vault.kv]
default_version = "v2"

[provider_registry]
url = "https://providers.dotenv-cloud.dev/index.json"
allow_unsigned = false
install_scope = "project" # project | user
```

Token example:

```toml
[providers.vault]
auth_method = "token"
token_env = "VAULT_TOKEN"
```

AppRole example:

```toml
[providers.vault]
auth_method = "approle"
role_id_env = "VAULT_ROLE_ID"
secret_id_env = "VAULT_SECRET_ID"
```

Resolution:

- Supports KV v1 and KV v2.
- KV version may be inferred from config or URI path.
- URI fragment selects a field from the returned data object.

Examples:

```dotenv
API_KEY=vault://kv/data/app#api_key
DB_PASSWORD=vault://secret/data/db#password
LEGACY_TOKEN=vault://secret/app#token
```

KV v2 handling:

- For `vault://kv/data/app#api_key`, request path is `kv/data/app`.
- The actual user fields are under response `data.data`.
- Metadata is under response `data.metadata`.

KV v1 handling:

- For `vault://secret/app#token`, request path is `secret/app`.
- User fields are under response `data`.

Error behavior:

- Missing token or AppRole inputs: `AuthenticationFailed`.
- Permission denied: `PermissionDenied`.
- Missing path: `NotFound`.
- Missing field fragment: `InvalidSecretPayload`.

## 9. Config System

### 9.1 Config Discovery

Default discovery order:

1. `--config <path>`
2. `DOTENV_CLOUD_CONFIG`
3. `dotenv-cloud.toml` in current directory
4. Search parent directories until filesystem root
5. No config, using defaults

TOML is the primary format. YAML may be supported with `dotenv-cloud.yaml` or `dotenv-cloud.yml`, but TOML must be implemented first.

If both TOML and YAML exist in the same directory and no `--config` is provided, TOML wins and a warning is emitted in verbose mode.

### 9.2 Example Config

```toml
default_environment = "dev"

[environment.dev]
env_file = ".env"
env_local_file = ".env.local"

[environment.dev.defaults]
LOG_LEVEL = "info"
PORT = "3000"

[environment.staging]
env_file = ".env.staging"
env_local_file = ".env.staging.local"

[environment.prod]
env_file = ".env.production"
env_local_file = ".env.production.local"

[precedence]
order = ["cli", "system", "remote", "env.local", "env", "defaults"]

[providers.aws]
region = "us-east-1"
profile = "dev"          # AWS named credentials profile (unrelated to environments)
timeout_ms = 2000

[providers.aws.ssm]
with_decryption = true

[providers.vault]
address = "https://vault.example.com"
auth_method = "token"
token_env = "VAULT_TOKEN"
timeout_ms = 2000

[providers.vault.kv]
default_version = "v2"
```

Default values live under each environment as `[environment.<name>.defaults]`.
They are the lowest precedence source AND the fallback used when a remote
reference for the same key fails to resolve (see §12).

### 9.3 Environments

Environments select environment-specific configuration (this concept was
previously called "profiles"; not to be confused with the AWS named credentials
`profile` under `[providers.aws]`):

```sh
dotenv-cloud --environment dev run -- npm start
dotenv-cloud --environment prod validate
```

Environment resolution order:

1. `--environment <name>`
2. `DOTENV_CLOUD_ENVIRONMENT`
3. `default_environment`
4. `dev`

## 10. CLI Design

### 10.1 Global Flags

| Flag | Description |
| --- | --- |
| `--config <path>` | Use an explicit config file. |
| `--environment <name>` | Select a named environment. |
| `--env-file <path>` | Override environment `.env` path. |
| `--env-local-file <path>` | Override environment `.env.local` path. |
| `--no-env-file` | Do not load `.env`. |
| `--no-env-local` | Do not load `.env.local`. |
| `--set KEY=VALUE` | Add or override an environment value at CLI precedence. May be repeated. |
| `--strict` | Treat warnings as errors. |
| `--timeout <duration>` | Override provider request timeout. Example: `2s`, `500ms`. |
| `--no-color` | Disable colored output. |
| `--verbose` | Emit diagnostic metadata without secrets. |
| `--quiet` | Suppress non-error output. |

Global flags must appear before the subcommand unless the parser supports stable global flag parsing after subcommands.

### 10.2 Command Reference

| Command | Purpose | Example |
| --- | --- | --- |
| `dotenv-cloud init` | Scan config and dotenv files, install required provider plugins, and create/update the lockfile. | `dotenv-cloud init` |
| `dotenv-cloud run -- <cmd>` | Resolve environment and execute a child process. | `dotenv-cloud run -- npm start` |
| `dotenv-cloud export` | Print shell-compatible environment assignments. | `eval "$(dotenv-cloud export)"` |
| `dotenv-cloud build` | Materialize resolved environment to a dotenv-format file or stdout. | `dotenv-cloud build --output .env.resolved` |
| `dotenv-cloud resolve <KEY>` | Resolve and print one key. Secret output is gated. | `dotenv-cloud resolve DB_PASSWORD --show` |
| `dotenv-cloud validate` | Parse config, check references, and optionally contact providers. | `dotenv-cloud validate --providers` |
| `dotenv-cloud doctor` | Diagnose local setup, credentials, config, and provider connectivity. | `dotenv-cloud doctor` |
| `dotenv-cloud providers` | Manage externally installed provider plugins. | `dotenv-cloud providers list` |

### 10.3 `init`

Syntax:

```sh
dotenv-cloud [global flags] init [init flags]
```

Purpose:

Initialize provider dependencies for the current project or user. Behavior depends on whether a config file already exists:

- **No `dotenv-cloud.toml`, interactive terminal:** runs an interactive setup. The user picks providers from the registry; each is installed; the core then sends a `describe` request to each installed plugin (§7.4) and prompts for the provider's configurable settings (e.g. AWS `region`, `ssm.with_decryption`). It writes a fresh `dotenv-cloud.toml` (`default_environment`, `[environment.<name>]`, `[providers.*]`) and `dotenv-cloud.lock`.
- **Config present:** scans configured dotenv files and `dotenv-cloud.toml`, detects required URI schemes, and installs any missing provider plugins automatically (no `--yes` confirmation), verifies integrity, and writes `dotenv-cloud.lock`.
- **Non-interactive (no TTY) or `--yes` with no config:** skips the interactive flow and uses the config/dotenv-driven path above (e.g. to regenerate the lockfile from installed providers).

Flags:

| Flag | Description |
| --- | --- |
| `--yes` | Skip the interactive setup; use the non-interactive config/dotenv-driven path. |
| `--project` | Install providers into `.dotenv-cloud/providers`. Default when a project config exists. |
| `--user` | Install providers into the user provider directory. |
| `--registry <url>` | Use a custom provider registry. |
| `--lockfile <path>` | Override lockfile path. Defaults to `dotenv-cloud.lock`. |
| `--upgrade` | Upgrade providers to the latest allowed versions. |
| `--offline` | Do not fetch providers; verify installed providers against lockfile only. |

Examples:

```sh
dotenv-cloud init
dotenv-cloud init --yes
dotenv-cloud init --registry https://providers.example.com/index.json
```

`init` must not resolve or print secret values. It only inspects URI schemes and provider configuration.

### 10.4 `run`

Syntax:

```sh
dotenv-cloud [global flags] run [run flags] -- <command> [args...]
```

Flags:

| Flag | Description |
| --- | --- |
| `--clear-env` | Start child with only resolved dotenv-cloud environment, not inherited system env. |
| `--preserve PATH,HOME` | Preserve listed system variables when `--clear-env` is used. |
| `--require KEY` | Require key to be present after resolution. May be repeated. |
| `--dry-run` | Resolve and print redacted summary without executing child. |
| `--redact-summary` | Print source summary with all secret-like values redacted. Default in verbose mode. |
| `--install-missing-providers` | Install missing provider plugins before resolution. Requires network and signature verification. |

Separator semantics:

- `--` marks the end of `dotenv-cloud` arguments and the beginning of the child command.
- Everything after `--` is passed as the child command argv without reinterpretation.
- If `--` is missing and the parser cannot unambiguously identify the command, `dotenv-cloud` must return usage error code `2`.

Example:

```sh
dotenv-cloud --environment dev run -- npm start
```

### 10.5 `export`

Syntax:

```sh
dotenv-cloud [global flags] export [export flags]
```

Flags:

| Flag | Description |
| --- | --- |
| `--shell <shell>` | One of `bash`, `zsh`, `fish`, `powershell`. |
| `--format <format>` | Alias for shell or output format. |
| `--include KEY` | Export only listed key. May be repeated. |
| `--exclude KEY` | Exclude listed key. May be repeated. |
| `--no-comments` | Omit generated comments. |

Usage:

```sh
eval "$(dotenv-cloud export --shell bash)"
source <(dotenv-cloud export --shell zsh)
dotenv-cloud export --shell fish | source
dotenv-cloud export --shell powershell | Invoke-Expression
```

Output examples:

Bash/zsh:

```sh
export DB_PASSWORD='redacted-value-is-not-shown-here'
```

Fish:

```fish
set -gx DB_PASSWORD 'value';
```

PowerShell:

```powershell
$Env:DB_PASSWORD = 'value'
```

The export command intentionally prints secrets because shell export requires it. The command must warn on stderr unless `--quiet` is used:

```text
warning: export prints resolved secret values to stdout
```

### 10.6 `build`

Syntax:

```sh
dotenv-cloud build [flags]
```

Purpose:

Materialize the resolved environment as dotenv output. This is useful for build systems that require a file, but it carries higher leakage risk.

Flags:

| Flag | Description |
| --- | --- |
| `--output <path>` | Write output to file. Defaults to stdout. |
| `--force` | Overwrite existing output file. |
| `--mode <dotenv|json>` | Output format. Defaults to `dotenv`. |
| `--chmod <mode>` | File permissions for new output file. Defaults to `0600` on Unix. |
| `--include KEY` | Include only listed key. |
| `--exclude KEY` | Exclude listed key. |

Security behavior:

- Refuse to overwrite existing files unless `--force` is provided.
- On Unix, create files with mode `0600` by default.
- Warn that resolved secrets are being materialized.

### 10.7 `resolve`

Syntax:

```sh
dotenv-cloud resolve <KEY> [flags]
```

Flags:

| Flag | Description |
| --- | --- |
| `--show` | Print the resolved value. Required for secret-like values. |
| `--json` | Print metadata as JSON. Secret values are redacted unless `--show` is present. |
| `--source` | Show winning source and shadowed source metadata. |

Default output is redacted:

```text
DB_PASSWORD=[redacted] source=remote provider=aws-secrets
```

### 10.8 `validate`

Syntax:

```sh
dotenv-cloud validate [flags]
```

Flags:

| Flag | Description |
| --- | --- |
| `--providers` | Contact remote providers to validate access. |
| `--no-providers` | Parse and validate locally only. |
| `--all-environments` | Validate every configured environment. |
| `--json` | Emit machine-readable diagnostics. |

Validation checks:

- Config file syntax.
- Environment references.
- Precedence order.
- Dotenv syntax.
- Unknown URI schemes.
- Invalid URI structure.
- Provider config completeness.
- Installed provider manifest integrity.
- Lockfile consistency.
- Optional provider connectivity and permissions.

### 10.9 `doctor`

Syntax:

```sh
dotenv-cloud doctor [flags]
```

Purpose:

Human-readable diagnosis of runtime readiness.

Checks:

- Binary version and platform.
- Config discovery result.
- Active environment.
- Dotenv files found.
- Provider plugin directory paths.
- Installed provider manifests.
- Lockfile consistency.
- AWS region/profile detectability.
- Vault address and authentication inputs.
- Network timeout configuration.
- Shell export compatibility hints.

Secrets must never be printed.

### 10.10 `providers`

Syntax:

```sh
dotenv-cloud providers <subcommand> [flags]
```

Subcommands:

| Subcommand | Purpose | Example |
| --- | --- | --- |
| `list` | List installed providers and configured schemes. | `dotenv-cloud providers list` |
| `search <query>` | Search configured provider registries. | `dotenv-cloud providers search aws` |
| `install <name>` | Download, verify, and install a provider plugin. | `dotenv-cloud providers install aws` |
| `update [name]` | Update one or all installed providers. | `dotenv-cloud providers update aws` |
| `remove <name>` | Remove an installed provider plugin. | `dotenv-cloud providers remove aws` |
| `info <name>` | Show provider metadata, schemes, version, and integrity info. | `dotenv-cloud providers info aws` |

List output:

```text
aws    installed   version=1.0.0   schemes=aws-secrets,aws-ssm   configured=yes
vault  missing     required=yes     schemes=vault            configured=yes
```

Flags:

| Flag | Description |
| --- | --- |
| `--json` | Emit provider registry and config status as JSON. |
| `--registry <url>` | Use a custom provider registry for search/install/update. |
| `--project` | Operate on project-local providers. |
| `--user` | Operate on user-level providers. |
| `--yes` | Accept install/update prompts. |

## 11. Execution Model

### 11.1 Subprocess Injection

`dotenv-cloud run -- <command>` resolves the final environment map, then starts the target command as a child process using OS-native process spawning APIs.

The child process receives:

- The merged environment.
- The current working directory unless overridden in a future version.
- Inherited stdin, stdout, and stderr by default.
- The same argv after the `--` separator.

### 11.2 Parent Shell Limitation

A child process cannot modify the environment of its parent process on standard operating systems. Environment variables are copied from parent to child at process creation. Any modifications made inside the child affect only that child and its descendants.

Therefore, `dotenv-cloud` cannot directly set variables in the invoking shell. The subprocess model is the correct default because it is portable, explicit, and avoids persistent shell contamination.

### 11.3 Why Subprocess Execution Is Preferred

- Works consistently across bash, zsh, fish, PowerShell, CI runners, and service wrappers.
- Keeps secrets scoped to one command invocation.
- Avoids accidental long-lived shell state.
- Preserves normal process exit codes.
- Does not require shell-specific hooks for the primary workflow.

### 11.4 Exit Code Behavior

Exit codes:

| Code | Meaning |
| --- | --- |
| `0` | Success. For `run`, child exited successfully. |
| `1` | Runtime error or validation failure. |
| `2` | CLI usage error. |
| `3` | Configuration error. |
| `4` | Dotenv parse error. |
| `5` | Secret resolution error. |
| `6` | Provider authentication or permission error. |
| `7` | Provider timeout or network error. |
| Child code | `run` returns the child process exit code after successful launch. |

If the child process is terminated by signal on Unix, `dotenv-cloud` should mirror conventional shell behavior when possible, returning `128 + signal`.

## 12. Data Flow

For `dotenv-cloud run -- npm start`:

1. Parse global flags and the `run` command.
2. Locate and parse `dotenv-cloud.toml`.
3. Select the active environment.
4. Load default config values.
5. Parse `.env`.
6. Parse `.env.local`.
7. Read system environment.
8. Parse CLI `--set KEY=VALUE` overrides.
9. Build a source map for every key.
10. Apply configured precedence.
11. Scan winning values and lower-precedence remote candidates as needed.
12. Load installed provider manifests for required URI schemes.
13. Launch provider plugin processes for required schemes.
14. Resolve remote URI values unless shadowed by higher-precedence values.
15. Cache resolved values in memory for the current execution.
16. Construct final environment map.
17. Validate required keys and strict rules.
18. Spawn child process with final environment.
19. Return the child process exit status.

## 13. Security Model

### 13.1 Secret Handling Principles

- Never log resolved secret values.
- Never include secrets in panic messages, debug output, structured diagnostics, or telemetry.
- Never persist resolved secrets in V1.
- Keep secret values in memory only for the current process lifetime.
- Prefer redacting wrapper types for secret-bearing strings.
- Avoid unnecessary cloning of secret values.
- Zeroize where practical, recognizing that language runtimes, OS process environments, and provider SDKs may copy memory internally.

### 13.2 Provider Plugin Supply Chain

Provider plugins execute with the user's local permissions and can access provider credentials through native SDK chains or environment variables. Plugin installation must therefore be treated as a privileged trust decision.

Required controls:

- Official provider downloads must be served over HTTPS.
- Provider archives must include checksums.
- Official provider archives must be signed.
- The core binary must verify checksum and signature before installation.
- `dotenv-cloud.lock` must pin provider name, version, source, schemes, and checksum.
- `run`, `export`, `build`, and `resolve` must verify installed provider manifests against the lockfile when a lockfile exists.
- Unsigned local provider plugins are allowed only with an explicit development configuration or unsafe flag.
- Provider stderr must be treated as untrusted diagnostic text and must pass through redaction before display.
- Provider stdout must be parsed only as protocol JSON; non-protocol output is an error.

Provider plugin installation is allowed in V1. Persistent storage of resolved secret values is not.

### 13.3 Redaction Policy

Values are redacted when:

- Key names match sensitive patterns: `SECRET`, `TOKEN`, `PASSWORD`, `PASS`, `PRIVATE_KEY`, `API_KEY`, `ACCESS_KEY`, `SESSION`.
- The source is a remote provider.
- The user marks a key as sensitive in config.

Redaction format:

```text
[redacted]
```

Partial redaction is allowed for non-secret diagnostics only:

```text
abcd...wxyz
```

Remote URI references should be redacted conservatively:

```text
aws-secrets://prod/db/password -> aws-secrets://prod/db/[redacted]
vault://kv/data/app#api_key -> vault://kv/data/app#[redacted]
```

### 13.4 Logging

Default logging:

- Errors and warnings only.
- No resolved values.
- No provider credentials.
- No request headers.

Verbose logging:

- May include config path, environment, provider names, source precedence, cache hit status, and redacted URI references.
- Must not include secrets.

### 13.5 Telemetry

Telemetry is disabled by default. V1 should not include telemetry unless explicitly compiled in and enabled by the user. If added later:

- Must be opt-in.
- Must not include environment variable keys or values by default.
- Must document all collected fields.

### 13.6 Subprocess Safety

`dotenv-cloud` must:

- Execute commands using argv-based process APIs, not shell string evaluation.
- Avoid invoking `/bin/sh -c` unless the user explicitly asks for shell mode in a future feature.
- Preserve child stdin/stdout/stderr by default.
- Avoid printing the final environment before execution.
- Ensure dry-run output is redacted.

### 13.7 Export and Build Risks

`export` and `build` intentionally emit resolved secret values. They must:

- Warn by default.
- Support `--quiet` for automation.
- Support include/exclude filtering.
- Use safe shell quoting.
- Refuse unsafe file overwrite behavior unless `--force` is supplied.

## 14. Performance Requirements

Targets:

| Metric | Target |
| --- | --- |
| Startup time without remote calls | Ideal `<50ms`, acceptable `<200ms`. |
| Startup time with remote calls | Dominated by provider latency; local overhead should remain `<200ms`. |
| Memory footprint | Minimal; target `<20MB` resident memory for typical use. |
| Background processes | None. |
| Persistent cache | None in V1. |

Implementation requirements:

- Parse config and dotenv files synchronously and cheaply.
- Resolve independent remote references concurrently with a configurable limit.
- Default provider timeout should be short, such as `2s`, and configurable.
- Cache duplicate references in memory.
- Do not launch provider plugins unless a reference or validation command needs them.
- Keep provider plugin process startup lazy and bounded by timeout.

## 15. Implementation Language Recommendation

### 15.1 Rust Preferred

Rust is the preferred implementation language.

Rationale:

- Produces fast, small CLI tools with predictable startup behavior.
- Strong memory safety guarantees without a garbage collector.
- Good ergonomics for structured error handling.
- Mature CLI ecosystem, including argument parsers, config parsers, dotenv parsers, URI parsers, async runtimes, and redaction/secrecy types.
- The core binary can avoid cloud SDK dependencies entirely by delegating provider-specific logic to external plugins.
- Rust is well suited for implementing a robust provider plugin host, process supervision, timeout handling, and redaction boundaries.
- Cross-compilation and Homebrew/cargo distribution are practical.

Recommended Rust dependencies:

| Capability | Candidate |
| --- | --- |
| CLI parsing | `clap` |
| Async runtime | `tokio` |
| Config parsing | `serde`, `toml`, optional `serde_yaml` |
| URI parsing | `url` |
| Dotenv parsing | dedicated parser or compatibility-tested internal parser |
| Plugin protocol | `serde_json` |
| Plugin process host | `tokio::process` |
| Secret redaction | `secrecy` |
| Error handling | `thiserror`, `anyhow` for binary boundary |
| Shell quoting | shell-specific quoting crate or audited internal implementation |

Provider plugins may use their own implementation language and SDK choices. Official providers should use the most mature provider SDK for their target platform while keeping plugin binary size reasonable.

### 15.2 Go Alternative

Go is a viable alternative.

Strengths:

- Excellent small-binary distribution story for the core CLI.
- Mature AWS and Vault SDKs.
- Simple concurrency model.
- Strong CLI ecosystem.
- Operational familiarity in infrastructure teams.

Tradeoffs:

- Garbage collector can increase memory footprint and make secret lifetime harder to reason about.
- Startup time is generally good but binary size can be larger depending on linked SDKs.
- Rust offers stronger compile-time guarantees around ownership and secret wrapper usage.

### 15.3 Decision

Use Rust for the V1 core binary. Implement all cloud and vault integrations as external provider plugins. The core binary must contain the plugin host and protocol only, not provider SDKs.

## 16. Dotenv Compatibility Requirements

The parser must support:

- `KEY=value`
- `KEY=value with spaces`
- `KEY="quoted value"`
- `KEY='single quoted value'`
- `KEY=escaped\nvalue`
- `export KEY=value`
- Blank lines
- Full-line comments
- Inline comments where compatible with established dotenv behavior

Malformed input must produce precise file, line, and column diagnostics when possible.

Example:

```dotenv
# Local values
REGION=us-east-1
PORT=3000

# Remote references
DB_PASSWORD=aws-secrets://prod/db/password
API_KEY=vault://kv/data/app#api_key
```

## 17. Conflict Handling

When a key appears in multiple sources:

1. Select the highest-precedence value.
2. Record shadowed values as metadata.
3. In verbose mode, print a redacted conflict summary.

Example verbose output:

```text
DB_PASSWORD source=system shadowed=remote,.env
PORT source=.env.local shadowed=.env,defaults
```

Secrets must remain redacted.

Strict mode may fail on conflicts if configured:

```toml
[conflicts]
error_on_shadow = ["DB_PASSWORD", "API_KEY"]
warn_on_any_shadow = true
```

## 18. Edge Cases and Required Behavior

### 18.1 Missing Secrets

Default behavior:

- Fail the command before launching the child process.
- Print key name, provider, and redacted reference.
- Exit with code `5` for resolution errors or `6` for auth/permission errors.

Optional behavior:

```toml
[resolution]
missing = "error" # error | warn | ignore
```

Ignoring missing secrets is not recommended and must warn unless `--quiet` is set.

**Default-value fallback.** When a remote reference fails to resolve — for any
reason (not found, authentication, network, timeout) — the core first checks the
active environment's `[environment.<name>.defaults]` for the same key. If a
default exists, it is used (recorded at the `defaults` source) and a warning is
emitted noting the fallback. Only when no default exists does the `missing`
policy above apply. This lets a project keep working against a local default
value when a backend is unavailable, while still surfacing the failure.

### 18.2 Partial Resolution Failures

If any required winning value fails to resolve, `run` must fail before spawning the child process.

For `export` and `build`, default behavior is also fail-closed. Future versions may support partial output with an explicit flag, but V1 should avoid partial secret materialization.

### 18.3 Circular References

Remote URI references must not recursively resolve unless a future interpolation feature explicitly supports it.

If local interpolation is supported:

```dotenv
A=${B}
B=${A}
```

the resolver must detect cycles and return a validation error:

```text
circular reference detected: A -> B -> A
```

If a remote secret returns a value that itself looks like `aws-secrets://...`, it is treated as a literal value by default. Recursive remote resolution is disabled in V1.

### 18.4 Invalid URI Formats

Invalid URI examples:

```dotenv
BAD=aws-secrets://
BAD=vault://#key
BAD=aws-ssm://
```

Behavior:

- `validate` reports all invalid references.
- `run` fails when an invalid winning value is needed.
- `--strict` fails on any invalid reference even if shadowed.

### 18.5 Malformed `.env` Files

Malformed dotenv syntax fails fast with file and line information:

```text
.env:12: invalid assignment: expected KEY=VALUE
```

`run` must not spawn the child process when dotenv parsing fails.

### 18.6 Conflicting Keys Across Sources

Default behavior is deterministic override according to precedence. Verbose mode reports source selection. Strict conflict policies are configurable.

### 18.7 Provider Timeouts

Default provider timeout:

```text
2s
```

Behavior:

- Timeout resolving a required winning value fails the command.
- Error class is `Timeout`.
- Exit code is `7`.
- Error output includes provider, key, timeout duration, and redacted reference.

### 18.8 Provider Rate Limits

Behavior:

- Do not retry indefinitely.
- Use provider SDK retry defaults only if bounded.
- Return `RateLimited` after retry exhaustion.
- Keep total command runtime bounded by timeout and retry configuration.

### 18.9 Binary Secret Values

Binary provider values are rejected by default unless explicitly encoded:

```dotenv
CERT=aws-secrets://prod/cert?binary=base64
```

V1 should support base64 output only.

### 18.10 Non-UTF-8 Values

Environment variables are not uniformly capable of carrying arbitrary bytes across platforms. V1 requires resolved values to be valid UTF-8 strings. Non-UTF-8 values return `InvalidSecretPayload`.

## 19. Configuration Schema

Indicative TOML schema:

```toml
default_environment = "dev"

[environment.<name>]
env_file = ".env"
env_local_file = ".env.local"

[environment.<name>.defaults]
KEY = "VALUE"

[precedence]
order = ["cli", "system", "remote", "env.local", "env", "defaults"]

[providers.aws]
region = "us-east-1"
profile = "dev"          # AWS named credentials profile
timeout_ms = 2000

[providers.aws.ssm]
with_decryption = true

[providers.vault]
address = "https://vault.example.com"
namespace = "optional"
auth_method = "token"
token_env = "VAULT_TOKEN"
timeout_ms = 2000

[providers.vault.kv]
default_version = "v2"

[provider_registry]
url = "https://providers.dotenv-cloud.dev/index.json"
allow_unsigned = false
install_scope = "project"

[resolution]
missing = "error"
concurrency = 8

[conflicts]
warn_on_any_shadow = false
error_on_shadow = []

[sensitive]
keys = ["DB_PASSWORD", "API_KEY"]
patterns = ["*_TOKEN", "*_SECRET"]
```

## 20. Output and Diagnostics

### 20.1 Human Output

Errors should be concise and actionable:

```text
error: failed to resolve DB_PASSWORD
provider: aws-secrets
reference: aws-secrets://prod/db/[redacted]
reason: secret not found
```

### 20.2 JSON Output

Commands with `--json` should produce stable machine-readable output:

```json
{
  "status": "error",
  "errors": [
    {
      "key": "DB_PASSWORD",
      "provider": "aws-secrets",
      "reference": "aws-secrets://prod/db/[redacted]",
      "class": "NotFound",
      "message": "secret not found"
    }
  ]
}
```

Secret values must be redacted unless a command explicitly opts into showing them.

## 21. Testing Strategy

### 21.1 Unit Tests

- Dotenv parser compatibility.
- URI parser behavior.
- Precedence merge behavior.
- Redaction functions.
- Shell export quoting.
- Config loading and environment selection.
- Error classification.

### 21.2 Integration Tests

- `run` preserves child exit code.
- `run` passes environment to child process.
- `export` output can be evaluated by bash, zsh, fish, and PowerShell.
- `build` writes files with safe permissions.
- Core plugin host launches provider processes, performs handshakes, and parses protocol responses.
- Provider installation verifies checksum, signature, manifest, and lockfile behavior.
- Provider resolution works through official provider plugins against mocked AWS and Vault APIs.

### 21.3 Provider Contract Tests

Use local/mocked services:

- AWS provider plugin with SDK mocked clients or local HTTP-compatible test layer.
- Vault provider plugin with Vault dev server or mock HTTP server for KV v1/v2.
- Fake provider plugin for protocol compatibility, malformed output, timeout, and stderr redaction tests.

Tests must assert no secret values appear in logs, errors, or debug output.

## 22. Packaging and Distribution

Distribution targets:

- Homebrew tap.
- Cargo install.
- GitHub releases with signed checksums.
- Linux packages in later versions.

Core binary requirements:

- Small single executable for the core CLI.
- Static linking where feasible.
- No runtime dependency on a local daemon.
- No required config directory.
- No cloud provider SDKs linked into the core binary.

Provider distribution requirements:

- Providers are distributed as separate signed archives.
- Providers may be installed project-locally or user-locally.
- Official provider archive names should be stable, for example `dotenv-cloud-provider-aws`.
- Provider installation must update `dotenv-cloud.lock` when operating in project mode.

Suggested binary names:

```text
dotenv-cloud
```

No alternate product or binary name should be used.

## 23. Example Workflows

### 23.1 Local Development

`.env`:

```dotenv
REGION=us-east-1
DB_PASSWORD=aws-secrets://dev/db/password
API_KEY=vault://kv/data/myapp#api_key
```

Run:

```sh
dotenv-cloud init
dotenv-cloud run -- npm start
```

### 23.2 Resolve One Key

```sh
dotenv-cloud resolve DB_PASSWORD
```

Output:

```text
DB_PASSWORD=[redacted] source=remote provider=aws-secrets
```

Show value explicitly:

```sh
dotenv-cloud resolve DB_PASSWORD --show
```

### 23.3 Export Into Current Shell

```sh
eval "$(dotenv-cloud export --shell bash)"
```

### 23.4 Validate CI Configuration

```sh
dotenv-cloud --environment prod validate --providers --strict
```

### 23.5 Materialize Build Environment

```sh
dotenv-cloud --environment staging build --output .env.resolved --force
```

## 24. V1 Acceptance Criteria

V1 is complete when:

- `dotenv-cloud run -- <cmd>` works across Linux, macOS, and Windows.
- `.env` and `.env.local` parsing is compatibility-tested.
- Deterministic precedence is implemented and configurable.
- `dotenv-cloud init` detects required provider schemes and installs signed provider plugins.
- `dotenv-cloud providers install/list/update/remove/info/search` works.
- AWS Secrets Manager resolution works through the external AWS provider plugin.
- AWS SSM Parameter Store resolution works through the external AWS provider plugin.
- Vault KV v1/v2 resolution works through the external Vault provider plugin.
- Provider errors are classified and redacted.
- Provider manifests and lockfiles are verified before provider execution.
- Resolved secrets are cached only in memory.
- `export`, `build`, `resolve`, `validate`, `doctor`, `init`, and `providers` commands exist.
- Shell export supports bash, zsh, fish, and PowerShell.
- No command logs secrets unless the user explicitly chooses a secret-emitting mode.
- The core binary can be distributed as a small single executable without bundled provider SDKs.
