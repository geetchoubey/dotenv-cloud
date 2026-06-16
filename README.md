# dotenv-cloud

A CLI-only runtime tool that extends traditional dotenv behavior with secure
remote secret resolution. It loads local dotenv files, resolves remote secret
references through externally installed provider plugins, merges all values into
a deterministic runtime environment, and injects that environment into a child
process.

```sh
dotenv-cloud run -- npm start
dotenv-cloud run -- python app.py
dotenv-cloud run -- java -jar app.jar
```

`dotenv-cloud` is stateless and ephemeral during secret resolution. It runs no
daemon, never mutates the parent shell, and never persists resolved secrets.
Cloud/vault integrations are **external provider plugins** — the core binary
links no provider SDKs. See [`docs/TECHNICAL_SPEC.md`](docs/TECHNICAL_SPEC.md)
for the full specification.

## Status

This repository contains the **core CLI**. Provider plugins (AWS, Vault, …) live
in separate repositories and are installed into a provider directory.

Implemented in this build:

- dotenv parsing (quotes, escapes, `export`, comments) — spec §16
- secret-reference URI detection/parsing (`aws-secrets://`, `aws-ssm://`, `vault://`) — spec §6
- deterministic, configurable source precedence with remote promotion — spec §5
- redaction policy for keys, remote values, and URI references — spec §13.3
- provider plugin host (newline-delimited JSON over stdin/stdout) — spec §7.4
- per-execution in-memory secret cache — spec §6.5
- subprocess execution with exit-code propagation — spec §11
- shell export (`bash`, `zsh`, `fish`, `powershell`) with safe quoting — spec §10.5
- commands: `run`, `export`, `build`, `resolve`, `validate`, `doctor`, `init`, `providers`
- registry-backed provider installation: `providers install/search/update`, `init`,
  and `run --install-missing-providers` download an archive, verify its sha256
  (+ optional ed25519 signature), unpack it, and update `dotenv-cloud.lock`

See [`docs/REGISTRY.md`](docs/REGISTRY.md) for the registry index format and the
signing/integrity model.

## Commands

| Command | Purpose |
| --- | --- |
| `run -- <cmd>` | Resolve the environment and execute a child process. |
| `export` | Print shell-compatible `export` assignments. |
| `build` | Materialize the resolved environment to a file or stdout. |
| `resolve <KEY>` | Resolve and print one key (redacted unless `--show`). |
| `validate` | Parse config, check references, optionally contact providers. |
| `doctor` | Diagnose local setup, config, and provider connectivity. |
| `init` | Scan for required schemes and write the lockfile. |
| `providers` | `list` / `info` / `remove` installed provider plugins. |

Run `dotenv-cloud --help` or `dotenv-cloud <command> --help` for flags.

## Configuration

`dotenv-cloud.toml` is discovered by walking up from the current directory. See
[`examples/dotenv-cloud.toml`](examples/dotenv-cloud.toml).

## Writing a provider plugin

Providers are standalone executables that speak the JSON protocol in
[`docs/PROVIDER_PROTOCOL.md`](docs/PROVIDER_PROTOCOL.md). Install one by placing
its executable and a `manifest.toml` under `.dotenv-cloud/providers/<name>/`
(project-local) or the user provider directory.

## Building

```sh
cargo build --release   # produces target/release/dotenv-cloud
cargo test              # unit + integration tests
```

## License

MIT OR Apache-2.0.
