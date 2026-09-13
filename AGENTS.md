# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex, Cursor, etc.).

## Project Overview

Quip Node Desktop Manager — a Tauri v2 desktop app that orchestrates and monitors the Quip node
stack (miner + validator + dashboard + postgres + caddy). Runs the stack via Docker
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
│                                  # chain-specs/aglais-network.json). Embedded into
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
        ├── service_ports.rs       # Shared host port catalog and validation
        ├── container_http.rs      # HTTP probes inside the Compose network
        ├── settings.rs            # AppSettings, NodeConfig, ImageTag (Cpu|Cuda),
        │                          # StackStatus/StackHealth, DwaveConfig
        ├── secret.rs              # Node secret (64-char hex)
        ├── config.rs              # config.toml generation
        ├── cmd.rs                 # Command wrapper: PATH augmentation (login-shell
        │                          # $PATH + known tool dirs) + Windows no-console-flash
        ├── compose.rs             # docker compose orchestration: miner +
        │                          # validator + dashboard + postgres + caddy
        ├── stack_assets.rs        # include_str! the compose.yml + Caddyfile + chain
        │                          # spec; patch ports + Native upstream at stage time
        ├── log_stream.rs          # docker compose logs -f → Tauri events
        ├── native.rs              # native binary download + lifecycle
        ├── hardware.rs            # GPU/Docker/Python detection
        ├── network.rs             # Public IP detection only
        ├── update.rs              # Multi-image + app update monitor
        ├── migration_v2.rs        # v0.1 → v0.2 config/.env migration; backs up
        │                          # old files and promotes hand-edited host/port.
        │                          # REMOVE in v0.3 (drop v0.1 → v0.2 upgrades)
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
- **App version**: 0.2.6-rc4
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
  block-producing validator), `quip-dashboard`, `quip-postgres`, `quip-caddy`. The
  dashboard/Caddy reach the miner via the compose network alias `quip-miner`,
  and the validator via `quip-validator`. The miner self-bootstraps on first
  start — it auto-funds via the faucet for the selected channel and registers
  its keystore in `QuantumPow.Miners`, so there is no separate one-shot
  bootstrap container.
  D-Wave QPU mining activates on top of
  the CPU image via `config.toml [dwave]` (no separate qpu service). The
  upstream compose also defines an optional `quip-faucet` service behind a
  `faucet` profile, which the manager never starts.
- **Ports**: advanced settings in the GUI and TUI group host publications by
  service. See "Port Handling" for the complete list. Container addresses stay
  fixed regardless of host publishing settings.
- **Native listeners**: validator RPC defaults to `127.0.0.1:9944`. Native mode
  requires this host mapping, with public access optional. The native miner REST
  listener defaults to `0.0.0.0:20100` so Caddy can reach it through
  `host.docker.internal`.

## Docker Images

`vendor/nodes.quip.network/docker-compose.yml` declares the images, with
`${QUIP_*_TAG:-…}` placeholders. The manager's authoritative image **paths**
live in `src-tauri/src/compose.rs` (`CPU_IMAGE`, `CUDA_IMAGE`,
`VALIDATOR_IMAGE`, `DASHBOARD_IMAGE`).

The binary carries no default tag. Every start resolves a tag per image from
that image's own GitLab container registry for the selected update channel
(`resolve_channel_image_tags`) and writes the three results to `.env` as
`QUIP_MINER_TAG`/`QUIP_VALIDATOR_TAG`/`QUIP_DASHBOARD_TAG`. The repositories
advance on their own cadence, so the three tags often differ.

When a registry does not answer, the manager holds the tag `.env` already pins,
which keeps the stack on the version it runs instead of moving it backwards.
When `.env` pins nothing either — a first start with no network — the start
fails with a message. A compiled-in default tag cannot fill that gap: it ages
into a tag the registry no longer carries, which converts a passing network
fault into a permanent `pull` failure. Falling through to the compose file's own
`:-latest` default is also forbidden, because the update monitor compares
digests tag by tag, and `latest` moves under it.

- Miner (CPU): `registry.gitlab.com/quip.network/quip-miner/v0.3/quip-miner`
- Miner (CUDA): `registry.gitlab.com/quip.network/quip-miner/v0.3/quip-miner-cuda`

  The v0.3 images are a **separate repository line**, not new tags on the v0.2
  paths, which stop at `v0.2.1-rc54`. The CPU image also dropped its `-cpu`
  suffix when the coordinator absorbed the miner binaries, so the two names are
  no longer symmetric. Pointing a v0.3 tag at a v0.2 path fails the pull with
  `not found`.
