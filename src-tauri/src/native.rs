// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::log_stream::LogEntry;
use crate::settings::{data_dir, NodeConfig, RunMode};
use serde::Serialize;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};
use tauri::Emitter;

const PROTOCOL_PROJECT: &str = "quip.network%2Fquip-protocol";
const NATIVE_MINER_VALIDATOR_RPC_READY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(90);

#[derive(Serialize, Clone, Debug)]
pub struct NativeNodeStatus {
    pub running: bool,
    pub pid: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct BinaryDownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
}

pub struct NativeProcessState {
    child: Arc<Mutex<Option<Child>>>,
    stop_flag: Arc<Mutex<bool>>,
}

impl NativeProcessState {
    pub fn new() -> Self {
        NativeProcessState {
            child: Arc::new(Mutex::new(None)),
            stop_flag: Arc::new(Mutex::new(false)),
        }
    }
}

impl Default for NativeProcessState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn binary_name() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "quip-miner-macos-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "quip-miner-macos-x86_64"
    } else {
        "quip-miner"
    }
}

fn binary_path() -> std::path::PathBuf {
    data_dir().join("bin").join(binary_name())
}

/// Remove pre-v0.2 native binaries (named `quip-network-node-*`) and their
/// `.release` markers from `bin_dir`. The v0.2 manager downloads and runs
/// `quip-miner-*`, so the old files are dead weight (~60 MB). Returns the
/// names of files removed. Best-effort: unreadable dirs yield an empty list.
fn cleanup_legacy_native_binaries(bin_dir: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(bin_dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("quip-network-node") && std::fs::remove_file(entry.path()).is_ok() {
            removed.push(name);
        }
    }
    removed
}

/// Best-effort cleanup of legacy native binaries in the live `bin` dir.
pub fn cleanup_legacy_binaries() -> Vec<String> {
    cleanup_legacy_native_binaries(&data_dir().join("bin"))
}

fn binary_release_marker_path() -> std::path::PathBuf {
    data_dir()
        .join("bin")
        .join(format!("{}.release", binary_name()))
}

/// Fetch the quip-protocol releases list (GitLab returns them newest-first).
/// Returns the URL it hit and the specific cause on failure so callers can show
/// the user what was tried (transport error vs. HTTP status vs. bad JSON) rather
/// than a generic "couldn't list releases".
async fn fetch_protocol_releases(
    client: &reqwest::Client,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "https://gitlab.com/api/v4/projects/{}/releases?per_page=20",
        PROTOCOL_PROJECT
    );
    let resp = client
        .get(&url)
        .header("User-Agent", "quip-node-manager")
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("GET {url} returned HTTP {status}"));
    }
    resp.json()
        .await
        .map_err(|e| format!("GET {url} returned unparseable JSON: {e}"))
}

/// (tag, asset_url, description) for every release that ships `asset_name`,
/// newest-first (GitLab returns releases newest-first). This tracks whatever
/// the latest releases are tagged (`v0.2`, `v0.2.0rc1`, …) and skips older
/// releases that only carry the legacy asset name.
fn binary_release_candidates(
    releases: &serde_json::Value,
    asset_name: &str,
) -> Vec<(String, String, String)> {
    releases
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rel| {
            let tag = rel["tag_name"].as_str()?.to_string();
            let url = release_asset_url(rel, asset_name)?;
            let desc = rel["description"].as_str().unwrap_or("").to_string();
            Some((tag, url, desc))
        })
        .collect()
}

/// A release asset link is registered in the API as soon as the release is
/// tagged, but the artifact behind it 404s until its build job finishes. A
/// HEAD is unreliable (GitLab answers 200 at the redirect layer), so probe
/// with a 1-byte ranged GET — a 404 means "not built yet".
async fn asset_is_downloadable(client: &reqwest::Client, url: &str) -> bool {
    match client
        .get(url)
        .header("User-Agent", "quip-node-manager")
        .header("Range", "bytes=0-0")
        .send()
        .await
    {
        Ok(resp) => resp.status() != reqwest::StatusCode::NOT_FOUND,
        Err(_) => false,
    }
}

