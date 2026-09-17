#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Fail the build when the Windows executable is not signed as expected.
#
# The counterpart of verify-macos-signing.sh: sign-windows.sh reports what
# CodeSignTool told it, and this reads the finished file instead. Runs on
# Linux with osslsigncode, so it checks the Authenticode structure, the
# digest, the timestamp and the signer — not the Windows root store.
#
# Reads ESIGNER_ENV: in PROD the signing chain must build to a trusted root;
# in TEST the SSL.com sandbox signs with a "- Development" root that no
# trust store carries, so only the chain check is skipped, and it must be
# that root and no other.

set -euo pipefail

# The common name on the code signing certificate, as SSL.com issued it.
# Windows shows this string as the publisher, so a change here is a change to
# what the user sees. The PROD value is filled in once the certificate exists;
# until then a production signing run fails here, on purpose.
EXPECTED_CN_PROD='HADAMARD GATE INCORPORATED'
EXPECTED_CN_TEST='Esigner LLC'

fail() {
    printf 'verify-windows-signing: %s\n' "$1" >&2
    exit 1
}

[ "$#" -eq 1 ] || fail "usage: verify-windows-signing.sh <path-to-exe>"
EXE="$1"
[ -f "$EXE" ] || fail "file not found: $EXE"

case "${ESIGNER_ENV:-}" in
    PROD) EXPECTED_CN="$EXPECTED_CN_PROD" ;;
    TEST) EXPECTED_CN="$EXPECTED_CN_TEST" ;;
    *) fail "ESIGNER_ENV must be TEST or PROD, got '${ESIGNER_ENV:-}'" ;;
esac
[ -n "$EXPECTED_CN" ] || fail "EXPECTED_CN_PROD is empty: set it to the CN on the issued certificate"

printf 'verify-windows-signing: verifying the signature on %s\n' "$EXE"
# osslsigncode exits 1 whenever the chain does not verify, which in TEST is
# expected, so read the report and decide from its contents.
REPORT=$(osslsigncode verify -in "$EXE" 2>&1 || true)
printf '%s\n' "$REPORT"

printf '%s' "$REPORT" | grep -q '^Signature Index: 0' ||
    fail "no Authenticode signature found"

CURRENT=$(printf '%s' "$REPORT" | awk '/^Current message digest/ {print $NF; exit}')
CALCULATED=$(printf '%s' "$REPORT" | awk '/^Calculated message digest/ {print $NF; exit}')
[ -n "$CURRENT" ] && [ "$CURRENT" = "$CALCULATED" ] ||
    fail "the signed digest does not match the file"

printf '%s' "$REPORT" | grep -q '^\s*Timestamp time:' ||
    fail "the signature carries no timestamp"

# The first Subject line under "Signer's certificate" is the leaf. osslsigncode
# 2.8 prints it as /C=US/…/CN=x/…, 2.14 as C=US,…,CN=x,…; accept both.
SIGNER=$(printf '%s' "$REPORT" | awk '/^Signer.s certificate/ {f=1} f && /Subject:/ {print; exit}')
printf '%s' "$SIGNER" | grep -Eq "CN=$EXPECTED_CN(,|/|\$)" ||
    fail "the signer is not $EXPECTED_CN: $SIGNER"

case "$ESIGNER_ENV" in
    PROD)
        printf '%s' "$REPORT" | grep -q '^Signature verification: ok' ||
            fail "the signing certificate chain does not verify"
        ;;
    TEST)
        printf '%s' "$REPORT" | grep -q 'Root Certification Authority RSA R2 - Development' ||
            fail "TEST signature does not chain to the SSL.com development root"
        printf 'verify-windows-signing: sandbox signature, chain trust not checked\n'
        ;;
esac

printf 'verify-windows-signing: ok\n'
