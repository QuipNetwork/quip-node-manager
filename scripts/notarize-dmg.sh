#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Notarize and staple a DMG.
#
# Tauri notarizes and staples the .app bundle, but only signs the DMG that
# carries it. See crates/tauri-bundler/src/bundle/macos/dmg/mod.rs. A DMG that
# a browser downloads gets a quarantine attribute, and Gatekeeper judges the
# container as well as its contents, so the container needs its own ticket.

set -euo pipefail

fail() {
    printf 'notarize-dmg: %s\n' "$1" >&2
    exit 1
}

[ "$#" -eq 1 ] || fail "usage: notarize-dmg.sh <path-to-dmg>"

DMG="$1"
[ -f "$DMG" ] || fail "disk image not found: $DMG"

for var in APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH; do
    [ -n "${!var:-}" ] || fail "missing environment variable: $var"
done

[ -f "$APPLE_API_KEY_PATH" ] || fail "API key not found: $APPLE_API_KEY_PATH"

printf 'notarize-dmg: submitting %s\n' "$DMG"

set +e
SUBMIT_OUTPUT=$(xcrun notarytool submit "$DMG" \
    --key "$APPLE_API_KEY_PATH" \
    --key-id "$APPLE_API_KEY" \
    --issuer "$APPLE_API_ISSUER" \
    --wait \
    --timeout 30m \
    --output-format json 2>&1)
SUBMIT_RC=$?
set -e

printf '%s\n' "$SUBMIT_OUTPUT"

SUBMISSION_ID=$(printf '%s' "$SUBMIT_OUTPUT" |
    sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' | tail -1)
STATUS=$(printf '%s' "$SUBMIT_OUTPUT" |
    sed -n 's/.*"status" *: *"\([^"]*\)".*/\1/p' | tail -1)

if [ "$SUBMIT_RC" -ne 0 ] || [ "$STATUS" != "Accepted" ]; then
    if [ -n "$SUBMISSION_ID" ]; then
        printf 'notarize-dmg: fetching the rejection log\n' >&2
        xcrun notarytool log "$SUBMISSION_ID" \
            --key "$APPLE_API_KEY_PATH" \
            --key-id "$APPLE_API_KEY" \
            --issuer "$APPLE_API_ISSUER" >&2 || true
    fi
    fail "notarization status: ${STATUS:-unknown}"
fi

printf 'notarize-dmg: stapling %s\n' "$DMG"
xcrun stapler staple "$DMG" || fail "stapler staple failed for $DMG"
xcrun stapler validate "$DMG" || fail "stapler validate failed for $DMG"

printf 'notarize-dmg: %s is notarized and stapled\n' "$DMG"