/// Newest release whose `asset_name` artifact is actually downloadable. Skips
/// just-tagged releases whose build hasn't produced the artifact yet.
async fn resolve_latest_downloadable_release(
    client: &reqwest::Client,
    releases: &serde_json::Value,
    asset_name: &str,
) -> Option<(String, String, String)> {
    for candidate in binary_release_candidates(releases, asset_name) {
        if asset_is_downloadable(client, &candidate.1).await {
            return Some(candidate);
        }
    }
    None
}

/// Direct download URL for `asset_name` within `release`. Prefers the stable
/// `direct_asset_url` permalink, falling back to the job-artifact `url`.
fn release_asset_url(release: &serde_json::Value, asset_name: &str) -> Option<String> {
    release["assets"]["links"]
        .as_array()?
        .iter()
        .find(|link| link["name"].as_str() == Some(asset_name))
        .and_then(|link| {
            link["direct_asset_url"]
                .as_str()
                .or_else(|| link["url"].as_str())
                .map(str::to_string)
        })
}

fn installed_binary_release_tag() -> Option<String> {
    std::fs::read_to_string(binary_release_marker_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Record the release tag the on-disk binary came from, so update checks can
/// compare against the latest release even when the binary's own `--version`
/// string differs from the tag.
fn write_binary_release_marker(tag: &str) -> Result<(), String> {
    let path = binary_release_marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, tag).map_err(|e| format!("write {}: {e}", path.display()))
}

fn normalize_release_version(value: &str) -> String {
    value.trim().trim_start_matches('v').to_ascii_lowercase()
}

pub fn is_binary_available() -> bool {
    let path = binary_path();
    path.exists() && path.is_file()
}

fn pid_file_path() -> std::path::PathBuf {
    data_dir().join("node.pid")
}

fn node_output_log_path() -> std::path::PathBuf {
    data_dir().join("node-output.log")
}

pub(crate) fn native_miner_validator_url(config: &NodeConfig) -> String {
    crate::config::native_validator_rpc_url(config)
}

fn validator_rpc_http_probe_url(validator_url: &str) -> String {
    validator_url
        .strip_prefix("ws://")
        .map(|rest| format!("http://{rest}"))
        .or_else(|| {
            validator_url
                .strip_prefix("wss://")
                .map(|rest| format!("https://{rest}"))
        })
        .unwrap_or_else(|| validator_url.to_string())
}

// The miner picks its own backends (cpu/gpu/qpu) from the config.toml
// sections — the manager passes no subcommand and no per-key CLI overrides
// (signer_key lives in [miner]; the faucet default is built into the miner).
pub(crate) fn native_miner_args(config_path: &Path) -> Vec<String> {
    vec![
        "--config".to_string(),
        config_path.to_string_lossy().to_string(),
    ]
}

pub(crate) fn native_signer_key_path() -> std::path::PathBuf {
    data_dir().join("keystore.json")
}

pub(crate) fn native_keygen_args(signer_key: &Path) -> Vec<String> {
    vec![
        "keygen".to_string(),
        "--out".to_string(),
        signer_key.to_string_lossy().to_string(),
    ]
}

pub(crate) fn ensure_native_signer_key(bin: &Path) -> Result<bool, String> {
    let signer_key = native_signer_key_path();
    if signer_key.exists() {
        return Ok(false);
    }

    let work_dir = data_dir();
    std::fs::create_dir_all(&work_dir).map_err(|e| format!("Cannot create data dir: {}", e))?;

    let output = crate::cmd::new(bin)
        .args(native_keygen_args(&signer_key))
        .current_dir(&work_dir)
        .output()
        .map_err(|e| format!("Failed to generate native miner keystore: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            return Err(format!("Native miner keygen failed: {}", output.status));
        }
        return Err(format!(
            "Native miner keygen failed: {}: {}",
            output.status, detail
        ));
    }

    if !signer_key.exists() {
        return Err(format!(
            "Native miner keygen finished but did not create {}",
            signer_key.display()
        ));
    }

    Ok(true)
}

async fn wait_for_native_miner_validator_rpc(
    app: &tauri::AppHandle,
    validator_url: &str,
) -> Result<(), String> {
    // TODO: Revisit this readiness gate; it likely needs another pass to make
    // the flow and diagnostics more eloquent.
    let probe_url = validator_rpc_http_probe_url(validator_url);
    let _ = app.emit(
        "node-log",
        &LogEntry {
            timestamp: String::new(),
            level: "INFO".to_string(),
            message: format!("Waiting for validator RPC at {validator_url}"),
        },
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "system_health",
        "params": [],
    });
    let start = std::time::Instant::now();
    let mut last_error = String::from("not checked yet");

    while start.elapsed() < NATIVE_MINER_VALIDATOR_RPC_READY_TIMEOUT {
        match client.post(&probe_url).json(&body).send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(text) if status.is_success() => {
                        if serde_json::from_str::<serde_json::Value>(&text)
                            .map(|value| {
                                value.get("result").is_some() || value.get("error").is_some()
                            })
                            .unwrap_or(false)
                        {
                            let _ = app.emit(
                                "node-log",
                                &LogEntry {
                                    timestamp: String::new(),
                                    level: "INFO".to_string(),
                                    message: format!("Validator RPC is ready at {validator_url}"),
                                },
                            );
                            return Ok(());
                        }
                        last_error = format!("unexpected RPC response: {text}");
                    }
                    Ok(text) => {
                        last_error = format!("HTTP {status}: {text}");
                    }
                    Err(e) => {
                        last_error = format!("HTTP {status}, read error: {e}");
                    }
                }
            }
            Err(e) => {
                last_error = e.to_string();
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Err(format!(
        "Validator RPC not ready at {validator_url} after {}s: {last_error}",
        NATIVE_MINER_VALIDATOR_RPC_READY_TIMEOUT.as_secs()
    ))
}