- Validator: `registry.gitlab.com/quip.network/quip-validator/quip-network-node`
- Dashboard: `registry.gitlab.com/quip.network/dashboard.quip.network`
- Postgres: `postgres:16` (Docker Hub)
- Caddy: `caddy:2-alpine` (Docker Hub)

Selected by `AppSettings`:
- `image_tag: ImageTag` — `Cpu` | `Cuda`. D-Wave QPU mining is not a separate
  image: it rides on the CPU image via the `[dwave]` section in `config.toml`.
- `tls_enabled: bool` — controls whether Caddy provisions TLS (`:80`/`:443` are
  published by default and configurable in advanced settings).

The dashboard + postgres + caddy + validator services are always part
of the `cpu`/`cuda` profile — there is no `dashboard_enabled` toggle.

## Run Modes

| run_mode | node | compose services run |
|----------|------|----------------------|
| `Docker` | `quip-{cpu,cuda}` miner container via compose | every profile service: miner + `quip-validator` + `dashboard` + `postgres` + `caddy` (empty positional list ⇒ compose starts the whole profile) |
| `Native` (macOS only) | native miner binary on the host (`~/quip-data/bin/quip-miner-*`) | explicit list `quip-validator dashboard postgres caddy` — no miner container. The validator's JSON-RPC (9944) is published on `127.0.0.1:<validator_rpc_port>` so the host miner connects via `ws://127.0.0.1:<validator_rpc_port>`; the dashboard reaches the host miner's REST at `host.docker.internal:<rest_port>` |

## Compose Profiles

`image_tag → profile` (a single profile name, no TLS/dashboard variants):

| profile | services started |
|---------|------------------|
| `cpu` | `cpu` miner + `quip-validator` + `dashboard` + `postgres` + `caddy` |
| `cuda` | `cuda` miner + `quip-validator` + `dashboard` + `postgres` + `caddy` |

`compose_profile(image_tag)` returns the image's service name (`cpu` or `cuda`) —
there is no `qpu` profile (D-Wave mining rides on the CPU image via
`config.toml [dwave]`), and no `-notls`/`-nodash` variants. Caddy is always in
both profiles. The vendored compose file also defines an opt-in `faucet` profile
(`quip-faucet`), which the manager never selects.

In Native mode, `start_stack` passes an explicit positional service list
(`quip-validator dashboard postgres caddy`) that omits the miner, so
`--profile` gates eligibility while positional args restrict what actually
starts.

## Data Files (all in `~/quip-data/`)

