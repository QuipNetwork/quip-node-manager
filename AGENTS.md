# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex, Cursor, etc.).

## Project Overview

Quip Node Desktop Manager — a Tauri v2 desktop app that orchestrates and monitors the Quip node
stack (miner + validator + bootstrap + dashboard + postgres + caddy). Runs the stack via Docker
Compose, or in Native mode (default on macOS) where the miner binary runs on the host and the
support services run in Docker. The same binary also exposes a headless TUI (`--cli`, or when no
display is available — SSH/headless). Rust backend + vanilla HTML/CSS/JS frontend.

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
│                                  # chain-specs/quip-testnet.json). Embedded into
│                                  # the binary via include_str! in stack_assets.rs
│                                  # at compile time (NOT Tauri's bundle.resources),
│                                  # then staged + patched into ~/quip-data on
│                                  # every Start. See "Stack Asset Patching".
└── src-tauri/                     # Rust backend (Tauri v2)
    ├── Cargo.toml
    ├── tauri.conf.json            # no bundle.resources — resources are
    │                              # compile-time embedded
    ├── capabilities/
    │   └── default.json
    └── src/
        ├── main.rs                # Entry point; GUI by default, TUI when
        │                          # --cli passed or no display (headless/SSH)
        ├── lib.rs                 # Tauri builder, command registration,
        │                          # tray icon, background update monitor
        ├── settings.rs            # AppSettings, NodeConfig, ImageTag (Cpu|Cuda),
        │                          # StackStatus/StackHealth, DwaveConfig
        ├── secret.rs              # Node secret (64-char hex)
        ├── config.rs              # config.toml generation
        ├── cmd.rs                 # Command wrapper: PATH augmentation (login-shell
        │                          # $PATH + known tool dirs) + Windows no-console-flash
        ├── compose.rs             # docker compose orchestration: miner +
        │                          # validator + bootstrap + dashboard + postgres + caddy
        ├── stack_assets.rs        # include_str! the compose.yml + Caddyfile + chain
        │                          # spec; patch ports + Native upstream at stage time
        ├── log_stream.rs          # docker compose logs -f → Tauri events
        ├── native.rs              # native binary download + lifecycle
        ├── hardware.rs            # GPU/Docker/Python detection
        ├── network.rs             # Public IP detection only
        ├── update.rs              # Multi-image + app update monitor
        ├── migration_v2.rs        # v0.1 → v0.2 config/.env migration; backs up
        │                          # old files and promotes hand-edited host/port
        ├── hostnames.rs           # public_host parsing → Caddy hostname +
        │                          # validator libp2p --public-addr multiaddr
        ├── checklist.rs           # Pre-flight checks → checklist-update events;
        │                          # also owns the port-reachability probe
        ├── tui_app.rs             # Headless TUI app state + run loop (ratatui)
        ├── tui_input.rs           # TUI terminal event → Action handling
        └── tui_ui.rs              # TUI ratatui frame rendering