fn write_pid(pid: u32) {
    let _ = std::fs::write(pid_file_path(), pid.to_string());
}

fn remove_pid() {
    let _ = std::fs::remove_file(pid_file_path());
}

fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_file_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Check if a process with the given PID is still alive.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill -0 checks existence without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        crate::cmd::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let text = String::from_utf8_lossy(&o.stdout);
                text.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
}

/// Kill a process group by PID (kills all children too).
/// On Unix, we negate the PID to target the entire process group.
fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        // SIGTERM the entire process group
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        // SIGKILL anything still alive
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // /T kills the process tree (all children)
        let _ = crate::cmd::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
}

/// Check if a node is already running from a previous session.
/// Returns the PID if alive.
pub fn detect_orphan_node() -> Option<u32> {
    let pid = read_pid()?;
    if is_process_alive(pid) {
        Some(pid)
    } else {
        remove_pid();
        None
    }
}

/// Get the installed binary version by running `--version`.
/// Uses a 60-second timeout since some binary versions are slow to respond.
pub fn installed_binary_version() -> Option<String> {
    let bin = binary_path();
    if !bin.exists() {
        return None;
    }
    let mut child = crate::cmd::new(&bin)
        .args(["--version"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    let timeout = std::time::Duration::from_secs(60);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }

    let mut text = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut text);
    }
    let version = text
        .trim()
        .rsplit(' ')
        .next()
        .unwrap_or(text.trim())
        .trim_start_matches('v')
        .to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Serializes `download_native_binary` so concurrent callers coalesce instead
/// of racing: the loser waits on the lock, then finds the binary already
/// current and skips its own download. Belt-and-suspenders against duplicate
/// fetches from any pair of callers (checklist Retry, Start provisioning, the
/// update-restart `DownloadBinary` step) writing the same fixed temp path.
fn download_guard() -> &'static tokio::sync::Mutex<()> {
    static GUARD: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    GUARD.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Download the latest native miner binary from GitLab releases. Tracks the
