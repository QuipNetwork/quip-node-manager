<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Windows Code Signing & Distribution

This guide covers code signing, installer configuration, and distribution of
the Quip Node Manager Tauri app on Windows.

## Code Signing Certificates

Windows code signing requires a certificate from a trusted Certificate
Authority (CA). Since 2023 every CA must keep the private key in hardware or
in a cloud HSM, so no certificate type ships as a `.pfx` file.

| Type | Validates | SmartScreen | Cost per year |
|------|-----------|-------------|---------------|
| **IV** (Individual) | A person's identity | Reputation builds over time | $100--200 |
| **OV** (Organization) | The legal entity | Reputation builds over time | $200--400 |
| **EV** (Extended) | The legal entity, stricter | Trusted from the first download | $300--600 |

The project uses an **SSL.com eSigner** certificate. eSigner is SSL.com's
cloud HSM. The key never leaves SSL.com, and signing is an authenticated API
call. A document signing certificate cannot sign code, because Windows
rejects its key usage.

**Recommendation**: use an EV certificate. The immediate SmartScreen trust is
worth the higher cost for user-facing software.

## How CI Signs the Binary

The `sign-windows-x86_64` job in `.gitlab-ci.yml` signs
`dist/quip-node-manager-windows-x86_64.exe` after `build-windows-x86_64`
produces it. The job runs on a Linux runner. eSigner's `CodeSignTool` hashes
the file locally and sends only the hash to SSL.com. The returned Authenticode
signature is then written into the file. No Windows API is involved.

Two scripts do the work:

| Script | Purpose |
|--------|---------|
| `scripts/sign-windows.sh` | Downloads a sha256-pinned CodeSignTool and signs the file in place |
| `scripts/verify-windows-signing.sh` | Reads the signed file with `osslsigncode` and fails the job unless the digest, timestamp, and signer match |

The job runs on every tag. On branches it is a manual job, so a change to the
scripts can be tested without a tag.

### CI/CD Variables

Mask every one except `ESIGNER_ENV`. Protect all of them once they hold the
production values. GitLab withholds protected variables from unprotected
branches, so the manual branch job then fails with `ESIGNER_ENV is not set`.
The sandbox values are public, so they stay unprotected.

| Variable | Description |
|----------|-------------|
| `ESIGNER_ENV` | `TEST` for the SSL.com sandbox, `PROD` for the real service |
| `ESIGNER_USERNAME` | SSL.com account username |
| `ESIGNER_PASSWORD` | SSL.com account password |
| `ESIGNER_CREDENTIAL_ID` | eSigner credential ID of the code signing certificate |
| `ESIGNER_TOTP_SECRET` | TOTP secret shown once during eSigner enrollment |

GitLab refuses to mask a value that contains characters outside its masking
alphabet, such as `#`. A password with such characters must be stored
unmasked, or changed.

### Sandbox and Production

`ESIGNER_ENV=TEST` points CodeSignTool at SSL.com's sandbox. SSL.com
publishes a demo account for it, so the pipeline can run before the real
certificate exists:

| Variable | Sandbox value |
|----------|---------------|
| `ESIGNER_USERNAME` | `esigner_demo` |
| `ESIGNER_PASSWORD` | `esignerDemo#1` |
| `ESIGNER_CREDENTIAL_ID` | Run `CodeSignTool get_credential_ids` with the two values above |
| `ESIGNER_TOTP_SECRET` | `RDXYgV9qju+6/7GnMf1vCbKexXVJmUVr+86Wq/8aIGg=` |

A sandbox signature chains to "SSL.com EV Root Certification Authority RSA R2
- Development". No Windows machine trusts it, so a sandbox-signed binary
shows the same SmartScreen dialog as an unsigned one. The sign job refuses to
run with `TEST` on a release tag. Tags that contain `-rc` are allowed, so a
release candidate can exercise the path.

The sandbox API endpoint chains to a TLS root that the Java runtime does not
carry. `sign-windows.sh` adds that root to a private trust store for the
sandbox only. Production endpoints need nothing.

### Switching to Production

1. Enroll the issued certificate in eSigner (next section).
2. Set `ESIGNER_USERNAME`, `ESIGNER_PASSWORD`, `ESIGNER_CREDENTIAL_ID`, and
   `ESIGNER_TOTP_SECRET` to the real values.
3. Set `EXPECTED_CN_PROD` in `scripts/verify-windows-signing.sh` to the common
   name on the certificate. Windows shows this string as the publisher. The
   verify step fails while it is empty, so a production run cannot pass by
   accident.
4. Set `ESIGNER_ENV` to `PROD`.

## eSigner Enrollment

Enrollment is possible only after SSL.com validates the order and issues the
certificate. In the SSL.com portal:

1. Open **Orders**, find the code signing order, and click **details**.
2. Scroll to **eSigner Cloud Signing Enrollment**.
3. Under **Second factor authentication**, choose **OTP APP**.
4. Enter a **4 digit PIN** and store it.
5. Click **create OTP and issue certificate**.
6. A QR code appears with its **secret** as text. Copy the text into
   `ESIGNER_TOTP_SECRET` now. A page reload hides it, and recovering it means
   a reset in the portal.

The credential ID is on the same order page, or from:

```sh
CodeSignTool get_credential_ids -username=<user> -password=<password>
```

An empty list means the certificate is not yet enrolled, whatever the portal
shows.

## Verifying a Signed Binary on Windows

```powershell
Get-AuthenticodeSignature .\quip-node-manager-windows-x86_64.exe | Format-List
signtool verify /pa /v .\quip-node-manager-windows-x86_64.exe
```

`Status` must read `Valid`, and `SignerCertificate.Subject` must name the
organization on the certificate. A sandbox-signed file reports
`UnknownError` or `NotTrusted`. That is expected.

## SmartScreen Reputation

Windows SmartScreen protects users from unknown software:

| Certificate Type | SmartScreen Behavior |
|------------------|----------------------|
| **EV** | Immediate trust. No warnings from the first download. |
| **OV / IV** | Warnings shown until enough users download and run the software. Reputation builds over weeks to months. |
| **None** | "Windows protected your PC" blocking dialog. Most users will not proceed. |

## Distribution Format

CI ships the bare executable, built with `tauri build --no-bundle`. Tauri can
also produce an [NSIS](https://nsis.sourceforge.io/) installer, a single
`.exe` setup file with an uninstaller, Start menu shortcuts, and Add/Remove
Programs registration. A build with bundling writes it to:

```
src-tauri/target/release/bundle/nsis/Quip Node Manager Setup.exe
```

To ship the installer, add it to the sign job. `CodeSignTool batch_sign`
signs a directory of files with one OTP.

## Optional: Microsoft Store via MSIX

For distribution through the Microsoft Store:

1. Register as a Microsoft developer ($19 one-time for individuals,
   $99 for organizations) at
   [developer.microsoft.com](https://developer.microsoft.com/).

2. Tauri supports MSIX packaging -- see the
   [Tauri MSIX documentation](https://v2.tauri.app/distribute/windows-store/).

3. MSIX packages use a separate signing flow managed by the Microsoft Store
   submission process. No external code signing certificate is required for
   Store-distributed builds.

The Microsoft Store provides automatic updates, sandboxing, and visibility
to Windows users who prefer installing from the Store.
