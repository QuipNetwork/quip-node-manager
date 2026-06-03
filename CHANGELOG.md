# Changelog

> **Note:** Because we are so new, we do not have Microsoft and Apple developer accounts activated yet so that you can install these apps without warnings from your Operating System. We are actively going through the identification for that now, and should have this resolved in the next month.

## Quick Install

**macOS / Linux:**

```sh
curl -fsSL https://gitlab.com/quip.network/quip-node-manager/-/raw/v0.2.0-rc6/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://gitlab.com/quip.network/quip-node-manager/-/raw/v0.2.0-rc6/scripts/install.ps1 | iex
```

## Manual Install

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

---

## v0.2

- **Bundled local validator**: every Docker stack now runs a Substrate block-producing validator (`quip-validator`) alongside the miner, dashboard, postgres, and Caddy. The miner self-bootstraps on first start — it funds your keystore from the testnet faucet and registers it in `QuantumPow.Miners` — so no separate bootstrap container is needed. The miner talks to the validator over RPC — `ws://quip-validator:9944` in Docker mode, or `ws://127.0.0.1:9944` (published on the host loopback) when the miner runs natively on macOS. The manager waits for the validator's RPC to come up before starting a native miner.
- **Automatic v0.1 → v0.2 config migration**: on first start after upgrading, the app rewrites your old `config.toml` from the `[global]` schema to the new `[miner]` schema and migrates your `.env`, backing up the originals to `.v0.1_backup/` (and `.env.v0.1_backup`) first. Your hand-edited public host, public port, node name, log level, and log path are carried forward; obsolete v0.1 keys and the old dashboard `QUIP_NODE_URL`/`QUIP_NODE_TOKEN` env vars are dropped. Migration is logged to the console and refuses to run (rather than clobber) if a backup already exists.
- **Live image pull progress**: the Logs tab now shows per-image progress bars while `docker compose pull` downloads the stack, instead of streaming raw layer output into the console.
- **Configurable validator P2P and RPC ports**: the Validator/Miner settings now let you set the validator's libp2p peering port (default 30333) and, in native mode, the host port the local miner uses to reach the validator's JSON-RPC (default 9944).
- **Native miner binary renamed to `quip-miner-*`**: macOS native mode now downloads and runs `quip-miner-macos-arm64` / `-x86_64`. Any leftover `quip-network-node-*` binary from v0.1 is removed automatically on launch to reclaim disk space.
- **Run Mode is driven by the Metal toggle on macOS**: the explicit Docker/Native selector is removed. On macOS, turning Metal GPU mining on runs the native miner; turning it off runs CPU mining via the Docker stack. Windows and Linux always use Docker.
- **Host-published vs container-internal ports decoupled**: the validator's host port (default 30333, matching the container's fixed 30333) stays independently configurable, and when you set a public host the validator advertises a matching libp2p address automatically. The native miner's REST always binds to the host loopback; only the public-facing API port is user-configurable.
- **Pre-flight checklist rewritten around the two internet-facing ports**: the checklist now runs exactly two external reachability probes — the Public API port and the Validator P2P port. There is no separate image-availability check: Start always runs `docker compose pull`. The two external probes are cached for 5 minutes (cleared by a Retry click) so repeated checks don't trip check.quip.network's rate limit. The "Recheck All" button is now "Retry All".
- **Settings panel simplified**: the old per-node networking knobs (listen address, bootstrap peers, timeout/heartbeat/fanout, REST host/port, telemetry directory, TLS cert file paths, auto-mine, and verify-TLS) are no longer surfaced in the UI now that the validator and Caddy own those concerns. The TLS panel's hostname field is now the Caddy/API hostname (`:20049` for local HTTP, or `example.com, example.com:20049` for public Let's Encrypt TLS).
- **Start/Stop reuse containers**: Stop runs `docker compose stop` (containers stopped and kept) and Start runs `docker compose up -d --remove-orphans` — no `docker compose down`, so a restart reuses the existing containers and compose only recreates what actually changed. `--remove-orphans` reaps services the compose file no longer declares (e.g. the removed bootstrap container). Force-removing containers by name now happens only as a one-time fallback if `up` fails on a name conflict (a leftover from an older version), so a normal start never `docker rm`s anything. Named volumes are preserved throughout.
- **Local dashboard/API access hardened**: a non-DNS hostname (localhost, an IP, or a bare/edited port) no longer leaks into the Caddy site address, where it would switch Caddy to automatic HTTPS on the wrong port and break plain-HTTP local access. Such values normalize to a port-only `:20049` site that serves any host over HTTP; only a real public DNS name provisions TLS.
- **GPU sharing via NVIDIA MPS (Linux)**: the CUDA miner is now MPS-ready (`ipc:host`, `pid:host`, an MPS pipe mount, and a SM cap from your GPU utilization setting). On a native Linux GPU host the manager starts the host MPS control daemon before the stack so the miner shares the GPU's SMs in hardware; `pid:host` also enables NVML process-yielding. Harmless where MPS isn't available — note MPS is unsupported under WSL2/Docker Desktop, so Windows hosts fall back to software throttling.

