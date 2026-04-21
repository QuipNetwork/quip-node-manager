# Changelog

> **Note:** Because we are so new, we do not have Microsoft and Apple developer accounts activated yet so that you can install these apps without warnings from your Operating System. We are actively going through the identification for that now, and should have this resolved in the next month.

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

## v0.1

### Fixes

- **WSL pre-flight check (Windows)**: No longer falsely reports "WSL not installed" for non-admin users with Microsoft Store WSL. Detection now probes `wsl --list --verbose`, `wsl --version`, and `wsl --status` in sequence, and decodes the UTF-16LE output `wsl.exe` emits on Windows. The check is also demoted from a blocking requirement to a warning, since Docker Desktop's own check already fails first if WSL2 is truly missing.

## v0.0.7

### Changes

- **Native mode restricted to macOS**: Run Mode toggle is now only shown on macOS. Windows and Linux default to Docker mode with no option to switch. Backend enforces this on both load and save.

## v0.0.6

### New Features

- **WSL pre-flight check (Windows)**: Docker mode now verifies WSL is installed with a distro before starting, with actionable fix instructions
- **External links open in system browser**: Links in the app now open in the default browser instead of being swallowed by the webview (via tauri-plugin-opener)

### Improvements

- **UDP+TCP firewall checks**: Firewall and port forwarding checks now verify both UDP and TCP on all platforms, reporting exactly which protocol is missing
- **CLI firewall instructions**: Added step-by-step firewall setup (ufw on Linux, New-NetFirewallRule on Windows) and router forwarding notes to CLI docs
- **Automated release notes**: CI now reads release description from CHANGELOG.md (install instructions + current version's changelog)

## v0.0.5

### Updates

- **New app icon**: Updated to quipv4 across all platforms (window, tray, macOS/Windows/Linux/iOS/Android bundles)

## v0.0.4

### New Features

- **Auto-detect public IP at node start**: When `public_host` is not explicitly configured, the app detects the external IP and writes it to `config.toml` before starting the node. This ensures peers can connect back without manual configuration. Applied to all three start paths: Docker, Native, and TUI.

### Bug Fixes

- **Fix CI release job**: The `release-cli` flag `--assets-links` (plural) was not recognized; changed to `--assets-link` (singular, repeated per asset). Removed broken `jq` fallback.
- **Fix CI bundle copy with cached artifacts**: Clean `src-tauri/target/release/bundle` before building to prevent stale artifacts from prior versions breaking the glob copy step.
- **Make CI release job idempotent**: Release creation now handles the case where a release already exists for the tag.

## v0.0.3

### Bug Fixes

- **Fix Windows firewall check**: replaced invalid `localport=` netsh filter with proper `name=all dir=in` and block-based output parsing
- **Fix Docker log streaming on Windows/Linux**: replaced `sh -c "docker logs"` (no `sh` on Windows) with direct `docker logs` call and separate stdout/stderr threads
- **Fix version display**: was stuck at v0.0.0, now reads from Cargo.toml at compile time
- **Fix app bundle name on macOS**: now builds as `Quip Node Manager.app`
- **Migrate all URLs from piqued to quip.network namespace**: fixes version checks, binary downloads, and registry lookups
- **Add timeout to binary --version check**: prevents hanging when the binary doesn't respond

### New Features

- **Node version display**: shows protocol version next to app version (e.g. `v0.0.3 (node 0.0.4)`)
- **Version check in pre-flight checklist**: warns when node binary/image is outdated with an Update button
- **Unified log streaming**: both Docker and Native modes tail `node.log` for real mining activity, with fallback to docker logs or process stdout until node.log appears

### Improvements

- **Network checks are warnings, not blockers**: public IP, hostname, port forwarding, firewall, and version checks no longer prevent starting the node
- **Instant startup**: UI renders immediately; all backend calls run in background
- **Parallel checklist**: network checks run concurrently via `tokio::join!`
- **Parallel hardware survey**: GPU, Docker, and Python detection run in parallel threads
