# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex, Cursor, etc.).

## Project Overview

Quip Node Desktop Manager — a Tauri v2 desktop app that orchestrates and monitors a Quip network
node running in Docker. Rust backend + vanilla HTML/CSS/JS frontend.

## Architecture

```
quip-node-manager/
├── src/                           # Frontend (vanilla HTML/CSS/JS)
│   ├── index.html
│   ├── styles.css
│   └── app.js
├── vendor/
│   └── nodes.quip.network/        # git submodule — upstream compose stack
│                                  # (docker-compose.yml, caddy/Caddyfile,
│                                  # env.example). Bundled as Tauri resources
│                                  # and staged into ~/quip-data on Start.
└── src-tauri/                     # Rust backend (Tauri v2)
    ├── Cargo.toml
    ├── tauri.conf.json            # bundle.resources exposes the submodule
    ├── capabilities/
    │   └── default.json
    └── src/
        ├── main.rs                # Entry point
        ├── lib.rs                 # Tauri builder, command registration
        ├── settings.rs            # AppSettings, ImageTag, StackStatus
        ├── secret.rs              # Node secret (64-char hex)
        ├── config.rs              # config.toml generation
        ├── compose.rs             # docker compose orchestration (the stack)
        ├── stack_assets.rs        # stage compose.yml + Caddyfile to data dir
        ├── log_stream.rs          # docker compose logs -f → Tauri events
        ├── native.rs              # native binary download + lifecycle
        ├── hardware.rs            # GPU/Docker/Python detection
        ├── network.rs             # Public IP + port forwarding check
        ├── update.rs              # Multi-image + app update monitor
        └── checklist.rs           # Pre-flight checks → checklist-update events
```

## Key Details

- **Tauri version**: v2
- **JS tooling**: Bun
- **App version**: v0.0.0
- **Window size**: 900×700
- **Data directory**: `~/quip-data/` (bind-mount root for the compose stack)
- **Compose project name**: `quip` (→ `docker compose --project-name quip …`)
- **Compose command**: always via the `docker compose` (v2) CLI; not
  `docker-compose` (v1), not the Python bindings.
- **Container names** (from compose `container_name`): `quip-cpu` or
  `quip-cuda` (node, chosen by GPU presence), `quip-dashboard`,
  `quip-postgres`, `quip-caddy`. The dashboard reaches the active node
  via the compose network alias `quip-node`. (The upstream compose also
  defines a `qpu` service, but we never start it — QPU mining activates
  on top of the CPU image via `config.toml [dwave]`.)
- **Ports**:
  - `20049/udp+tcp` — node QUIC peer-to-peer (always published)
  - `20080/tcp` — dashboard (either directly via `dashboard-direct` service
    in no-TLS, or fronted by Caddy in TLS)
  - `80/tcp + 443/tcp` — Caddy (TLS only)
  - `<native_rest_port>/tcp` — native node REST (default 20100, bound to
    `127.0.0.1`). Docker Desktop's vpnkit forwards container connections
    to `host.docker.internal` through to the host's loopback, so no
    external exposure is needed. macOS/Windows only; the Linux Docker CE
    bridge would need a different bind strategy.

## Docker Images

Images are declared in `vendor/nodes.quip.network/docker-compose.yml`:

- Node (CPU / QPU): `registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu:latest`
- Node (CUDA): `registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cuda:latest`
- Dashboard: `registry.gitlab.com/quip.network/dashboard.quip.network:latest`
- Postgres: `postgres:16` (Docker Hub)
- Caddy: `caddy:2-alpine` (Docker Hub)

Selected by `AppSettings`:
- `image_tag: ImageTag` — `Cpu` | `Cuda` | `Qpu` (QPU uses the CPU image,
  distinguished only by the `[dwave]` section in `config.toml`)
- `dashboard_enabled: bool` — pulls dashboard + postgres
- `tls_enabled: bool` — pulls caddy and binds :80/:443

## Run Modes

| run_mode | node | compose services run |
|----------|------|----------------------|
| `Docker` | `quip-{cpu,cuda,qpu}` container via compose | `dashboard`+`postgres`+`caddy` (per profile) |
| `Native` (macOS only) | native binary on the host (`~/quip-data/bin/…`) | `dashboard`+`postgres`+`caddy` — no node container; dashboard reaches the native binary at `host.docker.internal:<rest_port>` |

## Compose Profiles

`(image_tag, dashboard_enabled, tls_enabled) → profile`:

