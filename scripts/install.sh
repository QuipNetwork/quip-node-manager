#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Quip Node Manager installer for macOS and Linux.
# Usage: curl -fsSL https://gitlab.com/piqued/quip-node-manager/-/raw/main/scripts/install.sh | sh
set -eu

REPO="piqued%2Fquip-node-manager"
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
BASE="https://gitlab.com/piqued/quip-node-manager/-/jobs/artifacts/${TAG}/raw/dist"

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

# ── Install ─────────────────────────────────────────────────────────────────
case "$PLATFORM" in
  macos)
    info "Mounting DMG..."
    MOUNT_DIR=$(hdiutil attach "$DEST" -nobrowse -noautoopen 2>/dev/null \
      | tail -1 | awk '{print $NF}')
    [ -z "$MOUNT_DIR" ] && MOUNT_DIR=$(hdiutil attach "$DEST" -nobrowse -noautoopen 2>/dev/null \
      | grep '/Volumes' | sed 's/.*\(\/Volumes\/.*\)/\1/')
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
    info "Removing quarantine flag..."
    xattr -dr com.apple.quarantine "/Applications/${BASENAME}" 2>/dev/null || true
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
