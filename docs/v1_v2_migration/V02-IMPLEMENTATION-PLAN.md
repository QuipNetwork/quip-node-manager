# Quip Node Manager v0.2 Implementation Plan

This is the execution plan derived from `V02-ALIGNMENT-PLAN.md`. The goal is
to move the manager to the `nodes.quip.network` v0.2 stack, including the
Docker stack and native macOS miner assets.

## Scope

Implement now:

- v0.2 Docker CPU/CUDA stack support.
- v0.2 config rendering with `[miner]`.
- v0.1 to v0.2 config migration.
- public API port handling for `20049`.
- validator host port handling, default `30033` mapped to container `30333`.
- dashboard/Caddy default flow on port `20049`.
- checklist and UI updates needed for the above.
- image/update references for v0.2 Docker images.

Defer:

- No remaining v0.2 alignment decisions.

Resolved after Phase 10:

- Set validator `--public-addr` from `public_host` when it is configured.
- Do not support dashboard-disabled mode in the v0.2 manager flow.
- Use `public_host` as the Caddy/API hostname when it is a DNS hostname;
  otherwise use the existing hostname setting.
- Do not use a local faucet; rely on the public testnet faucet.

## Phase 0: Baseline And Safety

Files:

- `vendor/nodes.quip.network`
- `V02-ALIGNMENT-PLAN.md`
- `V02-IMPLEMENTATION-PLAN.md`

Tasks:

- Keep the submodule pinned at `e28c202964268525d257bb65bb90785889547c0d`.
- Leave unrelated untracked files such as `.idea/` untouched.
- Run a pre-change status check and record existing dirty files.
- Run current test/build commands if they are already available locally:
  - `cargo test --manifest-path src-tauri/Cargo.toml`
  - `bun run build` if dependencies are installed

Verification gate:

- Baseline failures, if any, are recorded before code changes.

## Phase 1: Settings And Serialized State

Files:

- `src-tauri/src/settings.rs`
- `src/app.js`
- `src/index.html`
- `src/tui_app.rs`
- `src/tui_ui.rs`
- `src/tui_input.rs`

Tasks:

- Add `validator_port: u16` to `NodeConfig`.
- Default `validator_port` to `30033`.
- Keep existing v0.1 fields deserializable for compatibility, but treat them as
  legacy fields that are not rendered into v0.2 config.
- Change dashboard/Caddy default hostname semantics:
  - env `QUIP_HOSTNAME` default should be `:20049`.
  - UI dashboard URL should resolve that to `http://localhost:20049/`.
- Add frontend and TUI controls for validator P2P port.
- Rename user-facing "Port (UDP+TCP)" language to public API port.
- Keep `node_config.port` as the public API/Caddy host port, default `20049`.

Verification gate:

- Loading old `app-settings.json` does not wipe settings.
- New settings serialize and deserialize with defaults.
- Frontend can collect and populate the new validator port.

## Phase 2: v0.2 Config Renderer

Files:

- `src-tauri/src/config.rs`
- `src-tauri/src/settings.rs`

Tasks:

- Replace `[global]` rendering with `[miner]` rendering.
- Docker miner config:
  - `validators = ["ws://quip-validator:9944"]`
  - `signer_key = "/data/keystore.json"`
  - `rest_host = "0.0.0.0"`
  - `rest_port = 80`
- Native/Metal config remains behind existing native mode paths:
  - use host-local paths.
  - point validators at `ws://127.0.0.1:<public-api-port>/rpc`.
  - launch as
    `quip-miner <cpu|gpu|qpu> --config <toml> --signer-key <keystore>
    --faucet-url https://faucet.testnet.quip.network`.
    `--config` belongs to the miner subcommand.
  - run `quip-miner keygen --out <data_dir>/keystore.json` before native
    miner start when the configured signer key does not exist.
  - native binary install/update uses the `v0.2-preview` `quip-miner` assets.
- Preserve useful miner fields:
  - `node_name`
  - `public_host`
  - `public_port`
  - `log_level`
  - `node_log`
- Render backend tables:
  - `[cpu]`
  - `[gpu]`
  - `[cuda.N]`
  - `[metal]`
  - `[modal]`
  - `[qpu]`
  - `[dwave]`