| profile | services |
|---------|----------|
| `cpu` / `cuda` / `qpu` | node + dashboard + postgres + caddy |
| `{cpu,cuda,qpu}-notls` | node + dashboard-direct + postgres |
| `{cpu,cuda,qpu}-nodash` | node only |

In Native mode, `start_stack` passes an explicit service list (omitting
the node service) so `--profile` gates eligibility while positional args
restrict what actually starts.

## Data Files (all in `~/quip-data/`)

| File | Generated / managed by | Purpose |
|------|------------------------|---------|
| `app-settings.json` | settings.rs (user preferences) | UI toggles + NodeConfig |
| `config.toml` | config.rs on every Start | Node config (bind-mounted into the node container in Docker mode; read directly by the binary in Native mode) |
| `.env` | compose.rs on every Start | Compose env (PUID, QUIP_HOSTNAME, CERT_EMAIL, DWAVE_API_KEY, POSTGRES_PASSWORD, QUIP_NODE_URL when Native); mode 0600 on Unix |
| `docker-compose.yml` | stack_assets.rs (copy of bundle) | Upstream compose file |
| `caddy/Caddyfile` | stack_assets.rs (copy, patched in Native) | Caddy routes; in Native mode `/api/v1/*` is rewritten to `host.docker.internal:<rest_port>` |
| `data/` | bind-mount target for the node's `/data` | `node.log`, `trust.db`, `telemetry/`, runtime `config.toml` |
| `dashboard-data/` | bind-mount target for the dashboard | Dashboard auxiliary state |
| `node-secret.json` | secret.rs | `{ "secret": "<64-hex>" }` |
| `bin/quip-network-node-*` | native.rs | Downloaded native binary |

Named Docker volumes (survive `docker compose down` by design):
`quip-pgdata`, `quip-caddy-data`, `quip-caddy-config`.

Bootstrap state at `~/.config/quip-node-manager/bootstrap.json`:
holds a `data_dir` override plus a per-install `postgres_password`
(generated once on first access, never rotated — it's keyed to the stored
Postgres volume hash).

## Shared Types (defined in `settings.rs`)

- `RunMode` — `Docker | Native` (Native is macOS-only)
- `ImageTag` — `Cpu | Cuda | Qpu` (serialised lowercase)
- `GpuBackend` — `Local | Modal | Mps`
- `NodeConfig` — port, secret, peers, GPU/QPU, REST, telemetry, …
- `AppSettings` — `{ node_config, image_tag, dashboard_enabled, tls_enabled,
  dashboard_hostname, cert_email, zerossl_api_key, run_mode,
  auto_update_enabled, … }`
- `StackStatus` — `{ services: Vec<ServiceStatus>, overall: StackHealth }`
- `ServiceStatus` — `{ name, service, running, health, status_text, image }`
- `StackHealth` — `Running | Degraded | Unhealthy | Stopped`

## Frontend IPC

The frontend uses `window.__TAURI__.core.invoke` (`withGlobalTauri: true`).

Events emitted by backend:
- `node-log` → `{ timestamp, level, message }`
- `stack-status` → (empty payload — frontend re-polls `get_stack_status`)
- `checklist-update` → `CheckItem { id, state, label, detail, required,
  fixable, updated_at_ms }`
- `pull-progress` → `{ line }` (one `docker compose pull` output line)
- `pull-started`, `pull-complete`, `stop-started`, `stop-complete` — lifecycle
- `image-update-available` → `{ image, info }` (emitted per image that has
  a new digest)
- `binary-update-available` → native-binary UpdateInfo
- `app-update-available` → node-manager UpdateInfo

Key Tauri commands (lib.rs `invoke_handler`):
- `start_stack` / `stop_stack` / `get_stack_status` / `get_stack_config`
- `pull_compose_images`
- `check_docker_installed` / `check_docker_hello_world` /
  `check_docker_compose_installed`
- `start_native_node` / `stop_native_node` / `get_native_node_status`
- `check_image_update(image_tag)` — node image digest
- `check_dashboard_image_update()` — dashboard image digest

## Commands

```bash
# One-time after clone: pull the compose submodule
git submodule update --init --recursive

# Development
bun run dev

# Production build
bun run build

# Install dependencies
bun install
```

## Code Standards

- All Rust files: `// SPDX-License-Identifier: AGPL-3.0-or-later` header
- All JS files: `// SPDX-License-Identifier: AGPL-3.0-or-later` header
- Tauri commands return `Result<T, String>`
- No relative imports (`..`) in Rust — use `crate::module::Type`
- Line length ≤ 100 chars

## License

AGPL-3.0-or-later. All new source files require the standard license header.
