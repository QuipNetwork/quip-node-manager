# Quip Node Manager v0.2 Alignment Plan

This plan aligns `quip-node-manager` with the `nodes.quip.network` v0.2 stack
currently vendored at:

- v0.2: `e28c202964268525d257bb65bb90785889547c0d`
- previous main baseline: `7e27bb6a16eb3863f92318d9d8325c6c59eda3df`

## Upstream Diff Summary

The v0.2 stack is a topology change, not just an image update.

- The old monolithic `quip-node` becomes a `quip-miner` client.
- Miner images move from:
  - `quip-network-node-cpu:latest`
  - `quip-network-node-cuda:latest`
  to:
  - `quip-miner-cpu:${QUIP_MINER_TAG:-v0.2-preview}`
  - `quip-miner-cuda:${QUIP_MINER_TAG:-v0.2-preview}`
- A new substrate validator service is bundled into the `cpu` and `cuda`
  profiles:
  - `registry.gitlab.com/quip.network/quip-validator/quip-network-node`
- The old `qpu` compose service/profile is removed. QPU mining is now CPU miner
  plus `[qpu]` and `[dwave]` config sections.
- A new `quip-bootstrap` sidecar registers and funds the miner before the miner
  starts.
- Dashboard indexing moves from old miner REST assumptions toward substrate RPC
  via `QUIP_VALIDATOR_RPC_URLS`.
- Caddy becomes the single HTTP/WS front door:
  - `/rpc` -> `quip-validator:9944`
  - `/api/v1/*` -> `quip-miner:80`
  - `/` -> dashboard
- Public `20049/tcp` is now the Caddy API/dashboard/RPC port, not the miner's
  QUIC peer port.
- Validator libp2p uses `30333/tcp+udp` inside the upstream compose stack.
  The manager should default the host-exposed validator port to `30033`, mapped
  to the container's internal `30333`.

## Manager Architecture Impact

### Settings Model

Keep `ImageTag::{Cpu,Cuda}` and continue treating QPU as a CPU miner backend
selected by config. Add v0.2-specific fields:

- `validator_port: u16`, default `30033`
- optionally `miner_cpu_set: String`, default `"0"` for Docker CPU miners
- optionally `validator_rpc_urls: Vec<String>` for miner-only or remote-validator
  deployments
- optionally a stack version marker, such as `stack_version: "v0.2"`, to make
  migrations explicit and idempotent

Old v0.1 fields can stay in `NodeConfig` for backward-compatible settings
deserialization, but should stop being rendered into v0.2 `config.toml`:

- `peers`
- `auto_mine`
- `timeout`
- `heartbeat_interval`
- `heartbeat_timeout`
- `fanout`
- `tofu`
- `trust_db`
- miner TLS certificate settings
- file telemetry settings

### Config Generation

Replace `[global]` rendering with `[miner]`.

Docker miner output:

```toml
[miner]
validators = ["ws://quip-validator:9944"]
signer_key = "/data/keystore.json"
rest_host = "0.0.0.0"
rest_port = 80
```

Native / Physical Metal miner output:

```toml
[miner]
validators = ["ws://127.0.0.1:<public-api-port>/rpc"]
signer_key = "<data_dir>/keystore.json"
rest_host = "127.0.0.1"
rest_port = <native_rest_port>
```

Preserve and render backend tables:

- `[cpu]`
- `[gpu]`
- `[cuda.N]`
- `[nvidia.N]`
- `[metal]`
- `[modal]`
- `[qpu]`
- `[dwave]`

Carry forward these miner-level fields when present:

- `node_name`
- `public_host`
- `public_port`
- `log_level`
- `node_log`

Do not render v0.1 peer/TLS/TOFU/telemetry keys into v0.2 config.

### Compose Asset Patching

Update `stack_assets.rs` for v0.2 staged assets.

Current patching rewrites `"20049:20049/tcp"` and UDP mappings. In v0.2:

- Patch Caddy host public API port:
  - upstream default `"20049:20049"` -> `"<settings.node_config.port>:20049"`
