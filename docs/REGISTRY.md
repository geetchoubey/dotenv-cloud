# Provider Registry

`dotenv-cloud` installs provider plugins from a **registry index** — a single
JSON document listing providers, versions, and per-target archives. The index
URL comes from `[provider_registry].url` in config, the `--registry` flag, or a
built-in default. `https://`, `file://`, and local paths are all accepted.

## Commands that use it

```sh
dotenv-cloud providers search aws
dotenv-cloud providers install aws            # latest; aws@0.1.0 to pin
dotenv-cloud providers update [aws]           # one or all installed
dotenv-cloud init --yes                       # install providers for detected schemes
dotenv-cloud run --install-missing-providers -- <cmd>
```

Install flow: fetch index → select version (latest by semver, or pinned) →
select the current build target → download archive → **verify sha256** →
**verify ed25519 signature** (if present) → unpack → place `manifest.toml` +
executable under the provider dir → upsert `dotenv-cloud.lock`.

## Index format

```json
{
  "schema_version": 1,
  "providers": {
    "aws": {
      "package": "dotenv-cloud-provider-aws",
      "description": "AWS Secrets Manager and SSM Parameter Store",
      "schemes": ["aws-sm", "aws-ssm"],
      "versions": {
        "0.1.0-beta.1": {
          "targets": {
            "aarch64-apple-darwin": {
              "url": "https://.../dotenv-cloud-provider-aws-v0.1.0-beta.1-aarch64-apple-darwin.tar.gz",
              "sha256": "<hex>",
              "signature": "<base64 ed25519, optional>"
            },
            "x86_64-unknown-linux-gnu": { "url": "...", "sha256": "..." }
          }
        }
      }
    }
  }
}
```

- The top-level key (`aws`) is the short name used on the CLI; `package` is the
  installed manifest/crate name.
- `targets` keys are Rust target triples; the installer picks the one matching
  the running binary.
- Archives are `.tar.gz` containing a single top-level directory with the
  provider executable and its `manifest.toml`.

## Integrity & signing

- **sha256 is mandatory** — a mismatch aborts the install.
- **Signatures are ed25519** over the raw archive bytes, base64-encoded in the
  index `signature` field. Configure the trusted key to enable verification:

  ```toml
  [provider_registry]
  url = "https://providers.dotenv-cloud.dev/index.json"
  public_key = "<base64 ed25519 public key>"
  allow_unsigned = false
  ```

- If an archive has **no signature** (or is signed but no `public_key` is
  configured), the install is **refused** unless `allow_unsigned = true` or
  `--allow-unsigned` is passed.

## Install scope

- `--project` (default): `./.dotenv-cloud/providers/<name>/`
- `--user`: the platform user provider directory (see `dotenv-cloud doctor`).