- Stop rendering v0.1-only fields:
  - `peers`
  - `auto_mine`
  - `secret`
  - `genesis_config`
  - `listen`
  - `port`
  - `timeout`
  - `heartbeat_interval`
  - `heartbeat_timeout`
  - `fanout`
  - `tofu`
  - `trust_db`
  - miner TLS cert/key fields
  - telemetry fields
  - `http_log`

Verification gate:

- Unit tests assert `[miner]` exists and `[global]` does not.
- Docker config contains internal miner REST port `80`.
- Public host and public port are preserved in rendered TOML.
- QPU/D-Wave config still renders when configured.

## Phase 3: Stack Asset Patching

Files:

- `src-tauri/src/stack_assets.rs`
- `vendor/nodes.quip.network/docker-compose.yml`
- `vendor/nodes.quip.network/caddy/Caddyfile`

Tasks:

- Update patching for the v0.2 compose file.
- Patch public API/Caddy port:
  - upstream `"20049:20049"` to `"<node_config.port>:20049"`.
- Patch validator host port:
  - upstream `"30333:30333/tcp"` to `"<validator_port>:30333/tcp"`.
  - upstream `"30333:30333/udp"` to `"<validator_port>:30333/udp"`.
  - default output should contain `"30033:30333/tcp"` and
    `"30033:30333/udp"`.
- Patch Native/Metal Caddy upstream only as a compatibility placeholder:
  - Docker miner keeps `quip-miner:80`.
  - Native miner path rewrites `quip-miner:80` to
    `host.docker.internal:<native_rest_port>`.
- Patch validator `--public-addr` when `public_host` is configured:
  - DNS host -> `/dns4/<host>/tcp/<validator_port>`.
  - IPv4 host -> `/ip4/<host>/tcp/<validator_port>`.
  - IPv6 host -> `/ip6/<host>/tcp/<validator_port>`.

Verification gate:

- Unit tests cover default and custom public API port.
- Unit tests cover default and custom validator port for TCP and UDP.
- Staged Caddyfile still contains `/rpc` and `/api/v1/*` routes, and does not
  expose the optional local faucet route.

## Phase 4: Compose Orchestration

Files:

- `src-tauri/src/compose.rs`
- `src-tauri/src/log_stream.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/src/tui_app.rs`

Tasks:

- Replace profile logic with v0.2 profiles:
  - Docker mode: `cpu` or `cuda`.
  - Remove `cpu-notls`, `cuda-notls`, `cpu-nodash`, `cuda-nodash`.
- Do not support dashboard-disabled mode:
  - the v0.2 standard stack includes Caddy, dashboard, Postgres, validator,
    bootstrap, and miner.
  - remove the `dashboard_enabled` app setting; legacy settings files can keep
    the extra key, but the manager no longer reads or writes it.
- Native/Metal compose support services:
  - do not start `cpu`, `cuda`, or `quip-bootstrap`.
  - start `quip-validator`, `dashboard`, `postgres`, and `caddy` only if native
    support is still exercised before the native installer is complete.
- Update known container cleanup list:
  - `quip-cpu`
  - `quip-cuda`
  - `quip-validator`
  - `quip-bootstrap`
  - `quip-dashboard`
  - `quip-postgres`
  - `quip-caddy`
- Remove `quip-qpu` from v0.2 known containers, but consider keeping a
  one-time legacy cleanup path for old v0.1 containers.
- Update orphan image sweep image names.
- Update log streaming service names:
  - Docker miner logs still follow selected `cpu` or `cuda`.
  - consider including validator/bootstrap logs in start output if bootstrap
    failures are otherwise hard to see.

Verification gate:

- `docker compose config` works against staged v0.2 files.
- `compose_profile` tests reflect only `cpu` and `cuda`.
- Stop path removes v0.2 containers and does not depend on `quip-qpu`.

## Phase 5: Environment Generation

Files:

- `src-tauri/src/compose.rs`
- `src-tauri/src/settings.rs`

Tasks:

- Rewrite generated `.env` for v0.2.
- Emit:
  - `PUID`
  - `PGID`
  - `QUIP_HOSTNAME`
  - `CERT_EMAIL`
  - `ZEROSSL_API_KEY`
  - `DWAVE_API_KEY`
  - `POSTGRES_PASSWORD`
  - `QUIP_MINER_TAG=v0.2-preview`
  - `QUIP_DASHBOARD_TAG=v0.2-preview`
  - `QUIP_VALIDATOR_TAG=v0.2-preview`
  - `QUIP_MINER_CPUSET=0`
  - `VALIDATOR_NAME`
  - `QUIP_VALIDATORS=ws://quip-validator:9944`
  - `QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944`