/// most recent release that ships this platform's asset, so release-candidate
/// tags (e.g. `v0.2.0rc1`) are picked up automatically.
#[tauri::command]
pub async fn download_native_binary(app: tauri::AppHandle) -> Result<String, String> {
    use std::io::Write;

    let name = binary_name();

    let log = |msg: String| {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": msg,
        });
        let _ = app.emit("node-log", entry);
    };

    // Serialize downloads so concurrent callers coalesce instead of racing the
    // shared temp path (see `download_guard`). Held across the whole download.
    let _guard = download_guard().lock().await;

    // Resolve the latest release that actually ships this binary (rc-aware).
    let meta_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| e.to_string())?;
    let releases = fetch_protocol_releases(&meta_client).await?;
    let (tag, url, _desc) = resolve_latest_downloadable_release(&meta_client, &releases, name)
        .await
        .ok_or_else(|| {
            format!(
                "The latest quip-protocol release ships {name}, but its artifact \
                 isn't available yet — the build may still be running. Try again shortly."
            )
        })?;

    // If a concurrent caller installed this version while we waited on the
    // lock, skip the redundant re-download.
    let latest_version = normalize_release_version(&tag);
    if installed_binary_version().as_deref() == Some(latest_version.as_str()) {
        log(format!(
            "{name} already up to date (v{latest_version}); skipping download"
        ));
        return Ok(latest_version);
    }

    log(format!("Downloading {name} ({tag})"));
    log(format!("From {url}"));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(&url)
        .header("User-Agent", "quip-node-manager")
        .send()
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Download failed: HTTP {}. No release found — \
             a tagged release of quip-protocol is required.",
            resp.status()
        ));
    }

    let total = resp.content_length();

    // Stream to file
    let bin_dir = data_dir().join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("Cannot create bin dir: {}", e))?;

    let dest = binary_path();
    let tmp = dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("Cannot create file: {}", e))?;

    // Progress is surfaced as a bar in the UI via binary-download-progress;
    // no per-percent log lines.
    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;

        let _ = app.emit(
            "binary-download-progress",
            BinaryDownloadProgress {
                downloaded,
                total,
                done: false,
            },
        );
    }
    drop(file);

    // Move tmp → final
    std::fs::rename(&tmp, &dest).map_err(|e| format!("Cannot install binary: {}", e))?;

    // chmod +x on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).map_err(|e| e.to_string())?;
    }
    // Best-effort: the binary is already installed. A failed marker only
    // affects update detection, so warn instead of failing the download — but
    // don't swallow it silently, or stale-marker bugs become invisible.
    if let Err(e) = write_binary_release_marker(&tag) {
        log(format!(
            "Warning: could not record release marker ({e}); \
             update checks may keep re-offering this version"
        ));
    }

    let _ = app.emit(
        "binary-download-progress",
        BinaryDownloadProgress {
            downloaded,
            total,
            done: true,
        },
    );

    let version = installed_binary_version().unwrap_or_else(|| normalize_release_version(&tag));
    log(format!(
        "Installed {} from {} (binary version: {})",
        name, tag, version
    ));

    Ok(version)
}

/// Installed vs. latest-downloadable native miner versions, and whether an
/// update is warranted. `latest` is `None` when the release feed couldn't be
/// reached (so callers can distinguish "confirmed current" from "couldn't
/// check"). `update` is `Some` only when `latest` is strictly newer.
#[derive(Debug, Clone)]
pub struct BinaryVersions {
    pub installed: Option<String>,
    pub latest: Option<String>,
    pub update: Option<crate::update::UpdateInfo>,
}

