---
name: dotenv-cloud
description: >-
  Onboard a project onto dotenv-cloud and use it day to day — the CLI that
  extends dotenv so .env values can be references to secrets in AWS Secrets
  Manager (aws-secrets://), AWS SSM Parameter Store (aws-ssm://), or HashiCorp
  Vault (vault://), resolved at run time. Use this whenever the user wants to
  install, bootstrap, or configure dotenv-cloud (dotenv-cloud.toml), wire up a
  provider, or run/export/build their environment; whenever they talk about
  keeping secrets out of .env while still using .env; or when they hit problems
  with `providers install`, schemes, precedence, redaction, or secrets not
  resolving. Prefer this skill even if the user doesn't say "dotenv-cloud" by
  name but describes resolving remote secrets through a .env-style workflow.
---

# Using dotenv-cloud

dotenv-cloud lets a team keep its existing dotenv workflow — the app still reads
plain environment variables and you still keep a `.env` file — but any value can
be a **reference** to a secret in a remote vault instead of a hard-coded secret.
At run time the CLI resolves those references through provider plugins, merges
everything by a clear precedence, and injects the result into the process. The
real secrets stay in the vault; only references live in the repo.

Your job with this skill is to get a developer from zero to a working setup, and
to steer them around the sharp edges (collected in **Pitfalls** — read that
section, it prevents most support questions).

## 1. Install the CLI

| Platform | Command |
|---|---|
| macOS / Linux (Homebrew) | `brew install geetchoubey/tap/dotenv-cloud` |
| macOS / Linux (script) | `curl -fsSL https://geetchoubey.github.io/dotenv-cloud/install.sh \| sh` |
| Windows (PowerShell) | `irm https://geetchoubey.github.io/dotenv-cloud/install.ps1 \| iex` |

The scripts auto-detect the platform, verify the download's SHA-256, and (on the
script path) wire up shell tab-completion. Verify with `dotenv-cloud --version`.

To pin a version: `DOTENV_CLOUD_VERSION=v0.1.1` before the script, or
`brew install …@<version>` is not supported — use the script for pinning.

## 2. Create the config: `dotenv-cloud.toml`

Put this at the project root (it's discovered by walking up from the cwd). It is
**shared project configuration with no secrets in it**, so commit it.

```toml
default_environment = "dev"

[environment.dev]
env_file = ".env"
env_local_file = ".env.local"

[environment.prod]
env_file = ".env.production"

# Per-environment fallbacks. Lowest precedence for ABSENT keys, AND the value
# used when a remote reference for the same key fails to resolve.
[environment.dev.defaults]
LOG_LEVEL = "info"

[providers.aws]
region = "us-east-1"             # shared; fine to commit
# profile = "..."                # DON'T commit — see Pitfalls. Use AWS_PROFILE.

[providers.vault]
address = "https://vault.example.com"
[providers.vault.kv]
default_version = "v2"           # "v1" or "v2"

[provider_registry]
url = "https://geetchoubey.github.io/dotenv-cloud/index.json"
```

The full schema (every key + default) is in the docs site under
**Reference → Config TOML**
(https://geetchoubey.github.io/dotenv-cloud/#ref-config). Unknown keys are
rejected, so typos surface in `validate`.

### What to commit vs ignore

| File | Commit? | Why |
|---|---|---|
| `dotenv-cloud.toml` | ✅ | Shared config, no secrets. |
| `.env` (with references) | ✅ | `KEY=aws-secrets://…` is a reference, not a secret. |
| `dotenv-cloud.lock` | ✅ | Pins provider versions + sha256 (like a lockfile). |
| `.env.local` | 🚫 gitignore | Per-developer overrides and any literal local secrets. |
| `build`/`export` output | 🚫 gitignore | These contain **resolved real secret values**. |

## 3. Bootstrap providers

```sh
dotenv-cloud init                # see init behavior below
dotenv-cloud providers list      # what's installed
dotenv-cloud validate --providers  # checks config, references, and provider connectivity (no secrets printed)
```

`init` behaves differently depending on whether a config already exists:

- **No `dotenv-cloud.toml` (and a TTY):** runs an interactive setup — pick
  providers, installs them, asks each provider what to configure (e.g. AWS
  `region`, `ssm.with_decryption`), then writes a fresh `dotenv-cloud.toml` and
  lockfile.
- **Config present:** detects the schemes referenced in `.env` and installs any
  missing providers automatically (no `--yes` confirmation needed), then writes
  the lockfile. Use `--offline` to skip installs.
- **Non-interactive / `--yes` with no config:** falls back to the config-driven
  path (e.g. regenerating the lockfile from installed providers) instead of
  prompting.

`providers install` downloads a signed plugin, verifies its SHA-256 **and**
ed25519 signature against the CLI's built-in trusted key, then unpacks it.

## 4. Write references in `.env`

The scheme selects the provider. See the per-provider sections below for exact
syntax — getting the slashes right matters.

```dotenv
APP_NAME=billing                                   # plain value, passed through
DB_PASSWORD=aws-secrets://prod/billing/db#password # AWS Secrets Manager, JSON field
API_TOKEN=aws-ssm:///prod/billing/api_token        # SSM (THREE slashes — see Pitfalls)
SESSION_KEY=vault://secret/billing#session_key     # Vault KV
```

## 5. Use it

```sh
dotenv-cloud run -- node server.js     # resolve + exec, child inherits the merged env
dotenv-cloud resolve DB_PASSWORD       # inspect one key (redacted unless --show)
dotenv-cloud export --shell zsh        # print shell `export` lines (project env only)
dotenv-cloud build --mode dotenv -o out.env   # materialize to a file (contains real secrets!)
dotenv-cloud doctor                    # diagnose setup, credentials, installed providers
```

`run` inherits the shell environment and overlays the project values;
`--clear-env`/`--preserve` start from a clean slate. `export`/`build` emit only
the **project** environment (`.env`, `.env.local`, remote,
`[environment.*.defaults]`, `--set`) — never the whole shell.

## Provider: AWS (`aws-secrets://`, `aws-ssm://`)

```dotenv
# Secrets Manager
DB_PASSWORD=aws-secrets://prod/db/password
API_KEY=aws-secrets://prod/app/config#api_key        # #field selects a JSON field
DB_PASSWORD=aws-secrets://prod/db/password?version_id=abc
DB_PASSWORD=aws-secrets://prod/db/password?version_stage=AWSCURRENT
CERT=aws-secrets://prod/cert?binary=base64           # binary secrets must opt in

# SSM Parameter Store — parameter names are ABSOLUTE paths => three slashes
API_TOKEN=aws-ssm:///prod/app/api_token
API_TOKEN=aws-ssm:///prod/app/api_token?with_decryption=true   # default true
```

**Auth:** the standard AWS default credential chain (env vars, shared config,
SSO, IAM roles). dotenv-cloud never stores credentials. `region`/`profile` may
be set under `[providers.aws]`, but see the profile pitfall below.

## Provider: HashiCorp Vault (`vault://`)

```dotenv
DB_PASSWORD=vault://secret/app#password       # KV v2: reads secret/data/app, field "password"
WHOLE=vault://secret/app                       # no #field -> the whole secret JSON object
PINNED=vault://secret/app?version=3#password   # KV v2 specific version
```

- **KV v2** (default): the provider inserts the `data/` segment automatically, so
  `vault://secret/app` and `vault://secret/data/app` are equivalent. Set
  `[providers.vault.kv].default_version`.
- **KV v1**: path is used verbatim; set `default_version = "v1"`.
- **Auth:** token only (V1). Token from `VAULT_TOKEN` (or `token_env`), address
  from `VAULT_ADDR`/`address`, optional `VAULT_NAMESPACE`. TLS via `VAULT_CACERT`
  / `VAULT_SKIP_VERIFY` (dev only).

## Pitfalls

These are the things that actually trip people up. Most "it doesn't work" reports
are one of these.

1. **SSM needs three slashes.** `aws-ssm:///prod/app/token` — the parameter name
   is an absolute path. `aws-ssm://prod/app/token` (two slashes) treats `prod`
   as the host/authority and resolves the wrong name.

2. **Don't commit `[providers.aws].profile`.** If it's set in the shared config,
   it *overrides* `AWS_PROFILE` in each developer's environment (the provider
   sets it explicitly). Omit it and let each developer use `AWS_PROFILE`; that's
   the idiomatic, no-edit-per-dev approach. Same logic for any machine-specific
   value.

3. **`export`/`build` output contains real secret values.** They resolve and
   materialize. Gitignore the output and treat it like a secret. (They do *not*
   dump your whole shell env — only the project env — but the project values are
   the actual secrets.)

4. **`[environment.*.defaults]` are both a precedence floor and a resolution
   fallback.** Precedence picks one winner per key *before* resolution: `remote`
   outranks `defaults`, so a `.env` reference wins and the default is shadowed
   for normal merging. But if that remote secret then **fails to resolve** (not
   found, auth, network, …), the key falls back to its environment default if one
   exists — with a warning. Only when there is *no* default does
   `resolution.missing = error`/`warn`/`ignore` apply. Defaults also fill keys
   that no higher source provides at all.

5. **Precedence defaults may surprise.** Default order is
   `cli > system > remote > env.local > env > defaults`. So a shell variable
   (`system`) overrides a `.env` value of the same name. Reorder via
   `[precedence].order`; `validate` warns about risky orderings.

6. **The AWS Secrets Manager scheme is `aws-secrets://`** (renamed from the old
   `aws-sm://`). An unknown scheme is treated as a plain literal and passed
   through verbatim — so a typo or a stale CLI silently emits the URI instead of
   resolving it. If a value comes out as `aws-…://…`, your CLI or scheme is wrong;
   run `dotenv-cloud --version` and `brew upgrade` / re-run the install script.

7. **Signature verification is on by default.** Installs verify the ed25519
   signature against the built-in key. `--allow-unsigned` is only for a
   custom/private registry or unsigned artifacts.

8. **`providers install` targets macOS/Linux.** The registry indexes `.tar.gz`
   builds; the Windows CLI works but automated provider install is not wired yet
   (download the provider archive manually if needed).

9. **After upgrading the CLI across a scheme/version change, reinstall
   providers.** An installed provider only handles the schemes its build knows;
   `dotenv-cloud providers install <name>` refreshes it (and `init` regenerates
   the lockfile with integrity).

10. **First-launch OS warnings are cosmetic.** macOS Gatekeeper may quarantine a
    manually downloaded binary (`xattr -d com.apple.quarantine <path>`; the
    install script does this for you, and Homebrew doesn't quarantine). Windows
    SmartScreen may warn that the publisher is unverified — the binaries are
    ed25519-signed for the registry but not Authenticode-signed.

## Quick reference

```sh
dotenv-cloud init --yes                 # set up providers + lockfile
dotenv-cloud validate --providers       # verify everything resolves
dotenv-cloud run -- <cmd>               # run with the resolved env
dotenv-cloud export --shell <shell>     # eval-able exports
dotenv-cloud build -o <file>            # materialize (secrets!) — gitignore it
dotenv-cloud resolve <KEY> [--show]     # inspect one key
dotenv-cloud providers install <name>   # aws | vault
dotenv-cloud doctor                     # diagnostics
```