- Do not emit:
  - `QUIP_NODE_URL`
  - `QUIP_NODE_TOKEN`
- Keep `.env` mode `0600` on Unix.

Verification gate:

- Generated `.env` has no v0.1 dashboard variables.
- Dashboard env uses `QUIP_VALIDATOR_RPC_URLS`.
- D-Wave token still propagates through `DWAVE_API_KEY`.

## Phase 6: v0.1 To v0.2 Migration

Files:

- `src-tauri/Cargo.toml`
- `src-tauri/src/migration_v2.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/src/compose.rs`
- `src-tauri/src/config.rs`

Tasks:

- Add a TOML parser dependency for structured migration parsing.
  - Use `toml` crate unless a repo-preferred parser already exists.
- Create a migration module with SPDX header.
- Detect schema:
  - v0.1: `[global]` present, `[miner]` absent.
  - v0.2: `[miner]` present, `[global]` absent.
  - ambiguous: both present, return a clear error.
  - unknown: neither present, return a clear error.
- Backup:
  - Docker data path: `<data_dir>/data`.
  - Native data path: `<data_dir>`.
  - create `.v0.1_backup`.
  - move existing entries into backup.
  - if backup exists and current config is v0.2, treat as already migrated.
  - if backup exists and current config is v0.1, create timestamped backup or
    fail clearly. Prefer failing clearly for first pass.
- Convert:
  - `[global].node_name` to `[miner].node_name`.
  - `[global].public_host` to `[miner].public_host`.
  - `[global].public_port` to `[miner].public_port`.
  - `[global].rest_host` to `[miner].rest_host` if useful.
  - `[global].log_level` to `[miner].log_level`.
  - `[global].node_log` to `[miner].node_log`.
  - force Docker `[miner].rest_port = 80`.
  - force Docker `[miner].signer_key = "/data/keystore.json"`.
  - default validators to `["ws://quip-validator:9944"]`.
  - preserve backend tables.
- Warn and drop v0.1-only keys.
- Migrate `.env` if present:
  - backup to `.env.v0.1_backup`.
  - remove `QUIP_NODE_URL` and `QUIP_NODE_TOKEN`.
  - append commented `QUIP_VALIDATOR_RPC_URLS` placeholder if absent.
- Run migration before writing the manager-generated v0.2 config on start.
- Emit migration warnings to `node-log`.

Verification gate:

- Unit tests mirror upstream fixtures for CPU, CUDA, and QPU/D-Wave.
- Migration is idempotent for already-v0.2 configs.
- Migration preserves `public_host` and `public_port`.
- Migration refuses ambiguous config.

## Phase 7: Checklist And Port Probes

Files:

- `src-tauri/src/checklist.rs`
- `src/app.js`
- `src/index.html`
- `src-tauri/src/tui_ui.rs`

Tasks:

- Reinterpret public API port check:
  - `node_config.port` is Caddy HTTP/WS, so use TCP `/checkport`.
  - do not use old QUIC `/checkconn` semantics for `20049`.
- Add validator P2P checklist item:
  - ID suggestion: `port-validator`.
  - label: `Validator P2P port 30033 reachable`.
  - check TCP reachability with `check.quip.network/checkport`.
  - local firewall check should cover TCP and UDP bindability.
  - state should be warning-only, never startup-blocking.
- Update checklist visibility and labels.
- Update frontend idle label text.
- Update TUI labels.

Verification gate:

- Recheck on public API port uses TCP probe.
- Validator port warning does not block start.
- Labels show `20049` for API and `30033` for validator by default.

## Phase 8: UI Cleanup

Files:

- `src/index.html`
- `src/app.js`
- `src/styles.css`
- `src-tauri/src/tui_ui.rs`
- `src-tauri/src/tui_input.rs`

Tasks:

- Rename old node/P2P controls to v0.2 language.
- Remove or hide obsolete controls:
  - bootstrap peers
  - auto-mine
  - miner TLS verify/cert/key
  - old REST HTTPS/insecure split
  - telemetry directory
  - heartbeat/timeouts/fanout
- Keep advanced legacy fields only if still needed for settings compatibility,
  not as primary UI.
- Add validator P2P port control.
- Change dashboard default display URL to `http://localhost:20049/`.
- Update Caddy hostname help:
  - local default `:20049`
  - production `example.com, example.com:20049`