| File | Generated / managed by | Purpose |
|------|------------------------|---------|
| `app-settings.json` | settings.rs (user preferences) | UI toggles + NodeConfig |
| `config.toml` | config.rs on every Start | Node config (bind-mounted into the node container in Docker mode; read directly by the binary in Native mode) |
| `.env` | compose.rs on every Start | Compose env: PUID, PGID, QUIP_HOSTNAME, CERT_EMAIL, ZEROSSL_API_KEY, DWAVE_API_KEY, POSTGRES_PASSWORD, QUIP_MINER_TAG, QUIP_DASHBOARD_TAG, QUIP_VALIDATOR_TAG, QUIP_MINER_CPUSET, VALIDATOR_NAME, QUIP_GPU_UTILIZATION; mode 0600 on Unix. (No QUIP_NODE_URL — removed in v0.2. No QUIP_VALIDATORS — the upstream compose made the miner fully config-driven, so validators live only in `config.toml`. QUIP_VALIDATOR_RPC_URLS is deliberately NOT written — it defers to the compose default `ws://quip-caddy:8088/rpc`, Caddy's internal front door, so the dashboard resolves both the chain RPC and the local miner REST from one host.) |
| `docker-compose.yml` | stack_assets.rs (embedded copy + patch) | Upstream compose with Caddy host API port → `<port>:20049`, validator libp2p → `<validator_port>:30333/tcp+udp`, `--public-addr` injected when `public_host` set, and (Native) validator RPC published on `127.0.0.1:<validator_rpc_port>:9944` |
| `caddy/Caddyfile` | stack_assets.rs (embedded copy + patch) | Caddy routes; the local faucet route is always stripped; in Native mode the `/api/v1/*` upstream is rewritten from `quip-miner:8086` to `host.docker.internal:<rest_port>` |
| `chain-specs/aglais-network.json` | stack_assets.rs (embedded copy) | Quip Testnet chain spec mounted into the validator container |
| `keystore.json` | native.rs (Native mode) | Native miner signer keystore (generated via `quip-miner keygen`) |
| `data/` | bind-mount target for the miner's `/data` (Docker) and host config.toml path (Native) | miner runtime `config.toml`, `keystore.json`; the validator's state lives under `data/validator-data/` (mounted as the validator container's `/data`) |
| `dashboard-data/` | bind-mount target for the dashboard | Dashboard auxiliary state |
| `node-secret.json` | secret.rs | `{ "secret": "<64-hex>" }` — read by secret.rs and gates the `secret` pre-flight check, but NOT written into config.toml in v0.2. The node's actual signing identity is `keystore.json` (Docker `/data/keystore.json`, Native `keystore.json`). |
| `bin/quip-miner-*` | native.rs | Downloaded native miner binary (`quip-miner-macos-arm64` / `-x86_64`). Legacy pre-v0.2 `quip-network-node-*` binaries here are auto-deleted on launch. |

Project-scoped Docker volumes (survive `docker compose down` by design):
`quip_pgdata`, `quip_caddy-data`, `quip_caddy-config`. The upstream compose
pins fixed global `name:`s (`quip-pgdata`, …); the manager strips them at
stage time (`stack_assets::strip_volume_names`) so they don't collide with
other Quip stacks on the same host.

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

`start_stack` and `pull_compose_images` call
`stack_assets::sync_stack_assets(run_mode, config)` before invoking Docker Compose.
It stages the embedded compose.yml, Caddyfile, and chain spec
(`chain-specs/aglais-network.json`), always overwriting — no merge. Patches:

1. **Compose host publications**: rebuild each managed service's `ports` list
   from `service_ports::PORT_SPECS` and the saved enable switches and host ports.
   Internal ports stay fixed. Public mappings omit the host address so Docker
   can publish on both IPv4 and IPv6. Omit mappings with their switch off.

2. **Validator public address**: when the user sets `public_host` and turns on
   P2P publishing, insert `--public-addr=<multiaddr>` using `validator_port`.

3. **Native validator RPC**: always publish
   `127.0.0.1:<validator_rpc_port>:9944/tcp`. The public-access switch removes
   the loopback restriction. Publish Docker RPC only when the user turns it on.

4. **Caddyfile faucet strip** (always): the optional local faucet route block is
   removed. The manager uses the public faucet for the selected channel. Beta
   uses the Aglais faucet. Release uses the testnet faucet. See
   `UpdateChannel::faucet_url`.

5. **Caddyfile upstream rewrite** (Native mode only): `quip-miner:8086` becomes
   `host.docker.internal:<native_rest_port>` so the dashboard container reaches
   the host miner. Docker mode keeps `quip-miner:8086`.

Why embedded + patched at stage time (instead of compose's `${VAR}` env
substitution): the Caddyfile upstream rewrite and the validator `--public-addr`
arg both require rewriting a YAML/Caddyfile token, not just supplying an env
var, so all the port/host remaps live in one patch pass for consistency.

## Port Handling

Container ports stay fixed. Each row below has an enable switch and host-port
field in advanced settings. The shared catalog is `service_ports::PORT_SPECS`.

| Service | Ports and purpose | Public by default |
|---------|-------------------|-------------------|
| `quip-validator` | 30333 TCP/UDP P2P, 9944 TCP RPC, and 9615 TCP metrics | P2P only |
| `quip-miner` | 8086 TCP REST for CPU or CUDA | No |
| `quip-dashboard` | 3001 TCP HTTP | No |
| `quip-postgres` | 5432 TCP PostgreSQL | No |
| `quip-caddy` | 20049 TCP API, 80 TCP HTTP, and 443 TCP HTTPS | Yes |
| `quip-caddy` | 443 UDP and 20049 UDP HTTP/3, 8088 TCP internal HTTP, and 2019 TCP administration | No |

Host ports default to the container ports. HTTP/3 requires TLS on its matching
site. Publishing the administration API also changes its bind address.
That API can change the Caddy configuration.

Native mode requires RPC, which defaults to 9944 and remains local unless the
user selects public access. Caddy also needs native miner REST.
Its separate host listener defaults to 20100. Switching modes preserves the
Docker miner's optional host mapping. The manager probes Docker services from
inside the validator container. Health reporting works with public ports off.
Public API and P2P checks show a warning when their switches are off, without
running an external probe.

The existing `port`, `validator_port`, and `validator_rpc_port` fields hold
host port numbers. Their switches are `public_api_enabled`,
`validator_p2p_enabled`, and `validator_rpc_enabled`. Other bindings live in
`NodeConfig.service_ports`. Saves reject zero, ports greater than 65535, and active
bindings that conflict on the same transport protocol.

The generated miner config and the staged first-start template both use only
`ws://quip-validator:9944` in Docker mode. Native uses
`ws://127.0.0.1:<validator_rpc_port>`. Public host mappings never change these
internal Docker URLs.

For the miner's own `config.toml`: `config.rs` always emits `public_port` in both
modes. It takes `config.public_port` when the user sets an override, and falls
back to `port` (the Caddy front door) otherwise, because that is the port an
outside peer actually reaches. There is no separate top-level `port` key in the
miner config — that is the v0.1 schema, and a test asserts it stays gone. The
The miner's REST surface is a `[dashboard]` section, not the v0.2
`[miner].rest_host` / `rest_port` pair. The v0.3 coordinator ignores those two
keys outright, and it disables the dashboard unless **both** `listen` and
`data_dir` are set, so neither may be omitted. Docker renders
`listen = "0.0.0.0:8086"` to match the Caddyfile's `quip-miner:8086` upstream,
with `data_dir = "/data/attempts"` inside the volume. Native renders
`listen = "0.0.0.0:<native_rest_port>"` (default 20100) and
`data_dir = <data_dir>/attempts`. This bind lets the Caddy container reach the
listener through the Docker host gateway.

### `public_host` resolution and the start gate

Both start paths (`compose::start_stack_core` and `native::start_native_node_core`)
fill an unset `public_host` before they write `config.toml`. The value comes from
`checklist::fetch_public_ip` (check.quip.network first, ipify as a fallback), which
is the same fetcher behind the `ip` checklist row, so the row and the advertised
address cannot disagree. The resolution is per start and is never persisted to
`app-settings.json`.

`checklist::require_public_host` then hard-aborts the start when the resolved value
is one no outside peer can reach: loopback, unspecified, RFC1918 private,
169.254.0.0/16 link-local, 100.64.0.0/10 carrier-grade NAT, multicast,
240.0.0.0/4 reserved, IPv6 `fc00::/7` unique-local, IPv6 `fe80::/10` link-local,
and (for names) anything `hostnames::is_public_dns_host` rejects, including mDNS
`.local`. IPv4-mapped IPv6 is unwrapped before the test. A local-network or
air-gapped deployment that wants to advertise a private address cannot start, and
no opt-in override exists yet.

There is no standalone `public-host` checklist row. The two port rows already probe
host and port together through `/checkport`, and they stay warn-only.

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
| HTTP 200, `reachable:true` | `HostResponded` | TCP connect succeeded — forward works and something is listening |
| HTTP 200, `reachable:false` (any `error`: timeout, RST/"connection refused", ...) | `Unreachable` | the external TCP connect could not be established |
| HTTP 429 | `RateLimited(retry_after_seconds)` | service rate-limited us |
| HTTP 5xx / network error / malformed body | `ServiceError` | not the user's fault |

`PortProbeResult` maps these to five user-facing states:

- `Verified` (Pass) — port bound locally + `HostResponded`
- `ForwardReady` (Pass) — port free locally + `HostResponded`
- `Unreachable` (Warn) — `Unreachable`
- `Unverified` (Warn) — `ServiceError`: check.quip.network was down/errored, so
  we couldn't verify (no green check we didn't earn)
- `RateLimited { retry_after_secs, endpoint }` (Warn) — service rate-limited
  (HTTP 429), so we couldn't verify; the retry time is shown so the user can
  recheck after the cool-down. Not a green check we didn't earn.

A check only goes **green** when check.quip.network positively confirmed the
port (`Verified`/`ForwardReady`); `is_externally_reachable()` is true for those
two and nothing else.

**Design rule:** *`/checkport` is a plain-TCP connect, so reachability is
binary.* Only `reachable:true` (a SYN-ACK proving the forward works and a
listener is up) passes. Every `reachable:false` — timeout, RST, or
"connection refused" — fails the check, because in each case the prober
could not open a TCP connection to the port.

### Probe Diagnostics

Every probe call emits a `[probe]` line to the `node-log` event with the
full request URL, HTTP status, and response body (truncated at 1 KB).
Users can copy/paste the raw output into support threads — the raw `error`
string is the ground truth for *why* a port was unreachable (it no longer
affects classification, which is binary on `reachable`). The `AppHandle` is
plumbed via `Option<AppHandle>` on `CheckCtx`,
so non-Tauri callers (the TUI) probe silently.

## Shared Types (defined in `settings.rs`)

- `RunMode` — `Docker | Native` (Native is macOS-only)
- `ImageTag` — `Cpu | Cuda` (serialised lowercase; a legacy `"qpu"` JSON string
  is accepted as an alias for `Cpu` via `deserialize_image_tag_compat`)
- `GpuBackend` — `Local | Modal | Mps`
- `NodeConfig` — port (public API), validator_port (libp2p), validator_rpc_port
  (RPC), service_ports, publishing switches, secret, peers, GPU/QPU, REST, telemetry, …
- `AppSettings` — `{ node_config, active_tab, window_maximized, image_tag,
  tls_enabled, hostname (alias dashboard_hostname), cert_email, zerossl_api_key,
  run_mode, auto_update_enabled }`
- `StackStatus` — `{ services: Vec<ServiceStatus>, overall: StackHealth }`
- `ServiceStatus` — `{ name, service, running, health, status_text, image }`
- `StackHealth` — `Running | Degraded | Unhealthy | Stopped`

The Rust service attaches container logs before stack startup. The follower waits for
staged files and retries after Docker exits. Each log session owns its stop flag
and child process. Native startup replaces the container session with a combined
file and container session. Stop cancels the current session and its retries.

## Frontend IPC

The frontend uses `window.__TAURI__.core.invoke` (`withGlobalTauri: true`).

Events emitted by backend (complete set): `node-log`, `checklist-update`,
`pull-progress`, `pull-complete`, `stop-started`, `stop-complete`,
`dashboard-db-mismatch`, `image-update-available`, `binary-update-available`,
`binary-download-progress`, `app-update-available`.

- `node-log` → `{ timestamp, level, message }`
- `checklist-update` → `CheckItem { id, state, label, detail, required,
  fixable, updated_at_ms }`
- `pull-progress` → `{ line }` (one `docker compose pull` output line) or a
  `--progress json` layer event forwarded verbatim
- `pull-complete` → `{ gen, success, error }` (emitted when the pull process
  exits — the authoritative "pull is over" signal)
- `stop-started`, `stop-complete` — stop lifecycle
- `dashboard-db-mismatch` → `{ message }` (Postgres volume password mismatch)
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

## Versioning & Release Tags

Canonical spec: `quip-miner/docs/VERSIONING.md`. This repo follows the same
cross-repo standard so `update.rs::parse_semver` orders release candidates
correctly — it splits the pre-release on `-`, so a no-hyphen `v0.2.1rc18` loses
*both* the patch and the rc number and collapses every rc to one value, which
freezes deployed nodes on an old rc.

| Artifact | Format | Example |
|----------|--------|---------|
| Git release tag (pre-release) | hyphenated SemVer `vMAJOR.MINOR.PATCH-rcN` | `v0.2.1-rc18` |
| Git release tag (stable) | `vMAJOR.MINOR.PATCH` | `v0.2.1` |
| Package version (`package.json`, `Cargo.toml`, `tauri.conf.json`) | toolchain-native (npm/Cargo SemVer; PEP 440 elsewhere) | `0.2.1-rc2` |

Rules:
- Pre-release git tags MUST be hyphenated (`-rcN` / `-alphaN` / `-betaN`); never
  the PEP 440 no-hyphen form for a git tag.
- Numeric parts (MAJOR.MINOR.PATCH and the rc number) MUST match between the git
  tag and the package version; only the separator may differ.
- CI: pre-release tags publish `:<tag>` + the rolling `:vMAJOR.MINOR` and MUST
  NOT move `:latest`; only `main` / a stable `vX.Y.Z` tag moves `:latest`. The
  `:latest` rule binds on image-publishing repos (quip-miner); this repo ships
  desktop binaries via a per-tag GitLab Release and has no `:latest` to gate.

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
