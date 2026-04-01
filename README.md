# Quip Node Manager

Desktop application for running and monitoring [Quip](https://gitlab.com/quip.network) network nodes. Supports Docker and native execution modes on macOS, Linux, and Windows.

## Quick Install

**macOS / Linux:**

```sh
curl -fsSL https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.ps1 | iex
```

## Manual Download

Download the latest release from the [Releases page](https://gitlab.com/quip.network/quip-node-manager/-/releases).

### macOS

Download the `.dmg`, open it, and drag the app to `/Applications`.

Because the app is not yet notarized, macOS will quarantine it. Open **Terminal** (Applications > Utilities > Terminal) and paste:

```sh
xattr -dr com.apple.quarantine /Applications/Quip\ Node\ Manager.app
```

Then launch the app from `/Applications`, not from the `.dmg` or Downloads folder.

### Linux

The recommended format is **AppImage** (works on any distro):

```sh
chmod +x quip-node-manager-linux-x86_64.AppImage
./quip-node-manager-linux-x86_64.AppImage
```

A `.deb` package is also available for Debian/Ubuntu:

```sh
sudo dpkg -i quip-node-manager-linux-x86_64.deb
```

### Windows

Download the `.exe` and run it. Windows SmartScreen may show a warning because the binary is not yet code-signed.

Click **More info**, then **Run anyway**.

## Features

- **Two run modes** -- Docker (default on Windows/Linux) pulls and manages a container image; Native (default on macOS) downloads a standalone binary
- **Pre-flight checklist** -- verifies Docker/binary availability, node secret, public IP, port forwarding, and firewall before starting
- **Live log streaming** -- tails node stdout/stderr in a collapsible drawer
- **GPU configuration** -- detects CUDA and Metal devices, per-device enable/disable, utilization slider, yielding mode
- **D-Wave QPU support** -- optional quantum processing unit configuration with daily budget controls
- **Background update monitor** -- checks for new Docker images, native binaries, and app releases every 30 minutes; optional auto-restart on image updates
- **TLS certificate guidance** -- built-in walkthrough for Let's Encrypt and self-signed certificate setup

## Development

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Bun](https://bun.sh/) (or Node.js)
- Platform-specific Tauri v2 dependencies ([see Tauri docs](https://v2.tauri.app/start/prerequisites/))

### Commands

```sh
bun install          # Install JS dependencies
bun run dev          # Launch development build
bun run build        # Production build for current platform
```

```sh
cd src-tauri
cargo check          # Type-check Rust code
cargo clippy         # Lint
```

### CLI Mode

The app also supports a terminal UI mode:

```sh
quip-node-manager --cli
```

## Architecture

- **Frontend**: `src/` -- vanilla HTML/CSS/JS with Tauri IPC (`withGlobalTauri: true`)
- **Backend**: `src-tauri/src/` -- Rust + Tauri v2 commands
- **Config**: TOML generation matching quip-protocol format
- **Data**: `~/quip-data/` stores settings, config, secrets, binaries, and trust database

See [AGENTS.md](AGENTS.md) for detailed architecture documentation.

## License

[AGPL-3.0-or-later](LICENSE)

Copyright (c) Postquant Labs