/// Resolve installed and latest-online versions in one pass. Surfaces both
/// numbers so the UI can show what it discovered, not just a yes/no verdict.
pub async fn resolve_binary_versions() -> Result<BinaryVersions, String> {
    let installed = tokio::task::spawn_blocking(installed_binary_version)
        .await
        .ok()
        .flatten();
    // No installed binary → nothing to compare against; skip the network call.
    let Some(current) = installed.clone() else {
        return Ok(BinaryVersions {
            installed: None,
            latest: None,
            update: None,
        });
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    // Update check is best-effort: an unreachable feed leaves the installed
    // binary running, so a fetch failure is non-fatal here (unlike the download
    // path, which surfaces the cause to the user). Leave `latest` as None so the
    // caller can say "couldn't check" rather than over-claiming "up to date".
    let latest_release = match fetch_protocol_releases(&client).await {
        Ok(releases) => {
            // Only offer an update we can actually install — a just-tagged
            // release whose artifact build hasn't finished is skipped until
            // it's downloadable.
            resolve_latest_downloadable_release(&client, &releases, binary_name())
                .await
                .filter(|(tag, _, _)| !tag.is_empty())
        }
        Err(_) => None,
    };
    let Some((tag, url, description)) = latest_release else {
        return Ok(BinaryVersions {
            installed: Some(normalize_release_version(&current)),
            latest: None,
            update: None,
        });
    };

    // Only offer a strictly newer release — never a downgrade. A bare
    // inequality check walks an installed `-rcN` back to the lower final
    // release (e.g. local 0.2.1-rc2 → latest-downloadable 0.2.0) whenever
    // the higher version's artifact is missing or sorts older by date.
    // Compare semver against the higher of the binary's own `--version` and
    // the recorded install tag, since either may lag the other.
    let mut installed_sv = crate::update::parse_semver(current.trim_start_matches('v'));
    if let Some(marker) = installed_binary_release_tag() {
        installed_sv =
            installed_sv.max(crate::update::parse_semver(marker.trim_start_matches('v')));
    }
    let latest_sv = crate::update::parse_semver(tag.trim_start_matches('v'));
    let latest_version = normalize_release_version(&tag);

    let update = (latest_sv > installed_sv).then(|| crate::update::UpdateInfo {
        version: latest_version.clone(),
        url,
        notes: description,
    });

    Ok(BinaryVersions {
        installed: Some(normalize_release_version(&current)),
        latest: Some(latest_version),
        update,
    })
}

/// Check if a newer v0.2 native miner release is available. Thin wrapper over
/// [`resolve_binary_versions`] preserving the `Option<UpdateInfo>` contract the
/// frontend update banner consumes.
#[tauri::command]
pub async fn check_binary_update() -> Result<Option<crate::update::UpdateInfo>, String> {
    Ok(resolve_binary_versions().await?.update)
}

#[tauri::command]
pub async fn start_native_node(
    app: tauri::AppHandle,
    state: tauri::State<'_, NativeProcessState>,
) -> Result<String, String> {
    // Check for already-running process (in-memory or orphan from PID file)
    if let Some(child) = state.child.lock().unwrap().as_ref() {
        let pid = child.id();
        return Err(format!("Node already running (PID {})", pid));
    }
    if let Some(pid) = detect_orphan_node() {
        return Err(format!(
            "Node already running from previous session (PID {}). Stop it first.",
            pid
        ));
    }

    let settings = crate::settings::load_settings();
    let mut config = settings.node_config;

    let migration = crate::migration_v2::migrate_for_run_mode(&RunMode::Native)?;
    migration.promoted.apply_to_node_config(&mut config);
    crate::migration_v2::persist_promoted_settings(&migration.promoted)?;
    crate::migration_v2::emit_report(&app, &migration);

    // Auto-detect public IP when no public_host is configured. A detection
    // failure must not be silent: without a public_host the validator
    // advertises no public address and peers can't dial in, so surface a
    // warning the user can act on.
    if config.public_host.is_empty() {
        match crate::network::detect_public_ip().await {
            Ok(ip) => config.public_host = ip,
            Err(e) => {
                let _ = app.emit(
                    "node-log",
                    &LogEntry {
                        timestamp: String::new(),
                        level: "WARN".to_string(),
                        message: format!(
                            "Could not auto-detect public IP ({e}); the node will not \
                             advertise a public address. Set a public host in Settings."
                        ),
                    },
                );
            }
        }
    }

    // Write config.toml for native mode. The renderer forces the native
    // miner's REST host to loopback (it's reached via host.docker.internal),
    // so no rest_host override is needed here.
    crate::config::write_config_toml(&config, &RunMode::Native)?;

    // Auto-provision the miner binary when it's missing — mirrors Docker
    // mode pulling images on start, so a fresh or relocated data dir doesn't
    // dead-end here.
    let bin = binary_path();
    if !bin.exists() {
        let _ = app.emit(
            "node-log",
            &LogEntry {
                timestamp: String::new(),
                level: "INFO".to_string(),
                message: format!(
                    "Native miner binary not found at {} — downloading…",
                    bin.display()
                ),
            },
        );
        download_native_binary(app.clone()).await?;
    }
    if !bin.exists() {
        return Err(format!(
            "Native miner binary still missing at {} after download",
            bin.display()
        ));
    }

    let config_path = data_dir().join("config.toml");

    // Redirect stdout+stderr to a log file so we can reconnect
    // after app restarts (orphan adoption).
    let log_file_path = node_output_log_path();
    let log_file = std::fs::File::create(&log_file_path)
        .map_err(|e| format!("Cannot create log file: {}", e))?;
    let log_file_err = log_file
        .try_clone()
        .map_err(|e| format!("Cannot clone log file: {}", e))?;

    let work_dir = data_dir();
    std::fs::create_dir_all(&work_dir).map_err(|e| format!("Cannot create data dir: {}", e))?;

    if ensure_native_signer_key(&bin)? {
        let _ = app.emit(
            "node-log",
            &LogEntry {
                timestamp: String::new(),
                level: "INFO".to_string(),
                message: format!(
                    "Generated native miner keystore at {}",
                    native_signer_key_path().display()
                ),
            },
        );
    }

    let validator_url = native_miner_validator_url(&config);
    wait_for_native_miner_validator_rpc(&app, &validator_url).await?;

    let miner_args = native_miner_args(&config_path);
    let mut cmd = crate::cmd::new(&bin);
    cmd.args(&miner_args)
        .current_dir(&work_dir)
        .stdout(log_file)
        .stderr(log_file_err);

    // Put the child in its own process group so we can kill the
    // entire tree (miner workers, QUIC handlers, etc.) at once.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start node: {}", e))?;

    let pid = child.id();

    // Log the command
    let cmd_msg = format!("$ {} {}", bin.display(), miner_args.join(" "));
    let _ = app.emit(
        "node-log",
        &LogEntry {
            timestamp: String::new(),
            level: "INFO".to_string(),
            message: cmd_msg,
        },
    );
    let _ = app.emit(
        "node-log",
        &LogEntry {
            timestamp: String::new(),
            level: "INFO".to_string(),
            message: format!("Native miner started (PID {})", pid),
        },
    );

    // Start tailing node.log (the protocol's own log, not stdout)
    let stop_flag = Arc::clone(&state.stop_flag);
    *stop_flag.lock().unwrap() = false;
    start_log_tail(app.clone(), Arc::clone(&stop_flag));

    write_pid(pid);
    *state.child.lock().unwrap() = Some(child);

    Ok(format!("Native miner started (PID {})", pid))
}

/// Tail node logs: starts with node-output.log (process stdout),
/// then switches to node.log once the node creates it.
fn start_log_tail(app: tauri::AppHandle, stop_flag: Arc<Mutex<bool>>) {
    use crate::log_stream::{start_log_stream_for_app, FallbackSource};
    let path = node_output_log_path();
    let _ = app.emit(
        "node-log",
        serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": format!("[log-stream] tailing {}", path.display()),
        }),
    );
    // Native path has no `docker logs` child, so the PID slot goes unused.
    let child_pid = Arc::new(Mutex::new(None));
    let fallback = FallbackSource::File(path);
    start_log_stream_for_app(app, stop_flag, child_pid, fallback);
}

