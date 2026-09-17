<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Linux Packaging & Distribution

This guide covers packaging, signing, and distributing the Quip Node Manager
Tauri app on Linux.

## AppImage

AppImage produces a single self-contained executable that runs on most Linux
distributions without installation.

### Building

Tauri generates an AppImage during the build:

```bash
bun run build
# Output: src-tauri/target/release/bundle/appimage/quip-node-manager.AppImage
```

### Verification

The AppImage carries no signature of its own. No Linux tool checks one on a
file a user downloaded, so each release publishes `SHA256SUMS` and a detached
signature over that file instead. See [Release signing](#release-signing).

### Running

```bash
chmod +x quip-node-manager.AppImage
./quip-node-manager.AppImage
```

## .deb Package

Debian/Ubuntu packages for APT-based distribution.

### Building

Tauri generates a `.deb` package during the build:

```bash
bun run build
# Output: src-tauri/target/release/bundle/deb/quip-node-manager_VERSION_amd64.deb
```

### Tauri Configuration

Configure `.deb` metadata in `src-tauri/tauri.conf.json`:

```json
{
  "bundle": {
    "linux": {
      "deb": {
        "depends": ["libwebkit2gtk-4.1-0", "libgtk-3-0", "docker.io"],
        "section": "utils",
        "priority": "optional",
        "desktopTemplate": "assets/quip-node-manager.desktop"
      }
    }
  }
}
```

### GPG-Signed APT Repository

Set up a signed APT repository so users can install and update via `apt`:

1. **Generate a dedicated GPG key for the repository:**

```bash
gpg --full-generate-key
# Choose RSA 4096, no expiration (or set a long expiration)
# Use an identity like "Quip Node Manager Releases <releases@quip.network>"
```

2. **Export the public key:**

```bash
gpg --armor --export "releases@quip.network" > quip-repo.gpg.asc
```

3. **Create the repository structure:**

```bash
mkdir -p repo/pool/main
mkdir -p repo/dists/stable/main/binary-amd64

# Copy the .deb into the pool
cp quip-node-manager_VERSION_amd64.deb repo/pool/main/
```

4. **Generate Packages and Release files:**

```bash
cd repo

# Generate Packages index
dpkg-scanpackages pool/main /dev/null > dists/stable/main/binary-amd64/Packages
gzip -k dists/stable/main/binary-amd64/Packages

# Generate Release file
cd dists/stable
apt-ftparchive release . > Release
```

5. **Sign the Release file:**

```bash
gpg --default-key "releases@quip.network" \
  --armor --detach-sign --output Release.gpg Release

gpg --default-key "releases@quip.network" \
  --clearsign --output InRelease Release
```

6. **User installation:**

```bash
# Add the repository GPG key
curl -fsSL https://releases.quip.network/quip-repo.gpg.asc \
  | sudo gpg --dearmor -o /usr/share/keyrings/quip-archive-keyring.gpg

# Add the repository
echo "deb [signed-by=/usr/share/keyrings/quip-archive-keyring.gpg] \
  https://releases.quip.network/repo stable main" \
  | sudo tee /etc/apt/sources.list.d/quip.list

# Install
sudo apt update
sudo apt install quip-node-manager
```

## .rpm Package

RPM packages for Fedora, RHEL, and openSUSE.

### Building

Tauri generates an `.rpm` package during the build:

```bash
bun run build
# Output: src-tauri/target/release/bundle/rpm/quip-node-manager-VERSION.x86_64.rpm
```

### RPM signing

CI signs the published RPM in the `sign-artifacts` job. `rpm --checksig` then
verifies it, once the user imports the public key:

```bash
sudo rpm --import https://gitlab.com/quip.network/quip-node-manager/-/raw/main/.gitlab/release-signing-key.asc
rpm --checksig quip-node-manager-linux-x86_64.rpm
```

A locally built RPM is unsigned. Sign one by hand only to reproduce a problem,
and never with the release key.

## Release signing

Each release publishes `SHA256SUMS` over every artifact, plus `SHA256SUMS.asc`,
a detached signature made with the release key.

| Item | Value |
|------|-------|
| Key | `Quip Node Manager Release Signing <rick@postquant.xyz>` |
| Fingerprint | `A63860E21E7070C2C26FDA5DC85BEAB01AD9FEE3` |
| Type | RSA 4096, expires 2029-09-16 |
| Public key | `.gitlab/release-signing-key.asc` in this repository |

RSA rather than Ed25519, because `rpm` on older RHEL releases cannot verify an
EdDSA signature.

`scripts/install.sh` checks the checksum on every run. It also checks the
signature when `gpg` is installed, against the fingerprint pinned in the
script. macOS ships no `gpg`, so a Mac without GnuPG gets the checksum check
and a message that the signature was skipped.

To verify by hand:

```bash
curl -fsSLO https://gitlab.com/quip.network/quip-node-manager/-/raw/main/.gitlab/release-signing-key.asc
gpg --import release-signing-key.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

The private key lives in two places only: the `RELEASE_GPG_PRIVATE_KEY` and
`RELEASE_GPG_PASSPHRASE` CI variables, which are protected and file type, and
the `RICK_PQ_PGP` item in the shared password vault, which also holds the
revocation certificate.

## Systemd Service (Headless Deployment)

For servers or headless systems running only the Quip network node (not the
GUI), use a systemd service:

```ini
# /etc/systemd/system/quip-node.service
[Unit]
Description=Quip Network Node
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service

[Service]
Type=simple
User=quip
Group=quip
WorkingDirectory=/home/quip
ExecStart=/usr/bin/quip-network-node
Restart=on-failure
RestartSec=10
TimeoutStopSec=30

# Hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/home/quip/quip-data
PrivateTmp=yes

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=quip-node

[Install]
WantedBy=multi-user.target
```

Enable and start the service:

```bash
sudo useradd --system --create-home quip
sudo systemctl daemon-reload
sudo systemctl enable --now quip-node.service
sudo journalctl -u quip-node.service -f
```

## Optional: Flatpak & Snap

For broader distribution through Linux app stores:

- **Flatpak**: Submit to [Flathub](https://flathub.org/).
  See [docs.flathub.org/docs/for-app-authors](https://docs.flathub.org/docs/for-app-authors/)
  for submission guidelines.

- **Snap**: Submit to the [Snap Store](https://snapcraft.io/).
  See [snapcraft.io/docs](https://snapcraft.io/docs) for packaging
  instructions with `snapcraft.yaml`.

Both formats provide sandboxing, automatic updates, and cross-distribution
compatibility.
