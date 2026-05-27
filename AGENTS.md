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
│                                  # env.example). Embedded into the binary
│                                  # via include_str! in stack_assets.rs at
│                                  # compile time (NOT Tauri's bundle.resources),
│                                  # then staged + patched into ~/quip-data on
│                                  # every Start. See "Stack Asset Patching".
└── src-tauri/                     # Rust backend (Tauri v2)
    ├── Cargo.toml
    ├── tauri.conf.json            # no bundle.resources — resources are
    │                              # compile-time embedded
    ├── capabilities/
    │   └── default.json
    └── src/
        ├── main.rs                # Entry point
        ├── lib.rs                 # Tauri builder, command registration
        ├── settings.rs            # AppSettings, ImageTag, StackStatus
        ├── secret.rs              # Node secret (64-char hex)
        ├── config.rs              # config.toml generation
        ├── compose.rs             # docker compose orchestration (the stack)
        ├── stack_assets.rs        # include_str! the compose.yml + Caddyfile;
        │                          # patch host port + Native upstream at stage time
        ├── log_stream.rs          # docker compose logs -f → Tauri events
        ├── native.rs              # native binary download + lifecycle
        ├── hardware.rs            # GPU/Docker/Python detection
        ├── network.rs             # Public IP detection only
        ├── update.rs              # Multi-image + app update monitor
        └── checklist.rs           # Pre-flight checks → checklist-update events;
                                   # also owns the port-reachability probe