/// Start tailing native node logs (for orphan reconnect on app restart).
#[tauri::command]
pub async fn start_native_log_tail(
    app: tauri::AppHandle,
    state: tauri::State<'_, NativeProcessState>,
) -> Result<(), String> {
    let _ = app.emit(
        "node-log",
        serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": "[log-stream] starting native tail (node-output.log \u{2192} node.log)",
        }),
    );
    let stop_flag = Arc::clone(&state.stop_flag);
    *stop_flag.lock().unwrap() = false;
    start_log_tail(app, stop_flag);
    Ok(())
}

/// Outer deadline for the native stop path. `kill_pid` itself takes ~2s
/// (SIGTERM → sleep → SIGKILL on Unix) so 5s gives real escalation room
/// without letting a stuck process block the UI forever.
const NATIVE_STOP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Stop the native node process with verify + auto-recheck.
///
/// Mirrors the Docker stop path: emits stop-started/complete events,
/// verifies the process is actually gone via `kill(pid, 0)`, enforces an
/// outer deadline so a stuck `child.wait()` can't hang the UI, and fires
/// a follow-on recheck of binary+version on success.
#[tauri::command]
pub async fn stop_native_node(
    app: tauri::AppHandle,
    state: tauri::State<'_, NativeProcessState>,
) -> Result<(), String> {
    let _ = app.emit("stop-started", serde_json::json!({}));
    *state.stop_flag.lock().unwrap() = true;

    // Snapshot PIDs we need to kill. Drops the guards before awaiting.
    let (child_pid, child_opt) = {
        let mut guard = state.child.lock().unwrap();
        let pid = guard.as_ref().map(|c| c.id());
        (pid, guard.take())
    };
    let orphan_pid = read_pid()
        .filter(|pid| child_pid.map(|cp| cp != *pid).unwrap_or(true) && is_process_alive(*pid));

    // Do the blocking kill work in a bounded thread so the async runtime
    // stays responsive and we can time out cleanly.
    let kill_result = tokio::time::timeout(
        NATIVE_STOP_DEADLINE,
        tokio::task::spawn_blocking(move || {
            if let Some(pid) = child_pid {
                kill_pid(pid);
            }
            if let Some(mut child) = child_opt {
                let _ = child.wait();
            }
            if let Some(pid) = orphan_pid {
                if is_process_alive(pid) {
                    kill_pid(pid);
                }
            }
        }),
    )
    .await;

    remove_pid();

    let timed_out = kill_result.is_err();
    let still_alive = child_pid.map(is_process_alive).unwrap_or(false)
        || orphan_pid.map(is_process_alive).unwrap_or(false);

    if timed_out || still_alive {
        let msg = if timed_out {
            "native stop exceeded deadline — process may still be running"
        } else {
            "native process still alive after SIGKILL — manual kill required"
        };
        let _ = app.emit(
            "stop-complete",
            serde_json::json!({ "success": false, "error": msg }),
        );
        return Err(msg.to_string());
    }

    let _ = app.emit("stop-complete", serde_json::json!({ "success": true }));

    let rc_app = app.clone();
    tokio::spawn(async move {
        // The binary check now covers both presence and freshness.
        crate::checklist::trigger_recheck_auto(rc_app, vec!["binary".into()]).await;
    });

    Ok(())
}