---

## v0.1.1

- **Full compose stack**: replaces the single `docker run quip-node` path with `docker compose` orchestration of the upstream `nodes.quip.network` stack (node + dashboard + postgres, optional Caddy for TLS). Container names now match the reference (`quip-cpu` / `quip-cuda` / `quip-qpu`) with a `quip-node` network alias for dashboard discovery.
- **Dashboard tab embedded**: the running dashboard container's UI is rendered in-app on the Dashboard tab via an iframe; URL derived from settings (plain HTTP on localhost, ACME HTTPS when a DNS hostname is configured).
- **Stack Configuration panel**: dashboard toggle, TLS toggle, and TLS subsettings (hostname, ACME email, ZeroSSL key). Image type is auto-derived from GPU configuration — CUDA when any NVIDIA device is enabled, CPU otherwise. QPU mining rides on the CPU image via `[dwave]` config.
- **Native mode hybrid** (macOS): native binary still runs the node on the host; the compose stack supplies dashboard + postgres (+ Caddy if TLS), wired to the host via `host.docker.internal` with a Caddyfile patched at stage time. Node REST binds 127.0.0.1 so the port isn't LAN-reachable.
- **Multi-service status**: `get_stack_status` parses `docker compose ps` (both JSONL and JSON-array outputs) and reports per-service running/health state plus a rolled-up `Running | Degraded | Unhealthy | Stopped`.
- **Multi-image update monitor**: per-image digest polling for the node image and the dashboard image; auto-update stops and restarts the stack as a unit.
- **Config.toml inside compose bind-mount**: `write_config_toml` now targets `~/quip-data/data/config.toml` in Docker mode so the node container sees it. Previously landed outside the `./data:/data` bind-mount, causing the node to fall back to `num_cpus = os.cpu_count()`.
- **GPU backend gating**: `[metal]` / `[modal]` sections are only emitted when at least one GPU device is enabled, mirroring the `[cuda.N]` gating. Metal is also suppressed in Docker mode — it can't run in a Linux container.
- **`confident_lehmann` rogue node**: `get_node_version` no longer runs `docker run --rm <image> --version` in Docker mode. The image entrypoint didn't exit on `--version`, so the anonymous container became a live node alongside the compose stack. `stop_stack` also sweeps orphan node-image runners after `docker compose down`.
- **Stop Node reliability**: `stop_stack` force-removes each of the six declared container names (`quip-cpu`/`cuda`/`qpu`/`dashboard`/`postgres`/`caddy`) after `docker compose down` as a backstop for project-label mismatches.
- **Docker Compose detection**: check now uses exit-status rather than string-matching `"Docker Compose version v2."`. Docker 29 ships Compose v5, which broke the previous parse.

### Pre-flight checks

