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
# Resolve the redirect first, then fetch without -L. curl draws one progress
# bar per HTTP transfer, and this URL redirects to a job-specific one, so a
# single -L call drew two overlapping bars: the second bar's opening frame
# landed past the end of the first bar's finished line and nothing erased it.
# --head keeps the resolve free -- without it, -o /dev/null downloads the whole
# artifact just to learn the URL. Should the resolved URL ever start
# redirecting too, the checksum below catches the redirect page as a mismatch.
REAL_URL=$(curl -fsSL --head -o /dev/null -w '%{url_effective}' "$URL") \
  || error "Could not resolve the download URL."
[ -n "$REAL_URL" ] || error "Resolving the download URL produced nothing."
curl -fS --progress-bar -o "$DEST" "$REAL_URL" || error "Download failed."

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

# Every failure from here on discards the download: an unverified artifact
# left behind in TMPDIR is worse than no artifact at all. SIG, KEY and GPGHOME
# stay unset until the signature step creates them, and rm ignores an empty
# operand, so this is safe to call anywhere past the SHA256SUMS download.
discard() {
    if [ -n "${GPGHOME:-}" ]; then
        # rm -rf unlinks the directory but does not stop the keyboxd and
        # gpg-agent processes that gpg 2.4 and later start against it, so
        # they would outlive the home they were pointed at. Kill them first.
        GNUPGHOME="$GPGHOME" gpgconf --kill all >/dev/null 2>&1 || true
        rm -rf "$GPGHOME"
    fi
    rm -f "$DEST" "$SUMS" "${SIG:-}" "${KEY:-}"
}

info "Verifying checksum..."
SUMS="${TMPDIR}/quip-SHA256SUMS.$$"
curl -fsSL -o "$SUMS" "$SUMS_URL" \
  || { discard; error "Could not download SHA256SUMS."; }

EXPECTED=$(awk -v f="$ARTIFACT" '$2 == f || $2 == "*" f {print $1; exit}' "$SUMS")
[ -n "$EXPECTED" ] || { discard; error "SHA256SUMS lists no entry for ${ARTIFACT}."; }

# Take the exit status of the checksum tool. A bare ACTUAL=$(...) hides it from
# set -e, so a tool that fails outright leaves ACTUAL empty and is reported
# below as a checksum mismatch: the download gets blamed for a broken local
# tool, and the operator is told the release is corrupt when it is not.
if command -v sha256sum >/dev/null 2>&1; then
  DIGEST=$(sha256sum "$DEST") \
    || { discard; error "sha256sum could not read ${DEST}."; }
elif command -v shasum >/dev/null 2>&1; then
  DIGEST=$(shasum -a 256 "$DEST") \
    || { discard; error "shasum could not read ${DEST}."; }
else
  discard
  error "Neither sha256sum nor shasum is available; cannot verify the download."
fi
ACTUAL=$(printf '%s\n' "$DIGEST" | cut -d" " -f1)
[ -n "$ACTUAL" ] \
  || { discard; error "The checksum tool printed no digest for ${ARTIFACT}."; }

if [ "$EXPECTED" != "$ACTUAL" ]; then
  discard
  error "Checksum mismatch for ${ARTIFACT}. The download was discarded."
fi
info "Checksum matches."

if command -v gpg >/dev/null 2>&1; then
  info "Verifying signature..."
  SIG="${TMPDIR}/quip-SHA256SUMS.asc.$$"
  KEY="${TMPDIR}/quip-release-key.asc.$$"
  curl -fsSL -o "$SIG" "$SIG_URL" \
    || { discard; error "Could not download SHA256SUMS.asc."; }
  curl -fsSL -o "$KEY" "$KEY_URL" \
    || { discard; error "Could not download the release signing key."; }

  # A throwaway GNUPGHOME, not --keyring. GnuPG 2.4 and later keep public keys
  # in keyboxd, which ignores --keyring and --no-default-keyring outright: it
  # says so on stderr and carries on. The key then lands in the caller's own
  # keyring and the fingerprint read back is whatever keyboxd already held --
  # another key, or nothing. A separate home directory is the one isolation
  # every gpg version honours, and it is what .gitlab-ci.yml already does on
  # the signing side. mktemp rather than a PID-derived name because TMPDIR
  # falls back to a world-writable /tmp and mkdir -p accepts a path somebody
  # else created first.
  GPGHOME=$(mktemp -d "${TMPDIR}/quip-gnupg.XXXXXX") \
    || { discard; error "Could not create a temporary GnuPG home."; }

  GNUPGHOME="$GPGHOME" gpg --batch --quiet --import "$KEY" \
    || { discard; error "Could not read the release signing key."; }

  # Same reason as the checksum tool above: take the exit status, so a gpg
  # that cannot answer is reported as a broken gpg and not as the wrong signer.
  FOUND=$(GNUPGHOME="$GPGHOME" gpg --batch --list-keys --with-colons) \
    || { discard; error "Could not read the release key back from gpg."; }
  FOUND=$(printf '%s\n' "$FOUND" | awk -F: '/^fpr/ {print $10; exit}')
  [ -n "$FOUND" ] \
    || { discard; error "gpg reported no fingerprint for the release key."; }

  if [ "$FOUND" != "$RELEASE_KEY_FINGERPRINT" ]; then
    discard
    error "Release key fingerprint is ${FOUND}, expected ${RELEASE_KEY_FINGERPRINT}."
  fi
  if ! GNUPGHOME="$GPGHOME" gpg --batch --verify "$SIG" "$SUMS" 2>/dev/null; then
    discard
    error "Signature on SHA256SUMS is not valid. The download was discarded."
  fi
  info "Signature verified."
  GNUPGHOME="$GPGHOME" gpgconf --kill all >/dev/null 2>&1 || true
  rm -rf "$GPGHOME"
  rm -f "$SIG" "$KEY"
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