- Patch validator libp2p host port:
  - upstream default `"30333:30333/tcp"` -> `"<validator_port>:30333/tcp"`
  - upstream default `"30333:30333/udp"` -> `"<validator_port>:30333/udp"`
  - manager default: `"30033:30333/tcp"` and `"30033:30333/udp"`

Set validator `--public-addr` from `public_host` when it is configured. The
manager converts DNS hosts to `/dns4/<host>/tcp/<validator_port>`, IPv4 hosts to
`/ip4/<host>/tcp/<validator_port>`, and IPv6 hosts to
`/ip6/<host>/tcp/<validator_port>`.

Native mode Caddy patching changes from `quip-node:80` to the v0.2 miner alias:

- Docker miner: leave `quip-miner:80`
- Native miner: rewrite `/api/v1/*` upstream from `quip-miner:80` to
  `host.docker.internal:<native_rest_port>`

### Compose Profiles And Services

Replace old profile logic:

- remove `cpu-notls`, `cuda-notls`, `cpu-nodash`, `cuda-nodash`
- use upstream `cpu` / `cuda` profiles
- do not add the local `faucet` profile; the stack uses the public faucet

Docker mode should start the full selected profile:

- `cpu` or `cuda`
- includes miner, validator, bootstrap, dashboard, postgres, caddy

Native / Physical Metal mode should start only Docker-side support services:

- `quip-validator`
- `dashboard`
- `postgres`
- `caddy`

It should not start `cpu`, `cuda`, or `quip-bootstrap`, because the miner runs
on the host.

Update known container cleanup:

- keep `quip-cpu`
- keep `quip-cuda`
- remove `quip-qpu`
- add `quip-validator`
- add `quip-bootstrap`
- keep `quip-dashboard`
- keep `quip-postgres`
- keep `quip-caddy`

### Environment Generation

Replace v0.1 `.env` content.

Required or useful v0.2 env values:

```env
PUID=<host uid>
PGID=<host gid>
QUIP_HOSTNAME=:20049
CERT_EMAIL=<optional>
ZEROSSL_API_KEY=<optional>
DWAVE_API_KEY=<optional>
POSTGRES_PASSWORD=<generated>
QUIP_MINER_TAG=v0.2-preview
QUIP_DASHBOARD_TAG=v0.2-preview
QUIP_VALIDATOR_TAG=v0.2-preview
QUIP_MINER_CPUSET=0
VALIDATOR_NAME=<node name or default>
```

For Docker miner:

```env
QUIP_VALIDATORS=ws://quip-validator:9944
QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944
```

For Native / Physical Metal miner:

```env
QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944
```

Do not emit stale v0.1 values:

- `QUIP_NODE_URL`
- `QUIP_NODE_TOKEN`

The dashboard has fully migrated from `QUIP_NODE_URL` to
`QUIP_VALIDATOR_RPC_URLS`. Do not emit temporary compatibility env vars for the
old dashboard contract.

## Physical Metal Miner Vs Docker Miner

### Docker Miner

Use this for CPU and CUDA containerized mining.

- Compose runs miner, validator, bootstrap, dashboard, postgres, and caddy.
- Miner config points at `ws://quip-validator:9944`.
- Miner REST is internal at `quip-miner:80`.
- Caddy exposes `/api/v1/*` externally through public `20049`.
- Bootstrap sidecar handles registration/funding.

### Physical Metal Miner

Use this for macOS Metal/MPS mining where the miner must run on the host.

- Native binary should be the v0.2 `quip-miner`, not old `quip-network-node`.
- Docker compose still runs validator, dashboard, postgres, and caddy.
- Compose must omit miner containers and bootstrap.
- Startup brings the Docker support stack up before launching the native miner,
  so the local validator RPC route is available.
- Native miner startup waits for the host-visible validator RPC route to answer a
  JSON-RPC probe before launching the miner, avoiding a compose/Caddy readiness
  race where the miner can fail before manual checks succeed.
- Native miner config uses `[metal]`.
- Native miner points at the local validator through Caddy's host-reachable
  `/rpc` route on the configured public API port.