- Keep QPU UI, but phrase it as D-Wave mining on top of CPU miner mode.

Verification gate:

- Form collect/populate round trips without losing settings.
- Text does not refer to old QUIC peer port behavior.
- Dashboard tab opens `localhost:20049` by default.

## Phase 9: Update Checks And Image References

Files:

- `src-tauri/src/compose.rs`
- `src-tauri/src/update.rs`
- `src-tauri/src/checklist.rs`

Tasks:

- Update image constants:
  - CPU miner: `registry.gitlab.com/quip.network/quip-protocol/quip-miner-cpu`
  - CUDA miner: `registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda`
  - validator:
    `registry.gitlab.com/quip.network/quip-validator/quip-network-node`
  - dashboard: `registry.gitlab.com/quip.network/dashboard.quip.network`
- Use configured preview tags where possible instead of hardcoded `latest`.
- Include validator in relevant image checks for Docker mode.
- Native binary update checks use the `v0.2-preview` `quip-miner` release
  assets.

Verification gate:

- Image pull/checklist references match v0.2 compose images.
- No `quip-network-node-cpu:latest` or `quip-network-node-cuda:latest`
  references remain in Docker paths.

## Phase 10: End-To-End Verification

Commands:

- `cargo test --manifest-path src-tauri/Cargo.toml`
- `bun run build`
- `docker compose config` against staged stack files

Manual smoke tests:

- Docker CPU start:
  - stack pulls v0.2 miner, validator, dashboard, postgres, caddy.
  - `quip-bootstrap` completes or logs actionable failure.
  - dashboard loads at `http://localhost:20049/`.
  - `/rpc` proxies to validator.
  - `/api/v1/*` proxies to miner.
- Docker CUDA config validation:
  - compose config includes CUDA miner and validator.
- Port checks:
  - public API port `20049` uses TCP check.
  - validator host port `30033` warns if unreachable and does not block start.
- Migration:
  - v0.1 config migrates to `[miner]`.
  - backup is present.
  - public host and public port survive.
- Native / Physical Metal start:
  - starts the validator/dashboard compose support stack before launching the
    host miner.
  - waits for the host-visible validator RPC route to answer JSON-RPC before
    launching the host miner.
  - launches with config path, signer key, and public faucet URL; other miner
    values come from config or binary defaults.
  - does not pass `--config` before the subcommand.
  - generates `<data_dir>/keystore.json` on first run and preserves existing
    keystores.

## Pending Follow-Ups

- Start failure cleanup:
  - If Docker mode fails to start after bringing up part of the stack, clean up
    dangling compose services so the next start begins from a known state.
  - If Native mode fails after starting the Docker support stack or while
    launching the host miner, stop the support services and clear any partial
    native process state.
  - Cleanup should avoid deleting data directories or named volumes unless the
    user explicitly asks for a reset.
- Node version check performance:
  - Make native miner version checks faster by using the cached downloaded
    release marker where possible.
  - Short-circuit remote release/version checks when the installed binary name
    and cached tag already match the target tag.
  - Revalidate when the target tag, platform asset name, or marker format
    changes.

## Suggested Implementation Order

1. Settings and UI field plumbing for `validator_port`.
2. Config renderer conversion to `[miner]`.
3. Stack asset patching for `20049` and `30033:30333`.
4. Compose profile/service/image updates.
5. Environment generation update.
6. Migration module and tests.
7. Checklist port probe updates.
8. UI cleanup pass.
9. Update/image reference cleanup.
10. Full verification.

This order keeps the stack runnable as early as possible, then layers migration
and UX polish on top.

## Deferred Decision Log

- `--public-addr`: resolved. Set it from `public_host` when configured, using
  the configured host-exposed validator port.
- Native miner install/update: resolved. Use `quip-miner-macos-arm64` and
  `quip-miner-macos-x86_64` from the `v0.2-preview` `quip-protocol` release.
- Dashboard-disabled support: resolved. It is not supported in the v0.2 manager
  flow.
- `dashboard_hostname` naming: resolved. Rename the stored setting to
  `hostname`; accept legacy `dashboard_hostname` while reading old settings.
  This hostname is shared by all services because Caddy proxies the stack. A
  DNS `public_host` takes precedence when suitable for Caddy.
- Faucet/localdev UI: resolved. The manager does not start or proxy a local
  faucet; bootstrap uses the public testnet faucet.
