<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Windows Code Signing & Distribution

This guide covers code signing, installer configuration, and distribution of
the Quip Node Manager Tauri app on Windows.

## Code Signing Certificates

Windows code signing requires a certificate from a trusted Certificate
Authority (CA). There are two types:

### OV (Organization Validation) Code Signing Certificate

- **Cost**: $200--400/year
- **Providers**: DigiCert, Sectigo, GlobalSign
- **Validation**: Verifies the organization's legal identity
- **SmartScreen**: Reputation builds gradually over time with download volume
- **Form factor**: Software-based (`.pfx` file) or hardware token

### EV (Extended Validation) Code Signing Certificate

- **Cost**: $300--600/year
- **Providers**: DigiCert, Sectigo, GlobalSign
- **Validation**: Stricter organization verification
- **SmartScreen**: Immediate trust -- no reputation building period
- **Form factor**: Hardware token required (USB key or cloud HSM)

**Recommendation**: Start with an EV certificate to avoid SmartScreen warnings
from day one. The immediate trust is worth the higher cost for user-facing
software.

## Signing the Installer

Use Microsoft's `signtool` (included with the Windows SDK) to sign the
installer executable:

```powershell
signtool sign `
  /fd SHA256 `
  /tr http://timestamp.digicert.com `
  /td SHA256 `
  /f cert.pfx `
  /p PASSWORD `
  "src-tauri\target\release\bundle\nsis\Quip Node Manager Setup.exe"
```

Parameter reference:

| Flag | Purpose |
|------|---------|
| `/fd SHA256` | File digest algorithm |
| `/tr http://timestamp.digicert.com` | RFC 3161 timestamp server URL |
| `/td SHA256` | Timestamp digest algorithm |
| `/f cert.pfx` | Path to the certificate file |
| `/p PASSWORD` | Certificate password |

### Signing with an EV Token (SafeNet/Hardware)

EV certificates on hardware tokens use a different invocation:

```powershell
signtool sign `
  /fd SHA256 `
  /tr http://timestamp.digicert.com `
  /td SHA256 `
  /sha1 CERTIFICATE_THUMBPRINT `
  "src-tauri\target\release\bundle\nsis\Quip Node Manager Setup.exe"
```

The `/sha1` flag selects the certificate by thumbprint from the Windows
certificate store (where the hardware token's certificate is registered).

### Verifying the Signature

```powershell
signtool verify /pa /v "Quip Node Manager Setup.exe"
```

## Tauri Configuration

Add signing settings to `src-tauri/tauri.conf.json`:

```json
{
  "bundle": {
    "windows": {
      "certificateThumbprint": "CERTIFICATE_THUMBPRINT_HEX",
      "digestAlgorithm": "sha256",
      "timestampUrl": "http://timestamp.digicert.com"
    }
  }
}
```

With this configuration, `bun run build` will automatically sign the NSIS
installer during the build process. The certificate must be available in the
Windows certificate store (either imported from `.pfx` or present on a
connected hardware token).

## SmartScreen Reputation

Windows SmartScreen protects users from unknown software:

| Certificate Type | SmartScreen Behavior |
|------------------|----------------------|
| **EV** | Immediate trust. No warnings from the first download. |
| **OV** | Warnings shown until enough users download and run the software. Reputation builds over weeks to months. |
| **None** | "Windows protected your PC" blocking dialog. Most users will not proceed. |

## NSIS Installer

Tauri uses [NSIS](https://nsis.sourceforge.io/) as the default Windows
installer framework. It produces a single `.exe` setup file that handles:

- Installation directory selection
- Start menu shortcuts
- Uninstaller registration in Add/Remove Programs
- Optional desktop shortcut

No additional NSIS configuration is needed beyond Tauri's defaults unless
custom installer pages are required.

The built installer is located at:

```
src-tauri/target/release/bundle/nsis/Quip Node Manager Setup.exe
```

## CI Setup (GitLab)

Sign builds in CI using a certificate stored as a CI/CD variable:

```yaml
build-windows:
  tags: [windows]
  variables:
    SIGNTOOL_PATH: "C:\\Program Files (x86)\\Windows Kits\\10\\bin\\10.0.22621.0\\x64\\signtool.exe"
  before_script:
    # Decode the base64-encoded .pfx from CI variable
    - >
      [System.IO.File]::WriteAllBytes(
        "cert.pfx",
        [System.Convert]::FromBase64String($env:WINDOWS_CERTIFICATE_PFX)
      )

    # Import into the certificate store
    - >
      Import-PfxCertificate
      -FilePath cert.pfx
      -CertStoreLocation Cert:\CurrentUser\My
      -Password (ConvertTo-SecureString -String $env:WINDOWS_CERTIFICATE_PASSWORD -AsPlainText -Force)

  script:
    - bun install
    - bun run build

    # Sign the installer
    - >
      & $env:SIGNTOOL_PATH sign
      /fd SHA256
      /tr http://timestamp.digicert.com
      /td SHA256
      /sha1 $env:WINDOWS_CERTIFICATE_THUMBPRINT
      "src-tauri\target\release\bundle\nsis\Quip Node Manager Setup.exe"

    # Verify
    - >
      & $env:SIGNTOOL_PATH verify /pa /v
      "src-tauri\target\release\bundle\nsis\Quip Node Manager Setup.exe"

  after_script:
    - Remove-Item cert.pfx -ErrorAction SilentlyContinue

  artifacts:
    paths:
      - src-tauri/target/release/bundle/nsis/*.exe
    expire_in: 30 days
```

### Required CI/CD Variables

| Variable | Description |
|----------|-------------|
| `WINDOWS_CERTIFICATE_PFX` | Base64-encoded `.pfx` certificate file |
| `WINDOWS_CERTIFICATE_PASSWORD` | Password for the `.pfx` file |
| `WINDOWS_CERTIFICATE_THUMBPRINT` | SHA-1 thumbprint of the certificate |

Store all of these as **masked, protected** CI/CD variables.

## Optional: Microsoft Store via MSIX

For distribution through the Microsoft Store:

1. Register as a Microsoft developer ($19 one-time for individuals,
   $99 for organizations) at
   [developer.microsoft.com](https://developer.microsoft.com/).

2. Tauri supports MSIX packaging -- see the
   [Tauri MSIX documentation](https://v2.tauri.app/distribute/windows-store/).

3. MSIX packages use a separate signing flow managed by the Microsoft Store
   submission process; no external code signing certificate is required for
   Store-distributed builds.

The Microsoft Store provides automatic updates, sandboxing, and visibility
to Windows users who prefer installing from the Store.
