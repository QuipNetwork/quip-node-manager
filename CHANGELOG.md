# Changelog

> **Note:** Every download is signed. The macOS builds carry an Apple Developer
> ID signature and Apple notarization, so they install with no Gatekeeper
> warning and no Terminal command. The Windows builds carry an Authenticode
> signature issued to HADAMARD GATE INCORPORATED. Each release also publishes
> `SHA256SUMS` and an OpenPGP signature over that file. See
> [Verify Your Download](#verify-your-download).

## Quick Install

**macOS / Linux:**

```sh
curl -fsSL https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.ps1 | iex
```

## Manual Install

### macOS

Download the `.dmg`, open it, and drag the app to `/Applications`.

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

A signed `.rpm` package is available for Fedora, RHEL and openSUSE:

```sh
sudo rpm --import https://gitlab.com/quip.network/quip-node-manager/-/raw/main/.gitlab/release-signing-key.asc
sudo rpm -i quip-node-manager-linux-x86_64.rpm
```

### Windows

Download the `.exe` and run it. Windows names HADAMARD GATE INCORPORATED as the
publisher. SmartScreen may still warn until the certificate builds reputation
across enough downloads. Click **More info**, then **Run anyway**.

## Verify Your Download

`scripts/install.sh` checks the checksum on every run, and checks the signature
as well when `gpg` is installed. To check a download by hand instead, expand the
section below.

<details>
<summary>Manual verification steps</summary>

### Checksums, every platform

Download `SHA256SUMS` and `SHA256SUMS.asc` from this release, then import the
release key and check both the signature and the checksum:

```sh
curl -fsSLO https://gitlab.com/quip.network/quip-node-manager/-/raw/main/.gitlab/release-signing-key.asc
gpg --import release-signing-key.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

On macOS, use `shasum -a 256 --check --ignore-missing SHA256SUMS` for the last
step.

`gpg --verify` must report a good signature from this key:

| Item | Value |
|------|-------|
| Key | `Quip Node Manager Release Signing <rick@postquant.xyz>` |
| Fingerprint | `A63860E21E7070C2C26FDA5DC85BEAB01AD9FEE3` |
| Type | RSA 4096, expires 2029-09-16 |

GnuPG reports the key as untrusted until you sign it yourself. That warning is
expected. Compare the fingerprint in the output against the value above.

### macOS

```sh
xcrun stapler validate quip-node-manager-macos-universal.dmg
spctl --assess --type open --context context:primary-signature -v quip-node-manager-macos-universal.dmg
```

`stapler` must report `The validate action worked`. `spctl` must report
`accepted` and `source=Notarized Developer ID`.

### Windows

```powershell
Get-AuthenticodeSignature .\quip-node-manager-windows-x86_64.exe | Format-List
```

`Status` must read `Valid`, and `SignerCertificate.Subject` must name
`HADAMARD GATE INCORPORATED`.

### Linux RPM

```sh
sudo rpm --import release-signing-key.asc
rpm --checksig quip-node-manager-linux-x86_64.rpm
```

The output must read `digests signatures OK`. An unsigned package reports
`digests OK` and still exits 0, so read the words, not the exit status.

The `.deb` and the AppImage carry no embedded signature. Check those two
against `SHA256SUMS` as shown above.

</details>

---

## v0.2.10-rc1

- **The install script checks the release signature again**: `install.sh` verified the signature on `SHA256SUMS` against a key it believed it had isolated in a temporary keyring. GnuPG ignores that instruction wherever keyboxd is enabled, so on those systems the check could not read the key back and stopped the install with a fingerprint mismatch that named no fingerprint. The key is now kept in a temporary home directory that every GnuPG version honours, and it no longer lands in your own keyring. Releases from v0.2.9 were signed correctly throughout; only the checking was broken.

- **A failing checksum tool no longer reads as a corrupt download**: if `sha256sum` or `shasum` could not run, `install.sh` reported a checksum mismatch and discarded the artifact. It now says the tool failed.

- **One progress bar during the download**: the download drew two overlapping progress bars; it now draws one.

- **D-Wave mining finds its API token**: the manager now gives the miner its D-Wave token, solver and region as `DWAVE_API_TOKEN`, `DWAVE_API_SOLVER` and `DWAVE_API_REGION`. Before, the D-Wave miner stopped at startup with "API token not defined" even when you entered a valid token. Native mode now passes these values to the miner too. `config.toml` no longer holds the token.

---

## v0.2.9

- **Unset solvers default to `msa` when it is installed**: when you leave a solver on Default, Start now runs the `msa` build for that backend if the miner image or bundle has one. Otherwise Start runs `sa`, as before. A solver that you choose does not change. The Default entry in the desktop app and the terminal UI shows the solver that Start will run.

- **Windows builds are signed with the company certificate**: the Windows executable now carries an Authenticode signature from SSL.com, issued to HADAMARD GATE INCORPORATED. Windows shows that name as the publisher instead of an unknown-publisher warning.

- **Linux releases publish a signed RPM**: Fedora, RHEL and openSUSE users can install an RPM and check it with `rpm --checksig`, after importing the release key.

- **Every release publishes SHA256SUMS and a signature over it**: the install script checks the checksum of what it downloaded, and checks the signature as well when `gpg` is installed. A mismatch deletes the download and stops the install.

- **Release tags must be signed**: CI verifies the signature on each `v*` tag against a list of keys tracked in the repository, and refuses to build a release from a tag it cannot verify.

## v0.2.9-rc3

- **Windows builds are signed with the company certificate**: the Windows executable now carries an Authenticode signature from SSL.com, issued to HADAMARD GATE INCORPORATED. Windows shows that name as the publisher instead of an unknown-publisher warning.

- **Linux releases publish a signed RPM**: Fedora, RHEL and openSUSE users can install an RPM and check it with `rpm --checksig`, after importing the release key.

- **Every release publishes SHA256SUMS and a signature over it**: the install script checks the checksum of what it downloaded, and checks the signature as well when `gpg` is installed. A mismatch deletes the download and stops the install.

- **Release tags must be signed**: CI verifies the signature on each `v*` tag against a list of keys tracked in the repository, and refuses to build a release from a tag it cannot verify.

## v0.2.9-rc1

- **Unset solvers default to `msa` when it is installed**: when you leave a solver on Default, Start now runs the `msa` build for that backend if the miner image or bundle has one. Otherwise Start runs `sa`, as before. A solver that you choose does not change. The Default entry in the desktop app and the terminal UI shows the solver that Start will run.

## v0.2.8

- **The CUDA and Metal solver selectors appear in the desktop app**: the app drew these selectors before the hardware check finished. Only the CPU selector was visible. You can now choose `gibbs` or `sa` for CUDA and Metal.

- **The terminal UI retries a failed solver list**: after a failed first read, the list held only the current solver until a restart. The next press now reads the list again.

## v0.2.7

- **The D-Wave budget is monthly, and the miner enforces it again**: the D-Wave miner in quip-miner v0.3.1 and later reads only `budget` and `budget_reset_day`. The manager wrote `daily_budget` instead, so a node with a Daily Budget set had no spending limit. Settings now has a Monthly Budget field, such as `40h`, and a Reset Day field, the day of the month in UTC. The manager writes both, plus a spend ledger in the data directory that survives a restart.

- **Set your D-Wave budget again**: the manager does not convert the old Daily Budget value. Enter the QPU time of your D-Wave plan for one quota period in Settings, with the day the quota resets. With no budget, the miner spends without a limit.

## v0.2.6

- **The stack log is one merged file**: every container writes through a collector into `data/logs/quip-node.log`, and the log pane tails that file. The file outlives a container replacement and keeps slashes and tabs. The reader follows it across the 10 MB rotation. The embedded stack is nodes.quip.network v0.3.2.

- **Logs stay visible during startup**: the log pane shows container output through waits and failed starts. It reconnects while the Compose files stage or Docker restarts.

- **Each service has its own host ports**: both interfaces group the validator, miner, dashboard, PostgreSQL, and Caddy host ports by service, each with a switch. When public ports are off, the health checks use the internal network. The manager rejects conflicting host ports.

- **Each GPU and CPU backend has a solver selector**: the CPU, CUDA, and Metal selectors read their lists from the miner image or the native bundle. The options match what the coordinator can start.

- **An initial chain sync reads SYNCING, not DEGRADED**: the status pill shows a progress bar with the block counts.

- **The Windows health probes work**: the embedded shell scripts had carriage-return line endings, so every probe failed and the validator stayed unhealthy.

- **The headless TUI matches the GUI**: settings, checks, status, updates, and log actions are the same in both. The TUI also writes the GPU backend it detects, so a Metal Mac gets a `[metal]` section.

- **Start refuses a config with no mining backend**: the manager names the cause before it writes the file. A Start before the hardware survey answers keeps the saved GPU list.

- **The Public API check probes the advertised port**: with "Override Public Host & Port" set, the check probes the port that peers dial.

---

## v0.2.6-rc7

- Stop the macOS release build from changing the runner's keychain
  configuration. The tag build made its job keychain the default keychain
  and added it to the user search list, and never put either back, so a
  developer Mac acting as a runner was left with `quip-ci` as its default
  keychain. The build now names its keychain on every `security` call and
  routes `codesign` through a wrapper that passes `--keychain`, writes no
  keychain preference, and deletes the keychain when the job ends.

## v0.2.6-rc6

- Make the Public API reachability check probe the port peers are told to
  dial. With "Override Public Host & Port" set, the check named and probed
  the Service Ports host port instead of the override, so a router that
  remaps the external port always reported "not reachable". The check now
  targets the advertised port, keeps its temporary listener on the host
  port, and shows both when they differ.
- Explain what the override port and the Caddy hostname port do. The
  override changes only the address peers dial, and the Caddy port is fixed
  at 20049 inside the container. Both fields now point to Service Ports as
  the place that moves the listening port. The TUI shows the same hint.

## v0.2.6-rc5

- Bring the headless TUI to parity with the GUI. Settings: a CPU mining
  switch, the TLS fields (hostname, ACME email, ZeroSSL key), the Metal
  adaptive-cap knobs, and a Save button that does not restart the stack.
  Checks: a Retry button on every item plus the Docker install fix. Status:
  the three health dimensions under the status line and the installed miner
  version. Actions: Check Updates with Update & Restart, and Reset Dashboard
  DB behind a two-press confirmation. Logs: a filter (`/`) and clear (`c`).
- Fix the TUI never writing the GPU backend it detected. A Metal Mac that only
  ever used the TUI kept the CUDA default, so config.toml got a `[cuda.N]`
  section for its GPU instead of `[metal]`. Start and Apply now set it from the
  hardware survey, as the GUI does.
- Generate the node secret through the same function in both front ends. The
  TUI had its own copy of the logic.

## v0.2.6-rc4

- Refuse to start when no mining backend is on. The miner rejects a
  config with no `[cpu]`, `[cuda.N]`, `[metal]`, or `[dwave]` table, and its
  error blames the v0.2 config format because `faucet_url` is present. The
  manager now names the cause before it writes the file: turn on CPU mining or
  enable a GPU.
- Keep the saved GPU list when the hardware survey has not answered. The GUI
  rebuilt the list from the toggles on the page, so a Start before the survey
  landed dropped every GPU from config.toml.
- Restore the GPU Yielding switch after a restart on macOS. The form read the
  CUDA slot before the hardware survey reported Metal, so the switch showed off
  and the next Save wrote that into the Metal tuning.
- Settings sliders: TLS (renamed from "Enable TLS/Caddy"), CPU mining, the
  public host override, and the validator RPC public-access choice are sliders.
  The override shows the detected address and the Public API port while off
  and reveals the fields when on. The public-access slider sits before the
  port and explains loopback versus all interfaces.

## v0.2.6-rc3

- Read the stack log from one merged file. Every container now writes through a
  collector into `data/logs/quip-node.log`, and the log pane tails that file
  instead of merging the output of `docker compose logs` itself. The merged file
  outlives a container replacement, which the per-container logs did not.
- Start the collector in both run modes, and stage the two files it mounts.
  Without the collector, every service sends its output to a port that has no
  listener. Nothing reports an error, so the file simply stays empty.
- Keep the merged file readable. The collector rewrote every `/` and every tab
  to `_`, which mangled each address and path the stack logs and hid the level
  on Caddy error lines. Both survive now.
- Follow the merged file across a rotation. The reader compared file length
  alone, so after the collector rotated at 10 MB it could keep reading the
  renamed file and show nothing further until the next restart.

## v0.2.6-rc2

- Repair the container health probes in the Windows builds, which failed on
  every call. The Windows build machine checked out the embedded shell scripts
  with carriage-return line endings, so bash rejected `set -euo pipefail` and
  exited before it opened a socket. The staged validator healthcheck carried
  the same fault, which held the validator at unhealthy and kept the miner and
  the dashboard waiting behind their `service_healthy` conditions.
- Report an initial chain sync as SYNCING instead of DEGRADED. The status pill
  shows a progress bar and the block counts read from `system_syncState`.
  A syncing node no longer counts as unhealthy for missing a participation
  marker it cannot yet have.
- Add solver selectors for the CPU, CUDA, and Metal backends in both interfaces.
  Each selector reads its list from the miner image or the native bundle at
  runtime, so the options match what the coordinator can start. Entries carry
  the algorithm name and the production or experimental track. A selector left
  at Default keeps the binary that the image chooses.

## v0.2.6-rc1

- Show container logs during stack startup. Keep logs visible through waits and failed starts.
  Retry the log connection while Compose files are staging or Docker reconnects.
  Replacing a log session permanently stops its previous follower.
- Remove the loopback RPC fallback from the staged Docker miner template.
  The miner uses `quip-validator:9944` regardless of public host port settings.
- Add host port switches and port fields grouped by service in both interfaces.
  These cover validator P2P, RPC and metrics, miner REST, dashboard HTTP,
  PostgreSQL, and Caddy listeners. Internal container ports stay fixed.
- Keep Native validator RPC required, with port 9944 as the default.
  Public RPC access is optional. Native miner REST has a separate editable host port.
- Check Docker service health through the internal network when public ports are off.
  Reject conflicting host ports and keep the previous mode when a mode change fails.

## v0.2.5

- **The miner asks the faucet for the network it actually runs**: the faucet URL followed the update channel, which was correct only while the two channels ran two different networks. With the Aglais images on the stable branches, both channels join Aglais, and a stack on Release kept asking the retired network's faucet. That failure is quiet: the miner has no built-in default and retries instead of exiting, so the balance stays at zero and the log reads like a slow faucet. The URL now follows the embedded chain specification, which is the thing that decides the network.

- **The validator no longer starts on a pre-Aglais image**: an operator upgrading from the pre-reset testnet got a validator that crash-looped with `GRANDPA DB is corrupted: Could not decode ... Public.0`, on a database it had just created. The Release channel took the newest tag with no `-rc` suffix, which for the validator was `v0.2.2` — a build that predates the relaunch and cannot decode the authority keys the Aglais genesis pins. The chain specification now carries a minimum validator version, and no channel resolves below it, including a tag pinned by an earlier run.

- **The update channel names build freshness, not a network**: Release takes stable tags and Beta also takes release candidates. Both join the network the embedded chain specification names. The retired testnet is no longer selectable, because the app carries no specification that can join it.

---

## v0.2.5-rc4

- **Participation health reads the account the node signs with**: the health check took the miner account from a field in `keystore.json` that nothing writes any more. It then queried an account that had never participated. A node that was mining normally showed `no participation marker on chain`. The account now comes from the running coordinator — the process that signs the extrinsics.

- **v0.2.5-rc3 has no downloads**: the Linux bundle for that tag never built. Its release job timed out after an hour while installing build dependencies, so no release was published. This version carries the same fix.

---

## v0.2.5-rc3

- **Participation health reads the account the node signs with**: the health check took the miner account from a field in `keystore.json` that nothing writes any more. It then queried an account that had never participated. A node that was mining normally showed `no participation marker on chain`. The account now comes from the running coordinator — the process that signs the extrinsics.

---

## v0.2.5-rc2

- **The miner asks the right faucet for funds**: the rendered config named the testnet faucet on every channel. A node on Beta then requested funds from the network it does not run on, and its balance stayed at zero. Beta now uses the Aglais faucet. Release keeps the testnet faucet. An install upgrading from v0.1 gets the same correction.

---

## v0.2.5-rc1

- **The stack tracks the Aglais relaunch**: the bundled compose now uses the Aglais chain spec. It also adds a miner config template and a validator sync healthcheck, both of which the manager stages. The retired `quip-testnet.json` spec is gone.

- **Upgrading resyncs the validator and reindexes the dashboard**: the relaunch moved the validator data directory and renamed the Postgres volume, so the first start after this update replays the chain and rebuilds the dashboard index. The old directory and volume stay on disk until you delete them.

- **The node no longer waits for the validator to sync**: the miner and dashboard now start alongside the validator, rather than hours later at the chain head. The miner retries in the meantime.

- **Images are selected by channel**: the manager still resolves an exact version for each image from its own registry. It now records which channel it picked. A stack run by hand from the compose file follows a moving `beta` or `stable` tag instead. Beta is the default, because Release selects the older network that still runs in parallel.

- **D-Wave settings reach the miner**: the manager writes the token under the name the miner and the Ocean SDK actually read. It also pins the solver and the Leap region. The previous name is still written, so existing setups keep working.

---

## v0.2.4

### Build

- The macOS tag pipeline signs the application with a Developer ID Application
  certificate and notarizes it.
- `scripts/notarize-dmg.sh` notarizes and staples the disk image. Tauri signs
  the disk image but does not notarize it.
- `scripts/verify-macos-signing.sh` fails the pipeline when an artifact is not
  signed, hardened, notarized, and stapled. A build with absent credentials
  exits 0 and produces an unsigned application, so the pipeline checks the
  result rather than the exit code.
- `OSX.md` documents the signing flow that the repository uses, and no longer
  covers the manual Intel and Apple Silicon universal-binary build. The
  supported build target is Apple Silicon, M1 and newer, only.
- The macOS build job installs rustup itself when the assigned runner does not
  carry it, instead of failing with `command not found`.

---

## v0.2.4-rc1

### Build

- The macOS tag pipeline signs the application with a Developer ID Application
  certificate and notarizes it.
- `scripts/notarize-dmg.sh` notarizes and staples the disk image. Tauri signs
  the disk image but does not notarize it.
- `scripts/verify-macos-signing.sh` fails the pipeline when an artifact is not
  signed, hardened, notarized, and stapled. A build with absent credentials
  exits 0 and produces an unsigned application, so the pipeline checks the
  result rather than the exit code.
- `OSX.md` documents the signing flow that the repository uses.

## v0.2.3

- **The stack runs the v0.3 miner**: v0.3 replaced the per-backend miner images with one image for each accelerator, and each image carries a coordinator and every miner it supports. The manager now reads `quip-miner/v0.3/quip-miner` and `quip-miner/v0.3/quip-miner-cuda`. The image tag is the single version it tracks, because the miners inside move on their own cadence. A fresh install with no pinned tag asks for `v0.3.0-rc7`.

- **Native mode installs the v0.3 macOS bundle**: it fetches `quip-miner-darwin-arm64.tar.gz` and extracts the coordinator with every darwin-arm64 miner. The coordinator is the supervised process and starts the miners itself. The install strips the quarantine attribute, which otherwise kills the ad-hoc signed binaries with no output. The generated `config.toml` names each miner by absolute path, because the coordinator resolves a bare name through PATH.

- **Native mode requires an Apple Silicon Mac**: every other platform is pointed at Docker mode. The v0.3 images carry the CPU, D-Wave and CUDA miners, so the one capability Native mode adds is Metal, which reaches a GPU no container can.

- **The miner serves its REST surface**: the manager wrote `rest_host` and `rest_port`, which the v0.3 coordinator does not read, so the dashboard had no data source. The manager now writes a `[dashboard]` section, which is where v0.3 takes the listen address and the attempt-log directory.

- **The dashboard shows miner data in Native mode**: the miner listened on loopback only. The dashboard container reaches it through the Docker host gateway, which does not arrive on loopback. Every `/api/v1/*` request failed with a 502. The miner now listens on all interfaces in both run modes.

- **The miner advertises a reachable public address**: `config.toml` omitted `public_port` unless an override existed, so peers had no advertised port. The `public_host` detection ran only in Docker mode, so a Native miner advertised nothing. Both now run on every start path, and `public_host` resolves through check.quip.network, the same service behind the `ip` row in the pre-flight checklist.

- **The manager refuses to start when no peer can reach the resolved `public_host`**: it rejects loopback and unspecified addresses, RFC1918 private ranges, link-local, carrier-grade NAT, multicast, and reserved ranges. It also rejects IPv6 unique-local addresses, IPv6 link-local addresses, and mDNS `.local` names. A local network or air-gapped deployment that advertises a private address on purpose cannot start. No opt-in override exists yet.

- **The stack recovers when Docker leaves a container off its network**: such a container runs but publishes none of its declared ports, and `docker compose up` does not repair it. In Native mode this hit the validator, which stopped publishing `127.0.0.1:9944`, so the miner never started. The manager now inspects every expected service after `up` and recreates a container that holds no network endpoint.

- **Stop waits for the miner to shut down**: the app allowed two seconds, then told the user to stop the process by hand. The miner was often still shutting down. Stop now allows two minutes and reports each step. A stop that does not take says the miner may be stuck, and gives the exact command to force it.

- **Caddy writes readable log lines**: a single failed request used to fill twenty lines of JSON. Caddy now writes one line for each event, and the log pane colors it by level.

- **The terminal UI reports a partly started stack**: it read only the miner, so a dead miner beside four running support containers showed as STOPPED. It now reads the compose stack in both run modes and names the services that are up.

- **The terminal UI gained the settings it lacked**: each GPU checkbox controls its own device. The GPU limit reaches a macOS Metal miner through the `[metal]` section. The storage directory accepts a new path, and the title bar shows the real version instead of v0.1.0.

- **Release pages link to the released version**: the install commands on a release page pointed at the tip of `main`. They now point at the tag.

- **CI checks Rust formatting**: a `lint` stage runs `cargo fmt --check` against `src-tauri`.

- **Note for hand-edited configs**: a `config.toml` written for v0.2 does not start under v0.3. The coordinator requires a backend section, `public_host`, and `public_port`. Let the manager write the file, or copy the template the miner image ships at `/app/config.toml`.

---

## v0.2.3-rc7

- **The dashboard shows miner data in Native mode**: the miner listened on loopback only. The dashboard container reaches the miner through the Docker host gateway, which does not arrive on loopback. Every `/api/v1/*` request failed with a 502, and the dashboard stayed empty. The miner now listens on all interfaces in both run modes.

- **Caddy writes readable log lines**: a single failed request used to fill twenty lines of JSON. Caddy now writes one line for each event, and the log pane colors it by level. That line no longer carries the request header dump, the duplicate client address, or the internal error id.

- **Stop waits for the miner to shut down**: the app allowed two seconds, then told the user to stop the process by hand. The miner was often still shutting down. A second press appeared to work, because the first press had already discarded the record it needed for a retry. Stop now allows two minutes and reports each step. It keeps that record, so a second press still works.

- **A stop that does not take names the command to run**: the message says the miner may be stuck and gives the exact `kill` command for it. A failed Docker stop gives the matching `docker kill` command.

---

## v0.2.3-rc6

- **The terminal UI reports a partly started stack**: it read only the miner, so a dead miner beside four running support containers showed as STOPPED. The desktop app called the same state PARTIAL. The terminal UI now reads the compose stack in both run modes and names the services that are up. An unhealthy stack no longer shows as STOPPED.

- **Each GPU checkbox controls its own device**: every checkbox toggled the first GPU, so devices after the first did nothing.

- **The terminal UI GPU limit reaches Metal**: Apply wrote the limit only to the per-device list. A macOS Metal miner reads the `[metal]` section instead, so the setting had no effect. Apply now writes both. The adaptive-cap values stay as the desktop app left them.

- **The storage directory is editable from the terminal UI**: the old build showed the path but gave no way to change it. A headless install had no way to choose where its data goes. The field now accepts a path, and the app must restart before the change takes effect.

- **The terminal UI shows its real version**: the title bar said v0.1.0 on every build.

---

## v0.2.3-rc5

- **The stack recovers when Docker leaves a container off its network**: Docker can leave a container running with no endpoint on the project network. That container publishes none of its declared ports. `docker compose up` does not repair it, because the container already runs and its configuration has not changed, so compose only starts it.

  In Native mode this hit the validator, which stopped publishing `127.0.0.1:9944`. The miner start waits on that port for 90 seconds. The miner never started, and the stack sat at PARTIAL with no miner logs.

  The manager now inspects every expected service after `up`. It recreates a running container that holds no network endpoint, then checks once more. The log names any service that stays broken, instead of letting it surface later as a connection error.

---

## v0.2.3-rc4

- **Docker mode pulls the v0.3 miner images again**: an image update pinned a v0.3 tag. The compose file still named the v0.2 repositories, which stop at `v0.2.1-rc54`. Every pull then failed with `not found`, and no stack would start. Both miner services now point at the v0.3 repository line, and a fresh install with no pinned tag asks for `v0.3.0-rc7`.

- **The miner serves its REST surface again**: the manager wrote `rest_host` and `rest_port`, which the v0.3 coordinator does not read. The miner started, Caddy proxied `/api/v1/*` to a port nothing listened on, and the dashboard had no data source. The manager now writes a `[dashboard]` section, which is where v0.3 takes the listen address and the attempt-log directory. Docker binds port 8086 and Native binds port 20100 on loopback.

- **Note for hand-edited configs**: a `config.toml` written for v0.2 does not start under v0.3. The coordinator requires a backend section, `public_host`, and `public_port`. Let the manager write the file, or copy the template the miner image ships at `/app/config.toml`.

---

## v0.2.3-rc3

- **The miner advertises a public address again**: `config.toml` omitted `public_port` whenever no explicit override existed, so peers had no advertised port. The manager now always writes `public_port`, and falls back to the Caddy front door port, which is the port a peer reaches from outside the host.

- **`public_host` fills itself in on both run modes**: the detection ran only in Docker mode, so a Native miner advertised nothing. It now runs on every start path and resolves through check.quip.network, the same service behind the `ip` row in the pre-flight checklist. The checklist and the advertised address can no longer disagree.

- **The manager refuses to start when no peer can reach the resolved `public_host`**: it rejects loopback and unspecified addresses, RFC1918 private ranges, link-local, carrier-grade NAT, multicast, and reserved ranges. It also rejects IPv6 unique-local addresses, IPv6 link-local addresses, and mDNS `.local` names. A local network or air-gapped deployment that advertises a private address on purpose cannot start. No opt-in override exists yet.

- **Release pages link to the released version**: the install commands on a release page pointed at the tip of `main`, so they installed something other than the release. They now point at the tag.

- **CI checks Rust formatting**: a `lint` stage runs `cargo fmt --check` against `src-tauri`, which stops the formatting drift that produced diffs across files nobody touched.

---

## v0.2.3-rc2

- **Native mode installs the v0.3 macOS bundle**: the manager downloaded the single `quip-miner` binary that v0.2 published and v0.3 does not, so it had nothing to install. It now fetches `quip-miner-darwin-arm64.tar.gz` and extracts the coordinator with every darwin-arm64 miner. The coordinator is the supervised process and starts the miners itself.

  Three details decide whether this works at all. The extracted binaries are ad-hoc signed rather than notarized, so a quarantine attribute kills them with no output. The install strips that attribute. Each miner is named by absolute path in the generated `config.toml`, because the coordinator resolves a bare name through PATH and the install directory is not on it. Version detection reads the coordinator and ignores the protocol suffix in its output.

  Native mode now refuses to start anywhere except an Apple Silicon Mac, and points at Docker mode instead. The v0.3 images carry the CPU, D-Wave and CUDA miners, so the one capability Native mode adds is Metal, which reaches a GPU no container can.

- **Install links track `main`**: the Quick Install commands fetched the installer from the `v0.2.0` tag, two releases back. They now read it from `main`. The installer resolves the newest release itself, so the fetch URL needs no further bump.

## v0.2.3-rc1

- **Track the v0.3 miner images**: v0.3 replaced the per-backend miner images with one image per accelerator, each bundling the coordinator and every miner it supports. Those images live in new registry repositories, so the manager kept resolving the v0.2 line and reported Miner v0.2.1 on both channels. It now reads `quip-miner/v0.3/quip-miner` and `quip-miner/v0.3/quip-miner-cuda`, and the image tag is the single version it tracks — the miners inside move on their own cadence. Beta picks up `v0.3.0-rc1`.

  This build is itself a release candidate, so only the Beta channel offers it. The v0.3 repositories carry no stable tag yet, so switching a v0.3-tracking build to the Release channel leaves the miner image unresolved until a stable v0.3 image is cut.

  Native mode is unchanged and still installs the v0.2 binary from quip-miner releases. v0.3 publishes no such binary, so Native mode does not yet reach v0.3.

---

## v0.2.2

- **Fixed image pulls after the GitLab project renames**: upstream renamed `quip-protocol` to `quip-miner` and `quip-protocol-rs` to `quip-validator`. The container registry doesn't redirect renamed paths, so pulls from the old paths failed with "denied, requested access to the resource is denied" and the stack couldn't start. The compose file, the update checker's image list, and the native-binary release lookup all point at the new paths now. Existing installs pick this up on the next Start, which rewrites the staged `docker-compose.yml`.

---

## v0.2

- **Bundled local validator**: every Docker stack now runs a Substrate block-producing validator (`quip-validator`) alongside the miner, dashboard, postgres, and Caddy. The miner self-bootstraps on first start — it funds your keystore from the testnet faucet and registers it in `QuantumPow.Miners` — so no separate bootstrap container is needed. The miner talks to the validator over RPC — `ws://quip-validator:9944` in Docker mode, or `ws://127.0.0.1:9944` (published on the host loopback) when the miner runs natively on macOS. The manager waits for the validator's RPC to come up before starting a native miner.
- **Automatic v0.1 → v0.2 config migration**: on first start after upgrading, the app rewrites your old `config.toml` from the `[global]` schema to the new `[miner]` schema and migrates your `.env`, backing up the originals to `.v0.1_backup/` (and `.env.v0.1_backup`) first. Your hand-edited public host, public port, node name, log level, and log path are carried forward; obsolete v0.1 keys and the old dashboard `QUIP_NODE_URL`/`QUIP_NODE_TOKEN` env vars are dropped. Migration refuses to run (rather than clobber) if a backup already exists. _This automatic v0.1 → v0.2 migration path will be removed in v0.3 — users still on v0.1 must upgrade through a v0.2 release first._
- **Live image pull progress**: the Logs tab now shows per-image progress bars while `docker compose pull` downloads the stack, instead of streaming raw layer output into the console. Only images that are actually downloading get a bar — images already up to date are omitted — and the panel closes reliably when the pull finishes (each pull is generation-tagged so a late progress event can't reopen a panel that has already closed).
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