```

## Key Details

- **Tauri version**: v2
- **JS tooling**: Bun
- **App version**: 0.2.0-rc1
- **Window size**: 900×700
- **Data directory**: `~/quip-data/` by default (bind-mount root for the compose
  stack). Overridable via `set_data_dir` → the `data_dir` key in
  `~/.config/quip-node-manager/bootstrap.json`; `~/quip-data` is only the
  fallback when unset.
- **Compose project name**: `quip` (→ `docker compose --project-name quip …`)
- **Compose command**: always via the `docker compose` (v2) CLI; not
  `docker-compose` (v1), not the Python bindings.
- **Container names** (from compose `container_name`): `quip-cpu` or
  `quip-cuda` (miner, chosen by GPU presence), `quip-validator` (Substrate
  block-producing validator), `quip-bootstrap` (one-shot keystore
  registration), `quip-dashboard`, `quip-postgres`, `quip-caddy`. The
  dashboard/Caddy reach the miner via the compose network alias `quip-miner`,
  and the validator via `quip-validator`. D-Wave QPU mining activates on top of
  the CPU image via `config.toml [dwave]` (no separate qpu service). The
  upstream compose also defines an optional `quip-faucet` service behind a
  `faucet` profile, which the manager never starts.
- **Ports** (published by the Caddy + validator services):
  - `<settings.node_config.port>:20049/tcp` — Caddy public API port: dashboard
    SPA, miner `/api/v1/*` REST, and Substrate `/rpc` WebSocket. Container-internal
    port is always 20049; the host side is rewritten at stage time to the user's
    configured port (default 20049). See "Port Handling".
  - `80/tcp + 443/tcp` — Caddy ACME/TLS (always published; TLS only provisioned
    when `QUIP_HOSTNAME` is a real DNS name).
  - `<settings.node_config.validator_port>:30333/tcp + /udp` — validator libp2p
    peering (host default 30333, container 30333 — a 1:1 mapping unless the user
    overrides the host port). Must be reachable from the public internet for
    chain peering.
  - `127.0.0.1:<validator_rpc_port>:9944` (Native mode only) — validator raw
    JSON-RPC published on host loopback (default 9944) so the host-side miner
    connects via `ws://127.0.0.1:9944`.
  - `<native_rest_port>/tcp` — native miner REST (default 20100, bound to
    `127.0.0.1`); the dashboard container reaches it via
    `host.docker.internal:<rest_port>`.

## Docker Images

Images are declared in `vendor/nodes.quip.network/docker-compose.yml` (with
`${QUIP_*_TAG:-v0.2}` placeholders); the manager's authoritative image paths +
tag live in `src-tauri/src/compose.rs` (`CPU_IMAGE`, `CUDA_IMAGE`,
`VALIDATOR_IMAGE`, `DASHBOARD_IMAGE`, `COMPOSE_IMAGE_TAG = "v0.2"`), written into
`.env` as `QUIP_MINER_TAG`/`QUIP_VALIDATOR_TAG`/`QUIP_DASHBOARD_TAG`:

- Miner (CPU): `registry.gitlab.com/quip.network/quip-protocol/quip-miner-cpu:v0.2`
- Miner (CUDA): `registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda:v0.2`
- Validator: `registry.gitlab.com/quip.network/quip-protocol-rs/quip-network-node:v0.2`
- Bootstrap (one-shot keystore registration): reuses the CPU miner image `quip-miner-cpu:v0.2`
- Dashboard: `registry.gitlab.com/quip.network/dashboard.quip.network:v0.2`
- Postgres: `postgres:16` (Docker Hub)
- Caddy: `caddy:2-alpine` (Docker Hub)

Selected by `AppSettings`:
- `image_tag: ImageTag` — `Cpu` | `Cuda`. D-Wave QPU mining is not a separate
  image: it rides on the CPU image via the `[dwave]` section in `config.toml`.
- `tls_enabled: bool` — controls whether Caddy provisions TLS (`:80`/`:443` are
  always published by the caddy service).

The dashboard + postgres + caddy + validator + bootstrap services are always part
of the `cpu`/`cuda` profile — there is no `dashboard_enabled` toggle.

## Run Modes

| run_mode | node | compose services run |
|----------|------|----------------------|
| `Docker` | `quip-{cpu,cuda}` miner container via compose | every profile service: miner + `quip-bootstrap` + `quip-validator` + `dashboard` + `postgres` + `caddy` (empty positional list ⇒ compose starts the whole profile) |
| `Native` (macOS only) | native miner binary on the host (`~/quip-data/bin/quip-miner-*`) | explicit list `quip-validator dashboard postgres caddy` — no miner/bootstrap container. The validator's JSON-RPC (9944) is published on `127.0.0.1:<validator_rpc_port>` so the host miner connects via `ws://127.0.0.1:<validator_rpc_port>`; the dashboard reaches the host miner's REST at `host.docker.internal:<rest_port>` |

## Compose Profiles

`image_tag → profile` (a single profile name, no TLS/dashboard variants):

| profile | services started |
|---------|------------------|
| `cpu` | `cpu` miner + `quip-bootstrap` + `quip-validator` + `dashboard` + `postgres` + `caddy` |
| `cuda` | `cuda` miner + `quip-bootstrap` + `quip-validator` + `dashboard` + `postgres` + `caddy` |

`compose_profile(image_tag)` returns the image's service name (`cpu` or `cuda`) —
there is no `qpu` profile (D-Wave mining rides on the CPU image via
`config.toml [dwave]`), and no `-notls`/`-nodash` variants. Caddy is always in
both profiles. The vendored compose file also defines an opt-in `faucet` profile
(`quip-faucet`), which the manager never selects.

In Native mode, `start_stack` passes an explicit positional service list
(`quip-validator dashboard postgres caddy`) that omits the miner and bootstrap,
so `--profile` gates eligibility while positional args restrict what actually
starts.

## Data Files (all in `~/quip-data/`)

| File | Generated / managed by | Purpose |
|------|------------------------|---------|
| `app-settings.json` | settings.rs (user preferences) | UI toggles + NodeConfig |
| `config.toml` | config.rs on every Start | Node config (bind-mounted into the node container in Docker mode; read directly by the binary in Native mode) |
| `.env` | compose.rs on every Start | Compose env: PUID, PGID, QUIP_HOSTNAME, CERT_EMAIL, ZEROSSL_API_KEY, DWAVE_API_KEY, POSTGRES_PASSWORD, QUIP_MINER_TAG, QUIP_DASHBOARD_TAG, QUIP_VALIDATOR_TAG, QUIP_MINER_CPUSET, VALIDATOR_NAME, QUIP_VALIDATOR_RPC_URLS (always), QUIP_VALIDATORS (Docker only); mode 0600 on Unix. (No QUIP_NODE_URL — removed in v0.2.) |
| `docker-compose.yml` | stack_assets.rs (embedded copy + patch) | Upstream compose with Caddy host API port → `<port>:20049`, validator libp2p → `<validator_port>:30333/tcp+udp`, `--public-addr` injected when `public_host` set, and (Native) validator RPC published on `127.0.0.1:<validator_rpc_port>:9944` |
| `caddy/Caddyfile` | stack_assets.rs (embedded copy + patch) | Caddy routes; the local faucet route is always stripped; in Native mode the `/api/v1/*` upstream is rewritten from `quip-miner:80` to `host.docker.internal:<rest_port>` |
| `chain-specs/quip-testnet.json` | stack_assets.rs (embedded copy) | Quip Testnet chain spec mounted into the validator container |
| `keystore.json` | native.rs (Native mode) | Native miner signer keystore (generated via `quip-miner keygen`) |
| `data/` | bind-mount target for the miner's `/data` (Docker) and host config.toml path (Native) | miner runtime `config.toml`, `keystore.json`; the validator's state lives under `data/validator-data/` (mounted as the validator container's `/data`) |
| `dashboard-data/` | bind-mount target for the dashboard | Dashboard auxiliary state |
| `node-secret.json` | secret.rs | `{ "secret": "<64-hex>" }` — read by secret.rs and gates the `secret` pre-flight check, but NOT written into config.toml in v0.2. The node's actual signing identity is `keystore.json` (Docker `/data/keystore.json`, Native `keystore.json`). |
| `bin/quip-miner-*` | native.rs | Downloaded native miner binary (`quip-miner-macos-arm64` / `-x86_64`). Legacy pre-v0.2 `quip-network-node-*` binaries here are auto-deleted on launch. |

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

`stack_assets::sync_stack_assets(run_mode, public_api_port, validator_port,
public_host, native_rest_port, validator_rpc_port)` is called from both
`start_stack` and `pull_compose_images` before any `docker compose` invocation.
It stages the embedded compose.yml, Caddyfile, and chain spec
(`chain-specs/quip-testnet.json`), always overwriting — no merge. Patches:

1. **compose.yml port remap** (always): Caddy's host-published `"20049:20049"`
   is rewritten to `"<public_api_port>:20049"`, and the validator's
   `"30333:30333/tcp"` and `"30333:30333/udp"` mappings to
   `"<validator_port>:30333/<proto>"`. Container-internal ports (20049, 30333)
   stay fixed; only host sides move. No-op when the configured ports equal the
   upstream defaults.

2. **compose.yml `--public-addr`** (when `public_host` is set): a
   `--public-addr=<multiaddr>` arg (built from `public_host` + `validator_port`,
   e.g. `/dns4/host/tcp/30333` or `/ip4/.../tcp/30333`) is inserted into the
   validator command after `--validator`.

3. **compose.yml validator RPC publish** (Native mode only): a
   `"127.0.0.1:<validator_rpc_port>:9944"` mapping is inserted into the
   validator's ports list so the host-side miner reaches the raw JSON-RPC
   directly. Docker mode does not publish it.

4. **Caddyfile faucet strip** (always): the optional local faucet route block is
   removed (the manager relies on the public testnet faucet).

5. **Caddyfile upstream rewrite** (Native mode only): `quip-miner:80` becomes
   `host.docker.internal:<native_rest_port>` so the dashboard container reaches
   the host miner. Docker mode keeps `quip-miner:80`.

Why embedded + patched at stage time (instead of compose's `${VAR}` env
substitution): the Caddyfile upstream rewrite and the validator `--public-addr`
arg both require rewriting a YAML/Caddyfile token, not just supplying an env
var, so all the port/host remaps live in one patch pass for consistency.

## Port Handling

Every container-internal port is fixed; only the host side is remapped at stage
time (in `stack_assets`). v0.2 has three independently host-mappable
validator/API ports:

| setting (`NodeConfig`) | container port | host default | published as |
|------------------------|----------------|--------------|--------------|
| `port` | Caddy 20049 (public API) | 20049 | `<port>:20049` |
| `validator_port` | validator libp2p 30333 | 30333 | `<validator_port>:30333/tcp+udp` |
| `validator_rpc_port` | validator JSON-RPC 9944 | 9944 | Native only: `127.0.0.1:<validator_rpc_port>:9944` |

For the miner's own `config.toml`: `config.rs` emits `public_port` verbatim from
`config.public_port` (`Option<u16>`, skipped when `None`) in both modes — there
is no Docker-side hardcoded `port = 20049` in the miner config. The Docker miner
config carries `rest_port = 80` (container-internal REST) and Native carries
`rest_port = <native_rest_port>` (default 20100, loopback-only).

## Pre-flight Port Reachability Check

`run_check_port` (public API port) and `run_check_port_validator` (validator
libp2p port) in `checklist.rs` each answer: *is this port reachable from the
public internet?* Both call `probe_port_forwarding_with_ctx`, which runs **one
`/checkport?port=N` TCP probe per recheck** against `check.quip.network`
(`CHECK_SERVICE` in checklist.rs). The probe is the same for both ports; only
the local-socket branch differs:

- **Port already bound locally** (`TcpListener::bind` fails): a service is
  already holding the port. Probe it directly — a `HostResponded` result maps to
  `Verified`.
- **Port free locally**: bind a temporary TCP listener and hold it for the
  duration of the probe (background accept loop, aborted on return), so the
  external probe has something to accept into — `HostResponded` maps to
  `ForwardReady`.

There is no `/checkconn`/QUIC endpoint; the manager never speaks QUIP itself.
Both states use `/checkport` over TCP. Users click Recheck after starting the
node to escalate `ForwardReady` → `Verified`.

### Response Classification

Probe responses are classified into `ProbeOutcome` with these rules:

| Service response (`/checkport`) | `ProbeOutcome` | Rationale |
|---------------------------------|----------------|-----------|
| HTTP 200, `reachable:true` | `HostResponded` | success |
| HTTP 200, `reachable:false` with `error` matching `is_connect_timeout()` (timeout / unreachable / no route) | `Timeout` | no host-level reply |
| HTTP 200, `reachable:false` with any other error (RST, TLS error, banner timeout, ...) | `HostResponded` | host *did* respond; router forward works |
| HTTP 429 | `RateLimited(retry_after_seconds)` | service rate-limited us |
| HTTP 5xx / network error / malformed body | `ServiceError` | not the user's fault |

`PortProbeResult` maps these to five user-facing states:

- `Verified` (Pass) — port bound locally + `HostResponded`
- `ForwardReady` (Pass) — port free locally + `HostResponded`
- `Unreachable` (Warn) — `Timeout` (either branch)
- `Unverified` (Warn) — `ServiceError`: check.quip.network was down/errored, so
  we couldn't verify (no green check we didn't earn)
- `RateLimited { retry_after_secs, endpoint }` (Warn) — service rate-limited
  (HTTP 429), so we couldn't verify; the retry time is shown so the user can
  recheck after the cool-down. Not a green check we didn't earn.

A check only goes **green** when check.quip.network positively confirmed the
port (`Verified`/`ForwardReady`); `is_externally_reachable()` is true for those
two and nothing else.

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
- `ImageTag` — `Cpu | Cuda` (serialised lowercase; a legacy `"qpu"` JSON string
  is accepted as an alias for `Cpu` via `deserialize_image_tag_compat`)
- `GpuBackend` — `Local | Modal | Mps`
- `NodeConfig` — port (public API), validator_port (libp2p), validator_rpc_port
  (Native), secret, peers, GPU/QPU, REST, telemetry, …
- `AppSettings` — `{ node_config, active_tab, window_maximized, image_tag,
  tls_enabled, hostname (alias dashboard_hostname), cert_email, zerossl_api_key,
  run_mode, auto_update_enabled }`
- `StackStatus` — `{ services: Vec<ServiceStatus>, overall: StackHealth }`
- `ServiceStatus` — `{ name, service, running, health, status_text, image }`
- `StackHealth` — `Running | Degraded | Unhealthy | Stopped`

## Frontend IPC

The frontend uses `window.__TAURI__.core.invoke` (`withGlobalTauri: true`).

Events emitted by backend (complete set): `node-log`, `checklist-update`,
`pull-progress`, `stop-started`, `stop-complete`, `image-update-available`,
`binary-update-available`, `binary-download-progress`, `app-update-available`.
(The frontend also registers listeners for `node-status`, `stack-status`, and
`pull-complete`, none of which the backend emits — dead listeners.)

- `node-log` → `{ timestamp, level, message }`
- `checklist-update` → `CheckItem { id, state, label, detail, required,
  fixable, updated_at_ms }`
- `pull-progress` → `{ line }` (one `docker compose pull` output line) or a
  `--progress json` layer event forwarded verbatim
- `stop-started`, `stop-complete` — stop lifecycle
- `image-update-available` → `{ image, info }` (emitted per image whose digest
  changed, gated on `info.update_available`)
- `binary-update-available` → native-binary UpdateInfo
- `binary-download-progress` → `BinaryDownloadProgress` (native binary download %)
- `app-update-available` → node-manager UpdateInfo

Key Tauri commands (lib.rs `invoke_handler`):
- `start_stack` / `stop_stack` / `get_stack_status` / `get_stack_config`
- `pull_compose_images`
- `check_docker_installed` / `check_docker_hello_world` /
  `check_docker_compose_installed`
- `start_native_node` / `stop_native_node` / `get_native_node_status`
- `check_image_update(image_tag)` — node image digest
- `check_dashboard_image_update()` — dashboard image digest
- settings: `get_settings` / `update_settings` / `is_first_boot` /
  `get_default_data_dir` / `get_data_dir` / `set_data_dir` / `restart_app`
- `get_node_secret` / `generate_node_secret` / `generate_config_toml`
- hardware: `detect_gpu_backend` / `list_gpu_devices` / `run_hardware_survey`
- native: `check_native_binary` / `download_native_binary` /
  `check_binary_update` / `start_native_log_tail`
- `detect_public_ip` / `get_checklist` / `recheck`
- updates: `get_app_version` / `get_node_version` / `check_app_update`
- log streaming: `start_log_stream` / `stop_log_stream`

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
- Tauri commands return `Result<T, String>` (the common case; a few infallible
  commands return bare values, e.g. `is_first_boot -> bool`,
  `get_node_version -> Option<String>`, `restart_app -> ()`)
- No relative imports (`..`) in Rust — use `crate::module::Type`
- Line length ≤ 100 chars

## License

AGPL-3.0-or-later. All new source files require the standard license header.
