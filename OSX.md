<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# macOS Code Signing & Distribution

This guide covers code signing, notarization, and distribution of the
Quip Node Manager Tauri app on macOS.

## Prerequisites

- **Apple Developer Program** membership ($99/year) --
  [developer.apple.com/programs](https://developer.apple.com/programs/)
- **Organization membership**, not individual. Individual membership puts the
  personal legal name of the enrollee in the Gatekeeper prompt.
- **Xcode CLI tools** installed: `xcode-select --install`
- An **Apple ID** enrolled in the Developer Program
- An **App Store Connect API key**, created at
  [appstoreconnect.apple.com](https://appstoreconnect.apple.com/access/integrations/api)
  with the Developer role. Download the `.p8` file once, because Apple does
  not offer it a second time. Record the Key ID and the Issuer ID with it.

## Step 1: Create a Developer ID Certificate

1. Open [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates).
2. Click the **+** button to create a new certificate.
3. Select **Developer ID Application** (for distributing outside the App Store).
4. Generate a Certificate Signing Request (CSR) using Keychain Access:
   - Open Keychain Access > Certificate Assistant > Request a Certificate From
     a Certificate Authority.
   - Enter your email, select "Saved to disk," and save the `.certSigningRequest`
     file.
5. Upload the CSR and download the resulting `.cer` file.
6. Double-click the `.cer` file to install it into your login keychain.
7. Verify installation:

```bash
security find-identity -v -p codesigning
# Should list: "Developer ID Application: TEAM_NAME (TEAM_ID)"
```

## Step 2: Set the signing environment

Tauri signs, notarizes, and staples the application bundle during the build.
Set these variables and Tauri signs the build. Nothing else calls
`codesign`.

| Variable | Value |
|---|---|
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Richard Carback (W64XX7HSTH)` |
| `APPLE_API_KEY` | App Store Connect Key ID |
| `APPLE_API_ISSUER` | App Store Connect Issuer ID |
| `APPLE_API_KEY_PATH` | path to the downloaded `.p8` file |

On a machine that already holds the certificate in its login keychain,
`APPLE_SIGNING_IDENTITY` is enough. In CI, set `APPLE_CERTIFICATE` to a base64
`.p12` and `APPLE_CERTIFICATE_PASSWORD` to its export password instead. Tauri
then creates a temporary keychain and imports the certificate. It deletes the
keychain when the build ends.

## Step 3: Build

```bash
bun run tauri build --target universal-apple-darwin
```

Tauri signs the bundle from the inside out and enables the hardened runtime.
It then submits the bundle to Apple and staples the ticket that comes back.

The result is at:

```
src-tauri/target/universal-apple-darwin/release/bundle/macos/Quip Node Manager.app
src-tauri/target/universal-apple-darwin/release/bundle/dmg/*.dmg
```

## Step 4: Notarize the DMG

Tauri signs the DMG but does not notarize it. Run:

```bash
./scripts/notarize-dmg.sh dist/quip-node-manager-macos-universal.dmg
```

## Step 5: Verify

A build with no credentials still exits 0 and produces an unsigned
application. Tauri logs `skipping app notarization` and continues. Check the
result rather than trusting the exit code:

```bash
./scripts/verify-macos-signing.sh \
  "src-tauri/target/universal-apple-darwin/release/bundle/macos/Quip Node Manager.app" \
  dist/quip-node-manager-macos-universal.dmg
```

The script fails when the signer is wrong, the hardened runtime is off, the
timestamp is absent, Gatekeeper objects, or either ticket is missing.

## Tauri configuration

`src-tauri/tauri.conf.json` needs no `bundle.macOS` block for this flow. The
environment variables drive everything, and `hardenedRuntime` already defaults
to `true`.

Add a block only for a specific need:

| Key | Use it when |
|---|---|
| `signingIdentity` | you want the build to reject a certificate that does not match |
| `entitlements` | a verification run shows a specific hardened runtime denial |
| `minimumSystemVersion` | the default `10.13` floor is wrong |

## CI setup (GitLab)

`.gitlab-ci.yml`, job `build-macos-universal`. The job sets the Apple
variables only when `$CI_COMMIT_TAG` is set, then calls
`scripts/notarize-dmg.sh` and `scripts/verify-macos-signing.sh`.

The job creates no keychain. `Keychain::with_certificate` inside Tauri creates
a temporary keychain and imports the certificate. It sets the key partition
list, then deletes the keychain when it drops.

### Required CI/CD variables

| Variable | Type | Description |
|---|---|---|
| `APPLE_CERTIFICATE` | Variable | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | Variable | password used during the `.p12` export |
| `APPLE_SIGNING_IDENTITY` | Variable | `Developer ID Application: Richard Carback (W64XX7HSTH)` |
| `APPLE_API_KEY` | Variable | App Store Connect Key ID |
| `APPLE_API_ISSUER` | Variable | App Store Connect Issuer ID |
| `APPLE_API_KEY_FILE` | File | the `.p8` private key |

Store all of these as **masked** and **protected**. Protected variables reach
protected tags only, so confirm the release tag pattern is protected. An
unprotected tag builds with empty credentials and produces an unsigned DMG.

### Why this repository does not target the Mac App Store

The Mac App Store requires App Sandbox. Quip Node Manager runs
`docker compose` and probes for a Docker daemon. It also runs miner binaries
that it downloads at run time. A sandboxed application cannot do any of that.
Notarization removes the Gatekeeper warning, which is the part that affects
users.