#[tauri::command]
pub async fn get_native_node_status(
    state: tauri::State<'_, NativeProcessState>,
) -> Result<NativeNodeStatus, String> {
    // Check in-memory child first
    let mut guard = state.child.lock().unwrap();
    if let Some(ref mut child) = *guard {
        match child.try_wait() {
            Ok(None) => {
                return Ok(NativeNodeStatus {
                    running: true,
                    pid: Some(child.id()),
                });
            }
            _ => {
                // Process exited
                *guard = None;
                remove_pid();
            }
        }
    }
    drop(guard);

    // Fall back to PID file (orphan from previous session)
    if let Some(pid) = detect_orphan_node() {
        return Ok(NativeNodeStatus {
            running: true,
            pid: Some(pid),
        });
    }

    Ok(NativeNodeStatus {
        running: false,
        pid: None,
    })
}

#[tauri::command]
pub async fn check_native_binary() -> Result<bool, String> {
    Ok(is_binary_available())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_binary_name_uses_v02_miner_asset() {
        assert!(binary_name().starts_with("quip-miner"));
        assert!(!binary_name().starts_with("quip-network-node"));
    }

    #[test]
    fn cleanup_removes_legacy_node_binaries_keeps_current() {
        let dir = std::env::temp_dir().join(format!("quip-bin-cleanup-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("quip-network-node-macos-arm64");
        let legacy_marker = dir.join("quip-network-node-macos-arm64.release");
        let current = dir.join(binary_name());
        std::fs::write(&legacy, b"old").unwrap();
        std::fs::write(&legacy_marker, b"v0.1").unwrap();
        std::fs::write(&current, b"new").unwrap();

        let removed = cleanup_legacy_native_binaries(&dir);

        assert!(!legacy.exists(), "legacy binary should be removed");
        assert!(!legacy_marker.exists(), "legacy marker should be removed");
        assert!(current.exists(), "current binary must be kept");
        assert!(removed.iter().any(|n| n == "quip-network-node-macos-arm64"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn candidates_are_newest_first_and_skip_legacy_asset_releases() {
        // GitLab returns releases newest-first. The v0.2 rcs ship the
        // quip-miner-* asset; the older v0.1.x release only has the legacy
        // quip-network-node-* asset, so it must be excluded.
        let releases = serde_json::json!([
            {
                "tag_name": "v0.2.0rc2",
                "description": "rc2 notes",
                "assets": { "links": [
                    { "name": binary_name(),
                      "direct_asset_url": "https://example/releases/v0.2.0rc2/downloads/bin" }
                ]}
            },
            {
                "tag_name": "v0.2.0rc1",
                "assets": { "links": [
                    { "name": binary_name(),
                      "direct_asset_url": "https://example/releases/v0.2.0rc1/downloads/bin" }
                ]}
            },
            {
                "tag_name": "v0.1.20",
                "assets": { "links": [ { "name": "quip-network-node-macos-arm64" } ] }
            }
        ]);

        let candidates = binary_release_candidates(&releases, binary_name());
        let tags: Vec<&str> = candidates.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(tags, vec!["v0.2.0rc2", "v0.2.0rc1"]);
        assert_eq!(
            candidates[0].1,
            "https://example/releases/v0.2.0rc2/downloads/bin"
        );
        assert_eq!(candidates[0].2, "rc2 notes");
    }

    #[test]
    fn no_release_shipping_the_binary_yields_no_candidates() {
        let releases = serde_json::json!([
            { "tag_name": "v0.1.20",
              "assets": { "links": [ { "name": "quip-network-node-macos-arm64" } ] } }
        ]);
        assert!(binary_release_candidates(&releases, binary_name()).is_empty());
    }

    #[test]
    fn asset_url_falls_back_to_url_when_no_direct_asset_url() {
        let rel = serde_json::json!({
            "assets": { "links": [ { "name": binary_name(), "url": "https://example/u" } ] }
        });
        assert_eq!(
            release_asset_url(&rel, binary_name()).as_deref(),
            Some("https://example/u")
        );
    }

    #[test]
    fn release_version_normalization_accepts_preview_tags() {
        assert_eq!(normalize_release_version("v0.2-preview"), "0.2-preview");
        assert_eq!(normalize_release_version("0.2-preview"), "0.2-preview");
        // Release-candidate tags normalize too, so equality checks match.
        assert_eq!(normalize_release_version("v0.2.0rc1"), "0.2.0rc1");
    }

    #[test]
    fn native_miner_args_pass_only_the_config_path() {
        // No backend subcommand and no per-key CLI overrides: the miner
        // decides cpu/gpu/qpu from the config.toml sections alone.
        let args = native_miner_args(Path::new("/tmp/config.toml"));

        assert_eq!(args, vec!["--config".to_string(), "/tmp/config.toml".to_string()]);
    }

    #[test]
    fn native_keygen_args_write_to_configured_signer_path() {
        let args = native_keygen_args(Path::new("/tmp/quip-data/keystore.json"));

        assert_eq!(
            args,
            vec!["keygen", "--out", "/tmp/quip-data/keystore.json"]
        );
    }

    #[test]
    fn native_miner_validator_url_uses_configured_raw_rpc_port() {
        // Native miner connects to the validator's raw RPC on the configured
        // host port (default 9944), not Caddy's /rpc route.
        let cfg = NodeConfig::default();
        assert_eq!(native_miner_validator_url(&cfg), "ws://127.0.0.1:9944");

        let custom = NodeConfig {
            validator_rpc_port: 9955,
            ..NodeConfig::default()
        };
        assert_eq!(native_miner_validator_url(&custom), "ws://127.0.0.1:9955");
    }

    #[test]
    fn validator_rpc_http_probe_url_converts_websocket_urls() {
        assert_eq!(
            validator_rpc_http_probe_url("ws://127.0.0.1:20049/rpc"),
            "http://127.0.0.1:20049/rpc"
        );
        assert_eq!(
            validator_rpc_http_probe_url("wss://node.example.com/rpc"),
            "https://node.example.com/rpc"
        );
        assert_eq!(
            validator_rpc_http_probe_url("http://127.0.0.1:20049/rpc"),
            "http://127.0.0.1:20049/rpc"
        );
    }
}
