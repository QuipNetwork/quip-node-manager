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

### GPG Signing

Sign the AppImage with a detached GPG signature so users can verify
authenticity:

```bash
# Generate a GPG key if you don't have one
gpg --full-generate-key

# Create a detached signature
gpg --detach-sign --armor \
  src-tauri/target/release/bundle/appimage/quip-node-manager.AppImage

# Output: quip-node-manager.AppImage.asc
```

Users verify the signature with:

```bash
gpg --verify quip-node-manager.AppImage.asc quip-node-manager.AppImage
```

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

### RPM Signing

1. **Import your GPG key into RPM:**

```bash
# Export the public key
gpg --armor --export "releases@quip.network" > RPM-GPG-KEY-quip

# Configure ~/.rpmmacros
cat >> ~/.rpmmacros <<'EOF'
%_gpg_name releases@quip.network
%_gpg_path ~/.gnupg
%__gpg /usr/bin/gpg
EOF
```

2. **Sign the RPM:**

```bash
rpm --addsign quip-node-manager-VERSION.x86_64.rpm
```

3. **User verification:**

```bash
rpm --import RPM-GPG-KEY-quip
rpm --checksig quip-node-manager-VERSION.x86_64.rpm
```

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