Seven new profile-aware check items replace the old `image` / `port` checks:
- `docker-compose` — Docker Compose v2+ plugin installed
- `stack-assets` — compose.yml + Caddyfile staged in `~/quip-data/`
- `stack-images` — all images the current profile needs are pulled
- `port-dashboard` — TCP 20080 bindable (when dashboard on, TLS off)
- `port-tls` — TCP 80 + 443 bindable (when TLS on)
- `rest-port-native` — native REST port free on the host (Native + dashboard only)
- `dwave-key` — D-Wave API token set when `[dwave]` is configured

---

## v0.1

- **WSL pre-flight check (Windows)**: No longer falsely reports "WSL not installed" for non-admin users with Microsoft Store WSL. Detection now probes `wsl --list --verbose`, `wsl --version`, and `wsl --status` in sequence, and decodes the UTF-16LE output `wsl.exe` emits on Windows. The check is also demoted from a blocking requirement to a warning, since Docker Desktop's own check already fails first if WSL2 is truly missing.

## v0.0.7

- **Native mode restricted to macOS**: Run Mode toggle is now only shown on macOS. Windows and Linux default to Docker mode with no option to switch. Backend enforces this on both load and save.

## v0.0.6

- **WSL pre-flight check (Windows)**: Docker mode now verifies WSL is installed with a distro before starting, with actionable fix instructions
- **External links open in system browser**: Links in the app now open in the default browser instead of being swallowed by the webview (via tauri-plugin-opener)
- **UDP+TCP firewall checks**: Firewall and port forwarding checks now verify both UDP and TCP on all platforms, reporting exactly which protocol is missing
- **CLI firewall instructions**: Added step-by-step firewall setup (ufw on Linux, New-NetFirewallRule on Windows) and router forwarding notes to CLI docs
- **Automated release notes**: CI now reads release description from CHANGELOG.md (install instructions + current version's changelog)

## v0.0.5

- **New app icon**: Updated to quipv4 across all platforms (window, tray, macOS/Windows/Linux/iOS/Android bundles)

## v0.0.4

- **Auto-detect public IP at node start**: When `public_host` is not explicitly configured, the app detects the external IP and writes it to `config.toml` before starting the node. This ensures peers can connect back without manual configuration. Applied to all three start paths: Docker, Native, and TUI.
- **Fix CI release job**: The `release-cli` flag `--assets-links` (plural) was not recognized; changed to `--assets-link` (singular, repeated per asset). Removed broken `jq` fallback.
- **Fix CI bundle copy with cached artifacts**: Clean `src-tauri/target/release/bundle` before building to prevent stale artifacts from prior versions breaking the glob copy step.
- **Make CI release job idempotent**: Release creation now handles the case where a release already exists for the tag.

## v0.0.3

- **Fix Windows firewall check**: replaced invalid `localport=` netsh filter with proper `name=all dir=in` and block-based output parsing
- **Fix Docker log streaming on Windows/Linux**: replaced `sh -c "docker logs"` (no `sh` on Windows) with direct `docker logs` call and separate stdout/stderr threads
- **Fix version display**: was stuck at v0.0.0, now reads from Cargo.toml at compile time
- **Fix app bundle name on macOS**: now builds as `Quip Node Manager.app`
- **Migrate all URLs from piqued to quip.network namespace**: fixes version checks, binary downloads, and registry lookups
- **Add timeout to binary --version check**: prevents hanging when the binary doesn't respond
- **Node version display**: shows protocol version next to app version (e.g. `v0.0.3 (node 0.0.4)`)
- **Version check in pre-flight checklist**: warns when node binary/image is outdated with an Update button
- **Unified log streaming**: both Docker and Native modes tail `node.log` for real mining activity, with fallback to docker logs or process stdout until node.log appears
- **Network checks are warnings, not blockers**: public IP, hostname, port forwarding, firewall, and version checks no longer prevent starting the node
- **Instant startup**: UI renders immediately; all backend calls run in background
- **Parallel checklist**: network checks run concurrently via `tokio::join!`
- **Parallel hardware survey**: GPU, Docker, and Python detection run in parallel threads
