# Provider Plugin Protocol (v1)

Provider plugins are standalone executables. The `dotenv-cloud` core launches a
plugin as a child process and exchanges **newline-delimited JSON** messages over
the plugin's **stdin** (requests) and **stdout** (responses).

Rules:

- **stdout** must contain only protocol JSON, one message per line.
- **stderr** is for redacted diagnostics only; the core treats it as untrusted.
- A plugin must never print secret values to stderr or logs.
- A plugin handles requests until its stdin is closed, then exits.

## Installation layout

Place the executable and a manifest under a provider directory:

```
.dotenv-cloud/providers/<name>/
  manifest.toml
  <executable>
```

`manifest.toml`:

```toml
name = "dotenv-cloud-provider-aws"
version = "1.0.0"
protocol_version = "1"
executable = "dotenv-cloud-provider-aws"   # relative to this dir, or absolute
schemes = ["aws-secrets", "aws-ssm"]
description = "AWS Secrets Manager and SSM provider"

[integrity]
sha256 = "..."
signature = "..."
```

Discovery order: explicit `[providers.paths]` in config → `.dotenv-cloud/providers`
→ user provider directory (`~/Library/Application Support/dotenv-cloud/providers`
on macOS, `${XDG_DATA_HOME:-~/.local/share}/dotenv-cloud/providers` on Linux,
`%LOCALAPPDATA%\dotenv-cloud\providers` on Windows).

## Handshake

Core → plugin (first message):

```json
{"type":"handshake","protocol_version":"1","dotenv_cloud_version":"0.1.0"}
```

Plugin → core:

```json
{"type":"handshake_result","protocol_version":"1",
 "plugin":{"name":"dotenv-cloud-provider-aws","version":"1.0.0","schemes":["aws-secrets","aws-ssm"]}}
```

The core rejects a plugin whose `protocol_version` differs from `1`.

## Resolve

Core → plugin:

```json
{"type":"resolve","request_id":"req-1","profile":"dev",
 "reference":{"original":"aws-secrets://prod/db/password","scheme":"aws-secrets",
   "authority":"prod","path":"/db/password","fragment":null,"query":{}},
 "provider_config":{"region":"us-east-1","timeout_ms":2000}}
```

Plugin → core (success):

```json
{"type":"resolve_result","request_id":"req-1","value":"secret-value",
 "metadata":{"provider":"aws-secrets","version":"AWSCURRENT"}}
```

Plugin → core (error):

```json
{"type":"error","request_id":"req-1","class":"PermissionDenied",
 "message":"access denied","reference":"aws-secrets://prod/db/[redacted]"}
```

### Error classes

`AuthenticationFailed`, `PermissionDenied`, `NotFound`, `InvalidReference`,
`InvalidSecretPayload`, `Timeout`, `RateLimited`, `Network`,
`ProviderUnavailable`, `Internal`.

Error messages must include enough context to act (key/provider/redacted
reference) but must never contain resolved values, tokens, or credentials.

## Minimal reference implementation (Python)

```python
#!/usr/bin/env python3
import sys, json

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg["type"] == "handshake":
        print(json.dumps({
            "type": "handshake_result", "protocol_version": "1",
            "plugin": {"name": "my-provider", "version": "0.1.0", "schemes": ["aws-secrets"]},
        }), flush=True)
    elif msg["type"] == "resolve":
        ref = msg["reference"]
        # ... resolve ref["path"] / ref["fragment"] using your SDK ...
        print(json.dumps({
            "type": "resolve_result", "request_id": msg["request_id"],
            "value": "resolved-secret", "metadata": {"provider": ref["scheme"]},
        }), flush=True)
```