- Caddy `/api/v1/*` should proxy to `host.docker.internal:<native_rest_port>`.
- Dashboard still talks to substrate RPC through the Docker validator.
- Native miner launches as
  `quip-miner <cpu|gpu|qpu> --config <toml> --signer-key <keystore>
  --faucet-url https://faucet.testnet.quip.network`.
  Other miner values come from the rendered config or binary defaults.
  `--config` is a subcommand option, not a top-level `quip-miner` option.
- Native miner first-run startup runs
  `quip-miner keygen --out <data_dir>/keystore.json` when the configured
  signer key is missing. Existing keystores are never overwritten.
- Native miner release assets for `v0.2-preview`:
  - `quip-miner-macos-arm64`
  - `quip-miner-macos-x86_64`

The host-visible validator RPC path is the Caddy `/rpc` route on
`127.0.0.1:<public-api-port>`. A direct loopback validator RPC publish was
tested and reverted because it did not change the miner's
`validators-unreachable ... AttributeError` failure.

## Port Handling

### Public API Port

Keep the existing user-facing `node_config.port`, but reinterpret it in v0.2 as
the public Caddy/API/dashboard/RPC port. Default remains `20049`.

Patch compose:

```yaml
ports:
  - "<node_config.port>:20049"
```

Checklist should continue probing this port, but it should use the TCP
`/checkport` path rather than the old QUIC `/checkconn` semantics, because
`20049` is Caddy HTTP/WS now.

### Validator P2P Port

Add a separate validator host port setting, default `30033`.

Patch compose:

```yaml
ports:
  - "<validator_port>:30333/tcp"
  - "<validator_port>:30333/udp"
```

Add a checklist item that checks reachability of this port and local firewall
state. This check should warn but must not block startup.

Startup behavior:

- `20049` conflict can remain a startup-relevant warning because Caddy will fail
  if the port is occupied.
- `30033`/validator reachability should be advisory. The validator can still
  run and mine outbound through bootnodes, but it becomes a less useful peer.

## v0.1 To v0.2 Upgrade Flow

Implement a Rust-native migration rather than shelling out to Python.

### Detection

For Docker mode, inspect:

- `<data_dir>/data/config.toml`

For Native mode, inspect:

- `<data_dir>/config.toml`

Classify:

- v0.1: has `[global]` and not `[miner]`
- v0.2: has `[miner]` and not `[global]`
- ambiguous: has both, require manual review
- unknown: neither, require manual review

### Backup

Mirror upstream converter behavior:

- create `.v0.1_backup`
- move existing entries into the backup
- write a fresh v0.2 `config.toml`

For the manager, prefer a collision-safe backup:

- if `.v0.1_backup` exists, treat migration as already run when current config
  is v0.2
- if `.v0.1_backup` exists and current config is still v0.1, create
  `.v0.1_backup.<timestamp>` or stop with a clear error

Do not delete operator data.

### Config Conversion

Carry over:

- `[global].node_name` -> `[miner].node_name`
- `[global].public_host` -> `[miner].public_host`
- `[global].public_port` -> `[miner].public_port`
- `[global].rest_host` -> `[miner].rest_host`
- `[global].log_level` -> `[miner].log_level`
- `[global].node_log` -> `[miner].node_log`

Force:

- `[miner].validators = ["ws://quip-validator:9944"]` unless the app has an
  explicit remote-validator setting
- `[miner].signer_key = "/data/keystore.json"` in Docker mode
- `[miner].rest_port = 80` in Docker mode

Preserve backend tables verbatim:

- `[cpu]`
- `[gpu]`
- `[cuda.N]`
- `[nvidia.N]`
- `[metal]`
- `[modal]`
- `[qpu]`
- `[dwave]`
- quantum provider tables

Warn and drop:

- `[global].listen`
- `[global].port`
- `[global].peer`
- miner TLS keys
- TOFU/trust DB
- heartbeat/timeouts/fanout
- telemetry tables and file telemetry keys

### `.env` Migration

If an existing `.env` is present:

- back it up to `.env.v0.1_backup`
- remove `QUIP_NODE_URL`
- remove `QUIP_NODE_TOKEN`
- add a commented `QUIP_VALIDATOR_RPC_URLS` placeholder if absent

