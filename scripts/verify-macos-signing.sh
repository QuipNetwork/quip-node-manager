#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Fail the build when the macOS artifacts are not signed and notarized.
#
# Tauri logs a warning and exits 0 when the notarization credentials are
# absent. See crates/tauri-bundler/src/bundle/macos/app.rs. Without this check
# a release ships unsigned and nobody finds out until a user meets Gatekeeper.

set -euo pipefail

# The common name on the Developer ID Application certificate, which is the
# organization name as it appears on the Apple Developer Program enrollment.
# Gatekeeper shows this string to the user, so a change here is a change to
# what the user sees.
EXPECTED_AUTHORITY='Developer ID Application: Richard Carback'

fail() {
    printf 'verify-macos-signing: %s\n' "$1" >&2
    exit 1
}

[ "$#" -eq 2 ] || fail "usage: verify-macos-signing.sh <path-to-app> <path-to-dmg>"

APP="$1"
DMG="$2"
[ -d "$APP" ] || fail "app bundle not found: $APP"
[ -f "$DMG" ] || fail "disk image not found: $DMG"

printf 'verify-macos-signing: verifying the signature on %s\n' "$APP"
codesign --verify --deep --strict --verbose=2 "$APP" ||
    fail "codesign rejected $APP"

DETAILS=$(codesign --display --verbose=4 "$APP" 2>&1)
printf '%s\n' "$DETAILS"

printf '%s' "$DETAILS" | grep -q "Authority=$EXPECTED_AUTHORITY" ||
    fail "the signer is not a $EXPECTED_AUTHORITY certificate"

printf '%s' "$DETAILS" | grep -q '^Timestamp=' ||
    fail "the signature carries no secure timestamp"

printf '%s' "$DETAILS" | grep -q 'flags=.*runtime' ||
    fail "the hardened runtime is not enabled"

printf 'verify-macos-signing: asking Gatekeeper about %s\n' "$APP"
ASSESS=$(spctl --assess --type execute --verbose=4 "$APP" 2>&1) ||
    fail "Gatekeeper rejected $APP"
printf '%s\n' "$ASSESS"

printf '%s' "$ASSESS" | grep -q 'source=Notarized Developer ID' ||
    fail "$APP is signed but not notarized"

printf 'verify-macos-signing: checking the stapled tickets\n'
xcrun stapler validate "$APP" || fail "no stapled ticket on $APP"
xcrun stapler validate "$DMG" || fail "no stapled ticket on $DMG"

printf 'verify-macos-signing: all checks passed\n'
