<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# macOS Code Signing & Distribution

This guide covers code signing, notarization, and distribution of the
Quip Node Manager Tauri app on macOS.

## Prerequisites

- **Apple Developer Program** membership ($99/year) --
  [developer.apple.com/programs](https://developer.apple.com/programs/)
- **Xcode CLI tools** installed: `xcode-select --install`
- An **Apple ID** enrolled in the Developer Program
- An **app-specific password** generated at
  [appleid.apple.com](https://appleid.apple.com/) (under Sign-In and Security
  > App-Specific Passwords)

## Step 1: Create a Developer ID Certificate

1. Open [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates).
2. Click the **+** button to create a new certificate.
3. Select **Developer ID Application** (for distributing outside the App Store).
4. Generate a Certificate Signing Request (CSR) using Keychain Access:
   - Open Keychain Access > Certificate Assistant > Request a Certificate From
     a Certificate Authority.
   - Enter your email, select "Saved to disk", and save the `.certSigningRequest`
     file.
5. Upload the CSR and download the resulting `.cer` file.
6. Double-click the `.cer` file to install it into your login keychain.
7. Verify installation:

```bash
security find-identity -v -p codesigning
# Should list: "Developer ID Application: TEAM_NAME (TEAM_ID)"
```

## Step 2: Build the App

Build the Tauri app for release:

```bash
cd /path/to/quip-node-manager
bun run build
```

The built `.app` bundle is located at:

```
src-tauri/target/release/bundle/macos/Quip Node Manager.app
```

## Step 3: Code Sign the App Bundle

Sign the `.app` bundle with your Developer ID certificate:

```bash
codesign \
  --deep \
  --force \
  --verify \
  --verbose \
  --sign "Developer ID Application: TEAM_NAME (TEAM_ID)" \
  "src-tauri/target/release/bundle/macos/Quip Node Manager.app"
```

Verify the signature:

```bash
codesign --verify --deep --strict --verbose=2 \
  "src-tauri/target/release/bundle/macos/Quip Node Manager.app"

spctl --assess --type execute --verbose \
  "src-tauri/target/release/bundle/macos/Quip Node Manager.app"
```

## Step 4: Create a DMG

Package the signed app into a `.dmg` for distribution:

```bash
hdiutil create -volname "Quip Node Manager" \
  -srcfolder "src-tauri/target/release/bundle/macos/Quip Node Manager.app" \
  -ov -format UDZO \
  "Quip-Node-Manager.dmg"
```

Sign the DMG itself:

```bash
codesign \
  --force \
  --sign "Developer ID Application: TEAM_NAME (TEAM_ID)" \
  "Quip-Node-Manager.dmg"
```

## Step 5: Notarize the DMG

Submit the DMG to Apple for notarization:

```bash
xcrun notarytool submit "Quip-Node-Manager.dmg" \
  --apple-id "your-email@example.com" \
  --team-id "TEAM_ID" \
  --password "APP_SPECIFIC_PASSWORD" \
  --wait
```

The `--wait` flag blocks until notarization completes (typically 2--15 minutes).

Check notarization status if needed:

```bash
xcrun notarytool log <submission-id> \
  --apple-id "your-email@example.com" \
  --team-id "TEAM_ID" \
  --password "APP_SPECIFIC_PASSWORD"
```

## Step 6: Staple the Notarization Ticket

Attach the notarization ticket to the DMG so Gatekeeper can verify it offline:

```bash
xcrun stapler staple "Quip-Node-Manager.dmg"
```

Verify stapling:

```bash
xcrun stapler validate "Quip-Node-Manager.dmg"
```

## Tauri Configuration

Add signing identity settings to `src-tauri/tauri.conf.json`:

```json
{
  "bundle": {
    "macOS": {
      "signingIdentity": "Developer ID Application: TEAM_NAME (TEAM_ID)",
      "providerShortName": "TEAM_ID"
    }
  }
}
```

With this configuration, `bun run build` will automatically sign the app
bundle during the build process.

## Universal Binary (arm64 + x86_64)

To produce a single binary that runs natively on both Apple Silicon and
Intel Macs:

1. Add both Rust targets:

```bash
rustup target add aarch64-apple-darwin
rustup target add x86_64-apple-darwin
```

2. Build for each architecture:

```bash
cd src-tauri

cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
```

3. Combine with `lipo`:

```bash
lipo -create \
  target/aarch64-apple-darwin/release/quip-node-manager \
  target/x86_64-apple-darwin/release/quip-node-manager \
  -output target/release/quip-node-manager-universal
```

4. Re-bundle and sign the universal binary using the steps above.

## CI Setup (GitLab)

To sign builds in CI, import the signing certificate into a temporary
keychain on the macOS runner:

```yaml
build-macos:
  tags: [macos]
  variables:
    KEYCHAIN_NAME: build.keychain
    KEYCHAIN_PASSWORD: $CI_KEYCHAIN_PASSWORD
  before_script:
    # Decode the base64-encoded .p12 certificate from CI variable
    - echo "$MACOS_CERTIFICATE_P12" | base64 --decode > certificate.p12

    # Create a temporary keychain
    - security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_NAME"
    - security default-keychain -s "$KEYCHAIN_NAME"
    - security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_NAME"
    - security set-keychain-settings -t 3600 -u "$KEYCHAIN_NAME"

    # Import the certificate
    - >
      security import certificate.p12
      -k "$KEYCHAIN_NAME"
      -P "$MACOS_CERTIFICATE_PASSWORD"
      -T /usr/bin/codesign
      -T /usr/bin/security

    # Allow codesign to access the keychain without prompting
    - >
      security set-key-partition-list
      -S apple-tool:,apple:
      -s -k "$KEYCHAIN_PASSWORD"
      "$KEYCHAIN_NAME"

    # Verify the identity is available
    - security find-identity -v -p codesigning "$KEYCHAIN_NAME"

  script:
    - bun install
    - bun run build

    # Notarize
    - >
      xcrun notarytool submit
      "src-tauri/target/release/bundle/dmg/Quip Node Manager.dmg"
      --apple-id "$APPLE_ID"
      --team-id "$APPLE_TEAM_ID"
      --password "$APPLE_APP_SPECIFIC_PASSWORD"
      --wait

    # Staple
    - >
      xcrun stapler staple
      "src-tauri/target/release/bundle/dmg/Quip Node Manager.dmg"

  after_script:
    # Clean up the temporary keychain
    - security delete-keychain "$KEYCHAIN_NAME"
    - rm -f certificate.p12

  artifacts:
    paths:
      - src-tauri/target/release/bundle/dmg/*.dmg
    expire_in: 30 days
```

### Required CI/CD Variables

| Variable | Description |
|----------|-------------|
| `MACOS_CERTIFICATE_P12` | Base64-encoded `.p12` export of the Developer ID certificate |
| `MACOS_CERTIFICATE_PASSWORD` | Password for the `.p12` file |
| `CI_KEYCHAIN_PASSWORD` | Arbitrary password for the temporary CI keychain |
| `APPLE_ID` | Apple ID email address |
| `APPLE_TEAM_ID` | 10-character Team ID from Apple Developer portal |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password for notarytool |

Store all of these as **masked, protected** CI/CD variables.
