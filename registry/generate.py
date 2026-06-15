#!/usr/bin/env python3
"""Generate the dotenv-cloud provider registry index.json.

Reads registry/providers.json (the catalog), queries each provider repo's
public GitHub Releases, and emits an index mapping
provider -> version -> target -> {url, sha256, signature}.

Only `.tar.gz` artifacts are indexed (the format the CLI installer unpacks);
`.zip` (Windows) assets are skipped. sha256/signature are read from the
`<asset>.sha256` / `<asset>.sig` sidecar assets published alongside each
archive.

Auth: uses GITHUB_TOKEN (if set) for higher API rate limits; public repos
need no special scope.
"""

import json
import os
import sys
import urllib.error
import urllib.request

API = "https://api.github.com"
HERE = os.path.dirname(os.path.abspath(__file__))


def gh_request(url, token, raw=False):
    req = urllib.request.Request(url)
    req.add_header("Accept", "application/octet-stream" if raw else "application/vnd.github+json")
    req.add_header("X-GitHub-Api-Version", "2022-11-28")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read()


def fetch_asset_text(asset, token):
    """Download an asset's content via the API URL (works for public and, with a
    token, private repos), returning it as stripped text."""
    return gh_request(asset["url"], token, raw=True).decode("utf-8", "replace").strip()


def list_releases(repo, token):
    out = []
    page = 1
    while True:
        url = f"{API}/repos/{repo}/releases?per_page=100&page={page}"
        try:
            data = json.loads(gh_request(url, token))
        except urllib.error.HTTPError as e:
            print(f"  warning: cannot list releases for {repo}: {e}", file=sys.stderr)
            break
        if not data:
            break
        out.extend(data)
        if len(data) < 100:
            break
        page += 1
    return out


def target_from_asset(name, bin_prefix, tag):
    """dotenv-cloud-provider-aws-v0.1.0-beta.3-x86_64-apple-darwin.tar.gz -> triple."""
    prefix = f"{bin_prefix}-{tag}-"
    if not name.startswith(prefix) or not name.endswith(".tar.gz"):
        return None
    return name[len(prefix):-len(".tar.gz")]


def build_provider(p, token):
    versions = {}
    for rel in list_releases(p["repo"], token):
        if rel.get("draft"):
            continue
        tag = rel["tag_name"]
        version = tag[1:] if tag.startswith("v") else tag
        assets = {a["name"]: a for a in rel.get("assets", [])}
        targets = {}
        for name, asset in assets.items():
            target = target_from_asset(name, p["bin_prefix"], tag)
            if not target:
                continue
            sha_asset = assets.get(f"{name}.sha256")
            sig_asset = assets.get(f"{name}.sig")
            if not sha_asset:
                print(f"  warning: {name} has no .sha256; skipping", file=sys.stderr)
                continue
            sha = fetch_asset_text(sha_asset, token).split()[0]
            entry = {"url": asset["browser_download_url"], "sha256": sha}
            if sig_asset:
                entry["signature"] = fetch_asset_text(sig_asset, token)
            targets[target] = entry
        if targets:
            versions[version] = {"targets": targets}
            print(f"  {p['name']} {version}: {len(targets)} target(s)")
    return {
        "package": p["package"],
        "description": p.get("description"),
        "schemes": p["schemes"],
        "versions": versions,
    }


def main():
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    with open(os.path.join(HERE, "providers.json")) as f:
        catalog = json.load(f)

    providers = {}
    for p in catalog["providers"]:
        print(f"scanning {p['repo']}")
        prov = build_provider(p, token)
        if prov["versions"]:
            providers[p["name"]] = prov
        else:
            print(f"  warning: no indexable releases for {p['name']}", file=sys.stderr)

    index = {"schema_version": 1, "providers": providers}

    out_dir = sys.argv[1] if len(sys.argv) > 1 else "_site"
    os.makedirs(out_dir, exist_ok=True)
    out_path = os.path.join(out_dir, "index.json")
    with open(out_path, "w") as f:
        json.dump(index, f, indent=2, sort_keys=True)
        f.write("\n")
    print(f"wrote {out_path} ({len(providers)} provider(s))")


if __name__ == "__main__":
    main()
