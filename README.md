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

- **Full compose stack** -- runs node + dashboard + postgres (+ optional Caddy for TLS) via Docker Compose; dashboard UI is embedded in the app's Dashboard tab
- **Two run modes** -- Docker (default on Windows/Linux) drives the full container stack; Native (macOS) runs a standalone binary on the host and still runs the dashboard/postgres containers, wired to the host via `host.docker.internal`
- **Per-image type** -- CPU, CUDA (NVIDIA GPU), or QPU (D-Wave) — selected from the Stack Configuration panel
- **Dashboard + TLS toggles** -- optional dashboard (Postgres-backed telemetry UI) and optional Caddy reverse proxy with automatic Let's Encrypt certificates
- **Pre-flight checklist** -- verifies Docker + Compose v2 availability, stack asset staging, all stack images, node secret, public IP, port forwarding, and local port conflicts (20080/80/443/native REST) before starting
- **Live log streaming** -- tails `docker compose logs -f <node>` in a collapsible drawer; switches to `data/node.log` once the node writes to it
- **GPU configuration** -- detects CUDA and Metal devices, per-device enable/disable, utilization slider, yielding mode
- **D-Wave QPU support** -- optional quantum processing unit configuration with daily budget controls
- **Background update monitor** -- checks for new node + dashboard image digests and manager app releases every 30 minutes; optional auto-restart on digest change
- **TLS certificate guidance** -- Caddy's ACME (Let's Encrypt or ZeroSSL) is wired up out of the box; set a DNS name + email and TLS "just works"

## Development

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Bun](https://bun.sh/) (or Node.js)
- [Docker Compose v2](https://docs.docker.com/compose/install/) (bundled with Docker Desktop; standalone `docker-compose` v1 is **not** supported)
- Platform-specific Tauri v2 dependencies ([see Tauri docs](https://v2.tauri.app/start/prerequisites/))

### Setup

This repo vendors the compose stack via a git submodule. After cloning:

```sh
git submodule update --init --recursive
```

(Or clone with `git clone --recurse-submodules` to do this in one step.)

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

- **Frontend**: `src/` -- vanilla HTML/CSS/JS with Tauri IPC (`withGlobalTauri: true`). Dashboard tab embeds the running dashboard container in an iframe.
- **Backend**: `src-tauri/src/` -- Rust + Tauri v2 commands. `compose.rs` drives the stack via `docker compose`; `stack_assets.rs` stages the bundled compose files into `~/quip-data/`.
- **Stack definition**: `vendor/nodes.quip.network/` -- git submodule tracking the upstream Docker Compose setup (node + dashboard + postgres + caddy).
- **Config**: TOML generation matching quip-protocol format; `.env` generated from settings on every Start.
- **Data**: `~/quip-data/` holds app settings, runtime config, secrets, binaries, trust database, and the staged compose files.

> **Native mode note (macOS):** when the dashboard is enabled alongside the native binary, the node's REST API binds to `127.0.0.1:20100`. Docker Desktop's vpnkit forwards container traffic from `host.docker.internal` through to the host's loopback, so the REST port is not exposed to the LAN.

See [AGENTS.md](AGENTS.md) for detailed architecture documentation.

## License

[AGPL-3.0-or-later](LICENSE)

Copyright (c) Postquant Labs