The manager overwrites `.env` on start today, so this primarily protects users
and support/debug workflows.

### Trigger Point

Run migration before first v0.2 start:

1. stop old v0.1 containers
2. sync v0.2 stack assets
3. migrate config and env if needed
4. write current manager-generated v0.2 config
5. start compose / native miner

Expose migration warnings in `node-log` and consider a UI event later if the
warnings need first-class treatment.

## UI Changes

Update labels to match v0.2 concepts.

- Rename "Node Configuration" to miner/validator wording where appropriate.
- Replace "Port (UDP+TCP)" with "Public API Port" for `20049`.
- Add "Validator P2P Port" with default `30033`.
- Remove or hide obsolete v0.1 controls:
  - bootstrap peers
  - auto-mine
  - verify TLS for miner
  - miner TLS cert/key
  - old REST HTTPS/insecure split
  - telemetry directory
  - heartbeat/timeouts/fanout
- Keep QPU configuration, but make clear it activates D-Wave mining on top of
  CPU miner mode.
- Change dashboard default URL from `localhost:20080` to `localhost:20049`.
- Update production hostname help for Caddy's comma-separated form:
  - `example.com, example.com:20049`

## Update Checks

Update image references:

- CPU miner: `quip-protocol/quip-miner-cpu`
- CUDA miner: `quip-protocol/quip-miner-cuda`
- Validator: `quip-validator/quip-network-node`
- Dashboard: `dashboard.quip.network`

Digest checks should use the configured tags, not hardcoded `latest`, once the
app supports v0.2 preview/version tags.

Native update and installation uses the `v0.2-preview` release assets from
`quip-protocol`, not the old `quip-network-node-*` native assets.

## Verification Plan

### Unit Tests

- `config.rs` renders `[miner]`, not `[global]`.
- Docker config includes:
  - `validators = ["ws://quip-validator:9944"]`
  - `signer_key = "/data/keystore.json"`
  - `rest_port = 80`
- Native Metal config includes `[metal]` and host-local signer/rest paths.
- v0.1 migration converts fixtures for CPU, CUDA, QPU/D-Wave.
- Migration preserves `public_host` and `public_port`.
- Migration is idempotent on already-v0.2 config.
- Compose patch remaps public API port.
- Compose patch remaps validator p2p TCP and UDP ports.

### Compose Smoke Tests

Run `docker compose config` against staged files for:

- Docker CPU
- Docker CUDA
- Native / Physical Metal support services
- custom public API port
- custom validator p2p port

### Manual Checks

- Start Docker CPU stack.
- Verify dashboard at `http://localhost:20049/`.
- Verify `/rpc` proxies to validator.
- Verify `/api/v1/*` proxies to miner.
- Verify bootstrap sidecar exits successfully.
- Verify validator appears in `get_stack_status`.
- Verify port checklist warns but does not block when `30033` is unreachable.
- On macOS, start Physical Metal miner with Docker validator/dashboard.

## Logged Decisions

- Use `30033` as the manager default for the host-exposed validator P2P port.
  It maps to upstream's internal validator port `30333`.
- Set validator `--public-addr` from `public_host` when it is configured,
  using the host-exposed validator port.
- Native miner installation/update uses the `v0.2-preview` release assets:
  `quip-miner-macos-arm64` and `quip-miner-macos-x86_64`.
- The dashboard has fully migrated from `QUIP_NODE_URL` to
  `QUIP_VALIDATOR_RPC_URLS`; no temporary compatibility env vars are needed.
- Dashboard-disabled support is not part of the v0.2 manager flow. The
  dashboard, Postgres, and Caddy remain part of the managed stack.
- Rename `dashboard_hostname` to `hostname`, while accepting legacy settings
  files that still contain `dashboard_hostname`. The hostname is shared by all
  services because Caddy proxies the stack. A DNS `public_host` takes
  precedence when it is suitable for Caddy; IP addresses fall back to the
  explicit hostname.
- Do not use a local faucet. The miner bootstrap uses the public testnet
  faucet, and the manager should not proxy `/api/faucet/*`.