```

## Key Details

- **Tauri version**: v2
- **JS tooling**: Bun
- **App version**: v0.1.5
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
  - `<settings.node_config.port>:20049/udp+tcp` — node QUIC peer-to-peer.
    The container always binds 20049 internally; the host side is
    rewritten at stage time to whatever the user configured. See
    "Port Handling" below.
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
| `docker-compose.yml` | stack_assets.rs (embedded copy + patch) | Upstream compose file with host port rewritten to `<settings.node_config.port>:20049` |
| `caddy/Caddyfile` | stack_assets.rs (embedded copy + patch in Native) | Caddy routes; in Native mode `/api/v1/*` is rewritten from `quip-node:80` to `host.docker.internal:<rest_port>` |
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

## Stack Asset Patching

`vendor/nodes.quip.network/docker-compose.yml` and `caddy/Caddyfile` are
**embedded into the binary at compile time** via `include_str!`. This
avoids Tauri's runtime resource resolution entirely — on Windows, the CI
ships a raw `.exe` (`tauri build --no-bundle`) with no sibling resource
folder, and a `BaseDirectory::Resource` lookup would fail. Embedding
makes the staged files travel as `&'static str` in `.rodata`.

`stack_assets::sync_stack_assets(run_mode, public_port, native_rest_port)`
is called from both `start_stack` and `pull_compose_images` before any
`docker compose` invocation. It always overwrites the staged files —
no merge with prior state. Two runtime patches are applied:

1. **compose.yml port remap** (always): the upstream's hardcoded
   `"20049:20049/udp"` and `"/tcp"` strings are rewritten to
   `"<public_port>:20049/<proto>"` across all node services (cpu/cuda/qpu).
   Container-internal port stays 20049; only the host side moves.
   No-op when `public_port == 20049`.

2. **Caddyfile upstream rewrite** (Native mode only): `quip-node:80`
   becomes `host.docker.internal:<native_rest_port>` so the dashboard
   container can reach the host-bound binary. Docker mode writes the
   Caddyfile verbatim.

Why embedded + patched at stage time (instead of using compose's `${VAR}`
env substitution): compose env substitution works for the `DASHBOARD_PORT`
case because the env is loaded via `env_file: .env`, but staging files
with embedded literals matches the Caddyfile patch pattern (which
*requires* a runtime rewrite of a YAML/Caddyfile token, not just an env
var) and keeps both modifications in one place.

## Port Handling

The node has two distinct concepts of "port", and the manager separates
them deliberately:

- **Container-internal bind port** — what the node binary actually
  `bind()`s. In Docker mode this is **always 20049**, set by
  `render_config_toml` in `config.rs`. The user cannot change it.
- **Host-published port** — what the router forwards to and what other
  peers see. This is `settings.node_config.port`, configurable from the
  UI. In Docker mode it's the host side of the compose mapping; in
  Native mode (no container layer) it's the same as the bind port.

In Docker mode, when these differ, `config.toml` also emits
`public_port = <settings.node_config.port>` so the node's peer-discovery
layer announces the correct external port. The decoupling fixes a class
of bugs where changing the port in the UI updated `config.toml` but
left compose publishing 20049 — leaving the published host port empty
and the configured internal port unreachable from the outside.

| Mode   | config.toml `port` | config.toml `public_port` | compose published port |
|--------|--------------------|---------------------------|------------------------|
| Docker | 20049 (hardcoded)  | user's port (if ≠ 20049)  | `<user_port>:20049`    |
| Native | user's port        | user's setting if any      | (no container)         |

## Pre-flight Port Reachability Check

`run_check_port` in `checklist.rs` answers a single question: *is this
host's `<settings.node_config.port>` reachable from the public internet?*
It uses `check.quip.network` (see `openapi.yaml` for the spec) and runs
**one probe per recheck**, chosen by local socket state:

- **Port already bound locally** (we can't `TcpListener::bind` it): node
  is running. Call `/checkconn?port=N`. The service performs a full
  QUIC handshake with ALPN `quip-v1` and waits for a `STATUS_RESPONSE`
  datagram. This is the authoritative end-to-end verification.
- **Port free locally**: node isn't running. Bind a temp TCP listener,
  hold it for the duration of `/checkport?port=N` (background accept
  loop, aborted on return), so the external probe has something to
  accept into. Verifies the router forward without requiring the node.

Why not both probes? `/checkconn` requires a real QUIP speaker on our
end, so it can't succeed without the node. `/checkport` adds nothing
when the node is already up (its TCP socket answers either way). One
probe per recheck halves load on `check.quip.network` and removes the
"TCP passes but QUIC failed for unknown reasons" ambiguity. Users
click Recheck after starting the node to escalate `ForwardReady` →
`Verified`.

### Response Classification

Probe responses are classified into `ProbeOutcome` with these rules:

| Service response | Outcome | Rationale |
|------------------|---------|-----------|
| HTTP 200, `quip:true` / `reachable:true` | `HostResponded` | success |
| HTTP 200, `..:false` with `error` matching `is_connect_timeout()` (timeout / unreachable / no route) | `Timeout` | no host-level reply |
| HTTP 200, `..:false` with any other error (ALPN mismatch, TLS error, "connection refused", banner timeout, ...) | `HostResponded` | host *did* respond; router forward works |
| HTTP 429 | `RateLimited(retry_after_seconds)` | service rate-limited us |
| HTTP 5xx / network error / malformed body | `ServiceError` | not the user's fault |

`PortProbeResult` maps these to four user-facing states:

- `Verified` (Pass) — port bound + `HostResponded` from `/checkconn`
- `ForwardReady` (Pass) — port free + `HostResponded` from `/checkport`
- `QuicHandshakeFailed` (Warn) — port bound + `Timeout` from `/checkconn`
- `Unreachable` (Warn) — port free + `Timeout` from `/checkport`
- `RateLimited` (Pass with detail) — service rate-limited
- `ServiceError` collapses to `Verified` / `ForwardReady` (lenient-pass)

**Design rule:** *any response from the host means the router forward is
working; only a connect-level timeout fails the check.* ALPN mismatches,
TLS errors, connection resets, and the like are all classified as
passing — they prove the network path works, even if the protocol
handshake didn't.

### Probe Diagnostics

Every probe call emits a `[probe]` line to the `node-log` event with the
full request URL, HTTP status, and response body (truncated at 1 KB).
Users can copy/paste the raw output into support threads — `is_connect_timeout`
might misclassify a future error string, and the raw body is the ground
truth. The `AppHandle` is plumbed via `Option<AppHandle>` on `CheckCtx`,
so non-Tauri callers (the TUI) probe silently.

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
