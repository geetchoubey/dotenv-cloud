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

The default registry is published at
`https://geetchoubey.github.io/dotenv-cloud/index.json`.

Install flow: fetch index → select version (latest by semver, or pinned) →
select the current build target → download archive → **verify sha256** →
**verify ed25519 signature against a trusted key** → unpack → place
`manifest.toml` + executable under the provider dir → upsert `dotenv-cloud.lock`.

## How the index is produced

The index is generated, not hand-edited. A GitHub Actions workflow
(`.github/workflows/registry.yml`) runs `registry/generate.py`, which reads the
provider catalog (`registry/providers.json`), queries each provider repo's
public GitHub Releases, and assembles `index.json` from the published
`.tar.gz` artifacts plus their `.sha256` / `.sig` sidecar assets. The result is
deployed to GitHub Pages.

It refreshes on: a manual run, a `repository_dispatch` (`registry-refresh`)
pushed by a provider's release workflow, completion of this repo's `Release`
workflow, and an hourly schedule.

## Index format

```json
{
  "schema_version": 1,
  "providers": {
    "aws": {
      "package": "dotenv-cloud-provider-aws",
      "description": "AWS Secrets Manager and SSM Parameter Store",
      "schemes": ["aws-secrets", "aws-ssm"],
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
  index `signature` field. The CLI ships with the project's **built-in trusted
  public key**, so signature verification is **on by default** with no
  configuration: a signed artifact must verify against a trusted key or the
  install is refused.
- Release artifacts are signed in CI. The maintainer holds the private key
  (`DOTENV_CLOUD_SIGNING_KEY` secret); the matching public key is baked into the
  CLI (`TRUSTED_PUBLIC_KEYS` in `src/provider/registry.rs`). Generate a keypair
  with `dotenv-cloud keygen`; sign a file with `dotenv-cloud sign <file>`.
- A **custom/private registry** can add its own trusted key via config:

  ```toml
  [provider_registry]
  url = "https://geetchoubey.github.io/dotenv-cloud/index.json"
  public_key = "<base64 ed25519 public key>"   # in addition to the built-in key
  allow_unsigned = false
  ```

- If an archive has **no signature**, the install is **refused** unless
  `allow_unsigned = true` or `--allow-unsigned` is passed.

## Install scope

- `--project` (default): `./.dotenv-cloud/providers/<name>/`
- `--user`: the platform user provider directory (see `dotenv-cloud doctor`).
