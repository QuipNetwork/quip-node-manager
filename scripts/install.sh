#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Quip Node Manager installer for macOS and Linux.
# Usage: curl -fsSL https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.sh | sh
set -eu

REPO="quip.network%2Fquip-node-manager"
API="https://gitlab.com/api/v4/projects/${REPO}/releases"

info()  { printf '\033[1;34m>\033[0m %s\n' "$*"; }
error() { printf '\033[1;31m!\033[0m %s\n' "$*" >&2; exit 1; }

# ── Detect platform ──────────────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux)  PLATFORM="linux" ;;
  *)      error "Unsupported OS: $OS. Use install.ps1 for Windows." ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH="x86_64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *)             error "Unsupported architecture: $ARCH" ;;
esac

# ── Fetch latest release tag ────────────────────────────────────────────────
info "Fetching latest release..."
TAG=$(curl -fsSL "$API" | grep -o '"tag_name":"[^"]*"' | head -1 | cut -d'"' -f4)
[ -z "$TAG" ] && error "Could not determine latest release."
info "Latest release: $TAG"

# ── Build artifact URL ──────────────────────────────────────────────────────
BASE="https://gitlab.com/quip.network/quip-node-manager/-/jobs/artifacts/${TAG}/raw/dist"

case "$PLATFORM" in
  macos)
    ARTIFACT="quip-node-manager-macos-universal.dmg"
    URL="${BASE}/${ARTIFACT}?job=build-macos-universal"
    ;;
  linux)
    ARTIFACT="quip-node-manager-linux-x86_64.AppImage"
    URL="${BASE}/${ARTIFACT}?job=build-linux-x86_64"
    ;;
esac

# ── Download ────────────────────────────────────────────────────────────────
TMPDIR="${TMPDIR:-/tmp}"
DEST="${TMPDIR}/${ARTIFACT}"
info "Downloading ${ARTIFACT}..."
curl -fSL --progress-bar -o "$DEST" "$URL" || error "Download failed."

# ── Verify ──────────────────────────────────────────────────────────────────
# No Linux or macOS tool checks a file fetched this way on its own, so the
# release publishes SHA256SUMS and a detached signature over it. The checksum
# is mandatory: a mismatch means the download is not the released artifact,
# whatever the cause. The signature is checked when gpg is present, and its
# absence is reported rather than passed over in silence.
#
# RELEASE_KEY_FINGERPRINT pins the signer. Fetching the key without pinning it
# would verify only that the file and the key came from the same place.
RELEASE_KEY_FINGERPRINT="A63860E21E7070C2C26FDA5DC85BEAB01AD9FEE3"
SUMS_URL="${BASE}/SHA256SUMS?job=sign-artifacts"
SIG_URL="${BASE}/SHA256SUMS.asc?job=sign-artifacts"
KEY_URL="https://gitlab.com/quip.network/quip-node-manager/-/raw/${TAG}/.gitlab/release-signing-key.asc"

info "Verifying checksum..."
SUMS="${TMPDIR}/quip-SHA256SUMS.$$"
curl -fsSL -o "$SUMS" "$SUMS_URL" || error "Could not download SHA256SUMS."

EXPECTED=$(awk -v f="$ARTIFACT" '$2 == f || $2 == "*" f {print $1; exit}' "$SUMS")
[ -n "$EXPECTED" ] || error "SHA256SUMS lists no entry for ${ARTIFACT}."

if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$DEST" | cut -d" " -f1)
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL=$(shasum -a 256 "$DEST" | cut -d" " -f1)
else
  error "Neither sha256sum nor shasum is available; cannot verify the download."
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
  rm -f "$DEST" "$SUMS"
  error "Checksum mismatch for ${ARTIFACT}. The download was discarded."
fi
info "Checksum matches."

if command -v gpg >/dev/null 2>&1; then
  info "Verifying signature..."
  SIG="${TMPDIR}/quip-SHA256SUMS.asc.$$"
  KEY="${TMPDIR}/quip-release-key.asc.$$"
  KEYRING="${TMPDIR}/quip-keyring.$$"
  curl -fsSL -o "$SIG" "$SIG_URL" || error "Could not download SHA256SUMS.asc."
  curl -fsSL -o "$KEY" "$KEY_URL" || error "Could not download the release signing key."

  gpg --batch --no-default-keyring --keyring "$KEYRING" --quiet --import "$KEY" \
    || error "Could not read the release signing key."
  FOUND=$(gpg --batch --no-default-keyring --keyring "$KEYRING" --list-keys --with-colons \
    | awk -F: '/^fpr/ {print $10; exit}')
  if [ "$FOUND" != "$RELEASE_KEY_FINGERPRINT" ]; then
    rm -f "$DEST" "$SUMS" "$SIG" "$KEY" "$KEYRING" "${KEYRING}~"
    error "Release key fingerprint is ${FOUND}, expected ${RELEASE_KEY_FINGERPRINT}."
  fi
  if ! gpg --batch --no-default-keyring --keyring "$KEYRING" --verify "$SIG" "$SUMS" 2>/dev/null; then
    rm -f "$DEST" "$SUMS" "$SIG" "$KEY" "$KEYRING" "${KEYRING}~"
    error "Signature on SHA256SUMS is not valid. The download was discarded."
  fi
  info "Signature verified."
  rm -f "$SIG" "$KEY" "$KEYRING" "${KEYRING}~"
else
  info "gpg is not installed, so the signature was not checked. The checksum was."
fi
rm -f "$SUMS"

# ── Install ─────────────────────────────────────────────────────────────────
case "$PLATFORM" in
  macos)
    info "Mounting DMG..."
    # The mount point is the trailing column of hdiutil's output and can
    # contain spaces (e.g. "/Volumes/Quip Node Manager"). Capture from
    # /Volumes/ to end of line; splitting on whitespace would truncate it.
    MOUNT_DIR=$(hdiutil attach "$DEST" -nobrowse -noautoopen 2>/dev/null \
      | grep -o '/Volumes/.*' | head -1)
    [ -z "$MOUNT_DIR" ] && error "Failed to mount DMG."
    APP_NAME=$(find "$MOUNT_DIR" -maxdepth 1 -name '*.app' | head -1)
    if [ -z "$APP_NAME" ]; then
      hdiutil detach "$MOUNT_DIR" -quiet 2>/dev/null || true
      error "No .app found in DMG."
    fi
    BASENAME=$(basename "$APP_NAME")
    info "Installing ${BASENAME} to /Applications..."
    rm -rf "/Applications/${BASENAME}"
    cp -R "$APP_NAME" /Applications/
    hdiutil detach "$MOUNT_DIR" -quiet 2>/dev/null || true
    rm -f "$DEST"
    info "Installed to /Applications/${BASENAME}"
    info "Launch from Applications or run: open /Applications/${BASENAME}"
    ;;
  linux)
    INSTALL_DIR="${HOME}/.local/bin"
    mkdir -p "$INSTALL_DIR"
    INSTALL_PATH="${INSTALL_DIR}/quip-node-manager"
    mv "$DEST" "$INSTALL_PATH"
    chmod +x "$INSTALL_PATH"
    info "Installed to ${INSTALL_PATH}"
    case ":$PATH:" in
      *":${INSTALL_DIR}:"*) ;;
      *) info "Add ${INSTALL_DIR} to your PATH if not already present." ;;
    esac
    info "Run: quip-node-manager"
    ;;
esac

info "Done."
