#!/bin/sh
# dotenv-cloud installer for macOS and Linux.
#
#   curl -fsSL https://geetchoubey.github.io/dotenv-cloud/install.sh | sh
#
# Environment overrides:
#   DOTENV_CLOUD_VERSION   tag to install (e.g. v0.1.0-beta.3). Default: latest release.
#   DOTENV_CLOUD_BIN_DIR   install directory. Default: $HOME/.local/bin.
#   DOTENV_CLOUD_TARGET    force a target triple (skips auto-detection).
#
# The downloaded archive is verified against its published SHA-256 digest.
set -eu

REPO="geetchoubey/dotenv-cloud"
BIN="dotenv-cloud"
BIN_DIR="${DOTENV_CLOUD_BIN_DIR:-$HOME/.local/bin}"

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "required tool '$1' not found"; }
need uname
need tar

# Prefer curl; fall back to wget.
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1"; }
  dlo() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO- "$1"; }
  dlo() { wget -qO "$2" "$1"; }
else
  err "need either 'curl' or 'wget'"
fi

detect_target() {
  if [ -n "${DOTENV_CLOUD_TARGET:-}" ]; then
    printf '%s' "$DOTENV_CLOUD_TARGET"
    return
  fi
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$arch" in
    x86_64 | amd64) arch="x86_64" ;;
    arm64 | aarch64) arch="aarch64" ;;
    *) err "unsupported architecture: $arch" ;;
  esac
  case "$os" in
    Darwin) printf '%s-apple-darwin' "$arch" ;;
    Linux)
      # Use musl on Alpine / non-glibc systems, gnu otherwise.
      if [ -f /etc/alpine-release ] || (ldd --version 2>&1 | grep -qi musl); then
        printf '%s-unknown-linux-musl' "$arch"
      else
        printf '%s-unknown-linux-gnu' "$arch"
      fi
      ;;
    *) err "unsupported OS: $os (try the manual download on the releases page)" ;;
  esac
}

latest_tag() {
  # Newest release including prereleases (releases/latest skips prereleases).
  # Buffer the response first so closing the grep pipe early can't SIGPIPE curl.
  body="$(dl "https://api.github.com/repos/$REPO/releases")"
  printf '%s' "$body" \
    | grep '"tag_name"' \
    | head -n1 \
    | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
}

verify_sha256() {
  file="$1"
  expected="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  else
    warn "no sha256 tool found; skipping integrity check"
    return
  fi
  [ "$actual" = "$expected" ] || err "SHA-256 mismatch (expected $expected, got $actual)"
  info "SHA-256 verified"
}

TARGET="$(detect_target)"
TAG="${DOTENV_CLOUD_VERSION:-$(latest_tag)}"
[ -n "$TAG" ] || err "could not determine the latest version"

ARCHIVE="${BIN}-${TAG}-${TARGET}.tar.gz"
BASE="https://github.com/$REPO/releases/download/$TAG"

info "Installing $BIN $TAG ($TARGET)"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

info "Downloading $ARCHIVE"
dlo "$BASE/$ARCHIVE" "$TMP/$ARCHIVE" || err "download failed; is $TARGET published for $TAG?"

# Verify integrity against the published .sha256 sidecar.
if sha_line="$(dl "$BASE/$ARCHIVE.sha256" 2>/dev/null)"; then
  verify_sha256 "$TMP/$ARCHIVE" "$(printf '%s' "$sha_line" | awk '{print $1}')"
else
  warn "no .sha256 sidecar found; skipping integrity check"
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
SRC="$(find "$TMP" -type f -name "$BIN" -perm -u+x 2>/dev/null | head -n1)"
[ -n "$SRC" ] || SRC="$(find "$TMP" -type f -name "$BIN" | head -n1)"
[ -n "$SRC" ] || err "could not find '$BIN' inside the archive"

mkdir -p "$BIN_DIR"
install -m 0755 "$SRC" "$BIN_DIR/$BIN" 2>/dev/null || { cp "$SRC" "$BIN_DIR/$BIN"; chmod 0755 "$BIN_DIR/$BIN"; }

# Clear the macOS quarantine flag so Gatekeeper doesn't block first launch.
if [ "$(uname -s)" = "Darwin" ]; then
  xattr -d com.apple.quarantine "$BIN_DIR/$BIN" 2>/dev/null || true
fi

info "Installed to $BIN_DIR/$BIN"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    printf '\n'
    warn "$BIN_DIR is not on your PATH. Add it:"
    printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
    ;;
esac

printf '\n'
info "Done. Get started:"
printf '    %s --help\n' "$BIN"
printf '    %s init            # detect & install providers for your project\n' "$BIN"
