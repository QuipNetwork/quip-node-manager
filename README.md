# Quip Node Manager

Desktop application for running and monitoring [Quip](https://gitlab.com/quip.network) network nodes. Supports Docker and native execution modes on macOS, Linux, and Windows.

## Quick Install

**macOS / Linux:**

```sh
curl -fsSL https://gitlab.com/quip.network/quip-node-manager/-/raw/v0.2.0/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://gitlab.com/quip.network/quip-node-manager/-/raw/v0.2.0/scripts/install.ps1 | iex
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

- **Full compose stack** -- runs miner + validator + dashboard + postgres behind a Caddy front door via Docker Compose; dashboard UI is embedded in the app's Dashboard tab
- **Two run modes** -- Docker (default on Windows/Linux) drives the full container stack; Native (macOS) runs a standalone binary on the host and still runs the validator/dashboard/postgres/caddy containers, wired to the host via `host.docker.internal`
- **CPU or CUDA image** -- chosen automatically from the GPU device toggles (CUDA whenever an NVIDIA device is enabled); D-Wave mining runs on the CPU image via the `[dwave]` config section
- **TLS toggle** -- the dashboard (Postgres-backed telemetry UI) and Caddy front door are always part of the stack; TLS is optional, with automatic Let's Encrypt or ZeroSSL certificates
- **Pre-flight checklist** -- verifies Docker + Compose v2 availability (plus WSL on Windows, the miner binary in Native mode, and the D-Wave token when QPU mining is configured), node secret, public IP, hostname, and external port reachability before starting (images aren't pre-checked -- Start always pulls them)
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

This repo vendors the compose stack via a git submodule. After cloning, fetch the
submodule at the commit locked by this repo:

```sh
make fetch-submodules
```

(Equivalent to `git submodule update --init --recursive`, or clone with
`git clone --recurse-submodules` to do this in one step.)

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
- **Stack definition**: `vendor/nodes.quip.network/` -- git submodule tracking the upstream Docker Compose setup (miner + validator + dashboard + postgres + caddy).
- **Config**: TOML generation matching quip-protocol format; `.env` generated from settings on every Start.
- **Data**: `~/quip-data/` (configurable at first boot) holds app settings, runtime config, secrets, native binaries, and the staged compose files.

> **Native mode note (macOS):** the node's REST API binds to `127.0.0.1:20100`. Docker Desktop's vpnkit forwards container traffic from `host.docker.internal` through to the host's loopback, so the REST port is not exposed to the LAN.

### Overriding the bundled stack

Node Manager rewrites `~/quip-data/docker-compose.yml` from its embedded copy on every Start and Apply, discarding whatever you edited there. Put changes in `~/quip-data/docker-compose.override.yml` instead. Staging never touches that file, and compose merges it over the base.

```yaml
services:
  quip-validator:
    environment:
      RUST_LOG: debug
```

Compose loads an override file automatically only when it discovers the compose file itself. Node Manager always passes `-f`, so it adds the override explicitly when the file exists, after the base file so the override wins.

Two things to watch:

- `command:` replaces the whole list rather than merging it. To change one validator flag, copy the entire `command:` block and edit the line you want.
- Copy that block from the staged `~/quip-data/docker-compose.yml`, not from `vendor/nodes.quip.network/`. Staging adds flags the vendored file doesn't contain, such as `--public-addr`.

> **Validator pruning isn't recommended.** The validator runs `--state-pruning=archive --blocks-pruning=archive`, keeping every state trie and block body from genesis. Size the disk for that. Pruning reclaims disk, but it breaks the dashboard: the descriptor worker scans from genesis and fails with `State already discarded` once it reads past the pruning window. Pruning also likely reduces your point awards on the SNAG platform. Only prune if you accept losing the dashboard.

See [AGENTS.md](AGENTS.md) for detailed architecture documentation.

## License

[AGPL-3.0-or-later](LICENSE)

Copyright (c) Postquant Labs
