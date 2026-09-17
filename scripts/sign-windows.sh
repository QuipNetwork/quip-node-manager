#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Sign a Windows executable with SSL.com eSigner, in place.
#
# eSigner keeps the private key in SSL.com's HSM. CodeSignTool hashes the file
# locally, sends the hash for signing, and splices the returned signature into
# the PE — no Windows API is involved, so this runs from a Linux job on the
# artifact the Windows build produced.
#
# Reads from the environment:
#   ESIGNER_ENV            TEST (SSL.com sandbox) or PROD
#   ESIGNER_USERNAME       SSL.com account username
#   ESIGNER_PASSWORD       SSL.com account password
#   ESIGNER_CREDENTIAL_ID  eSigner credential ID of the code signing certificate
#   ESIGNER_TOTP_SECRET    TOTP secret shown once during eSigner enrollment
#
# Needs java, curl, unzip, sha256sum and openssl on PATH.

set -euo pipefail

# The SSL.com download page serves an unversioned zip that changes under the
# same URL. GitHub releases are versioned, so pin one and check its hash: a
# changed download fails here rather than signing with a tool nobody vetted.
CODESIGNTOOL_VERSION=1.3.2
CODESIGNTOOL_SHA256=f14b1e1ef14bfa1fd00279c363aab0debbf5dcfba0e4bcdce5d22bb771de0e3a
CODESIGNTOOL_URL="https://github.com/SSLcom/CodeSignTool/releases/download/v${CODESIGNTOOL_VERSION}/CodeSignTool-v${CODESIGNTOOL_VERSION}.zip"

fail() {
    printf 'sign-windows: %s\n' "$1" >&2
    exit 1
}

[ "$#" -eq 1 ] || fail "usage: sign-windows.sh <path-to-exe>"
[ -f "$1" ] || fail "file not found: $1"
# Absolute, because CodeSignTool runs from its own directory below.
EXE=$(realpath "$1")

for var in ESIGNER_ENV ESIGNER_USERNAME ESIGNER_PASSWORD ESIGNER_CREDENTIAL_ID ESIGNER_TOTP_SECRET; do
    [ -n "${!var:-}" ] || fail "$var is not set"
done

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

printf 'sign-windows: fetching CodeSignTool v%s\n' "$CODESIGNTOOL_VERSION"
curl -fsSL --proto '=https' --tlsv1.2 -o "$WORK/cst.zip" "$CODESIGNTOOL_URL"
printf '%s  %s\n' "$CODESIGNTOOL_SHA256" "$WORK/cst.zip" | sha256sum -c --quiet ||
    fail "CodeSignTool download does not match the pinned sha256"
unzip -q "$WORK/cst.zip" -d "$WORK/cst"
JAR="$WORK/cst/jar/code_sign_tool-${CODESIGNTOOL_VERSION}.jar"
[ -f "$JAR" ] || fail "jar not found in the CodeSignTool zip"

# CodeSignTool reads conf/code_sign_tool.properties next to its jar. The zip
# ships the production endpoints; the sandbox ones come from SSL.com's
# "eSigner demo credentials" guide.
JAVA_OPTS=()
case "$ESIGNER_ENV" in
    PROD)
        printf 'sign-windows: signing with the production eSigner service\n'
        ;;
    TEST)
        printf 'sign-windows: signing with the eSigner SANDBOX — this signature is not trusted by Windows\n'
        cat >"$WORK/cst/conf/code_sign_tool.properties" <<'EOF'
CLIENT_ID=qOUeZCCzSqgA93acB3LYq6lBNjgZdiOxQc-KayC3UMw
OAUTH2_ENDPOINT=https://oauth-sandbox.ssl.com/oauth2/token
CSC_API_ENDPOINT=https://cs-try.ssl.com
TSA_URL=http://ts.ssl.com
EOF
        # cs-try.ssl.com chains to "SSL.com TLS RSA Root CA 2022", which the Java
        # runtime's cacerts does not carry (SSL.com's own CodeSignTool container
        # fails the same way). Production endpoints chain to older roots and need
        # nothing. Take the root off the served chain: it is self-signed, so this
        # is trust-on-first-use, acceptable for a sandbox that signs nothing real.
        openssl s_client -connect cs-try.ssl.com:443 -servername cs-try.ssl.com -showcerts \
            </dev/null 2>/dev/null |
            awk '/BEGIN CERT/{n++} n{buf[n]=buf[n] $0 "\n"} END{printf "%s", buf[n]}' \
                >"$WORK/sandbox-root.pem"
        openssl x509 -in "$WORK/sandbox-root.pem" -noout -subject | grep -q 'SSL.com TLS RSA Root CA 2022' ||
            fail "the certificate served by cs-try.ssl.com does not end in the expected root"
        JAVA_HOME_DIR=$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")
        cp "$JAVA_HOME_DIR/lib/security/cacerts" "$WORK/cacerts"
        keytool -importcert -noprompt -keystore "$WORK/cacerts" -storepass changeit \
            -alias sslcom-tls-rsa-root-2022 -file "$WORK/sandbox-root.pem" >/dev/null
        JAVA_OPTS=(-Djavax.net.ssl.trustStore="$WORK/cacerts" -Djavax.net.ssl.trustStorePassword=changeit)
        ;;
    *)
        fail "ESIGNER_ENV must be TEST or PROD, got '$ESIGNER_ENV'"
        ;;
esac

# CodeSignTool refuses to overwrite its input, so sign into a scratch
# directory and move the result back over the original.
OUT="$WORK/signed"
mkdir -p "$OUT"
# Credentials go on the command line; CodeSignTool offers no other channel.
# The job log does not echo this line because set -x is never enabled here.
#
# CodeSignTool exits 0 on its own errors ("Invalid input file path", a
# rejected login), so its exit status proves nothing. The gate is the output
# file below. The grep only drops a JVM warning it prints on every run, and
# the `|| true` keeps an all-warning output from tripping pipefail.
(cd "$WORK/cst" && java "${JAVA_OPTS[@]}" -jar "$JAR" sign \
    -username="$ESIGNER_USERNAME" \
    -password="$ESIGNER_PASSWORD" \
    -credential_id="$ESIGNER_CREDENTIAL_ID" \
    -totp_secret="$ESIGNER_TOTP_SECRET" \
    -input_file_path="$EXE" \
    -output_dir_path="$OUT") |
    { grep -v 'sun.reflect.Reflection.getCallerClass' || true; }

SIGNED="$OUT/$(basename "$EXE")"
[ -f "$SIGNED" ] || fail "CodeSignTool produced no output file; see its message above"
mv "$SIGNED" "$EXE"
printf 'sign-windows: signed %s\n' "$EXE"
