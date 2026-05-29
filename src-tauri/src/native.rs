// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::log_stream::LogEntry;
use crate::settings::{data_dir, RunMode};
use serde::Serialize;
use std::process::Child;
use std::sync::{Arc, Mutex};
use tauri::Emitter;

const PROTOCOL_PROJECT: &str = "quip.network%2Fquip-protocol";
const PROTOCOL_RELEASE_TAG: &str = crate::compose::COMPOSE_IMAGE_TAG;

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

fn binary_release_marker_path() -> std::path::PathBuf {
    data_dir()
        .join("bin")
        .join(format!("{}.release", binary_name()))
}

fn release_download_url() -> String {
    format!(
        "https://gitlab.com/quip.network/quip-protocol/-/releases/{}/downloads/{}",
        PROTOCOL_RELEASE_TAG,
        binary_name()
    )
}

fn installed_binary_release_tag() -> Option<String> {
    std::fs::read_to_string(binary_release_marker_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_binary_release_marker() {
    let path = binary_release_marker_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, PROTOCOL_RELEASE_TAG);
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

/// Download the configured v0.2 native miner binary from GitLab releases.
#[tauri::command]
pub async fn download_native_binary(app: tauri::AppHandle) -> Result<String, String> {
    use std::io::Write;

    let name = binary_name();
    let url = release_download_url();

    let log = |msg: String| {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": msg,
        });
        let _ = app.emit("node-log", entry);
    };

    log(format!("Downloading {}", url));

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
    if let Some(t) = total {
        log(format!("Binary size: {:.1} MB", t as f64 / 1_048_576.0));
    }

    // Stream to file
    let bin_dir = data_dir().join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("Cannot create bin dir: {}", e))?;

    let dest = binary_path();
    let tmp = dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("Cannot create file: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = 0;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;

        // Log every 10%
        if let Some(t) = total {
            let pct = (downloaded * 100) / t;
            if pct / 10 > last_pct / 10 {
                log(format!(
                    "Downloading... {:.1}/{:.1} MB ({}%)",
                    downloaded as f64 / 1_048_576.0,
                    t as f64 / 1_048_576.0,
                    pct
                ));
                last_pct = pct;
            }
        }

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
    write_binary_release_marker();

    let _ = app.emit(
        "binary-download-progress",
        BinaryDownloadProgress {
            downloaded,
            total,
            done: true,
        },
    );

    let version = installed_binary_version()
        .unwrap_or_else(|| normalize_release_version(PROTOCOL_RELEASE_TAG));
    log(format!(
        "Installed {} from {} (binary version: {})",
        name, PROTOCOL_RELEASE_TAG, version
    ));

    Ok(version)
}

/// Check if the configured v0.2 native miner release is installed.
#[tauri::command]
pub async fn check_binary_update() -> Result<Option<crate::update::UpdateInfo>, String> {
    let current = match tokio::task::spawn_blocking(installed_binary_version)
        .await
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => return Ok(None),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "https://gitlab.com/api/v4/projects/{}/releases/{}",
        PROTOCOL_PROJECT, PROTOCOL_RELEASE_TAG
    );
    let release: serde_json::Value = match client
        .get(&url)
        .header("User-Agent", "quip-node-manager")
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => return Ok(None),
    };

    let tag = release["tag_name"].as_str().unwrap_or(PROTOCOL_RELEASE_TAG);
    if tag.is_empty() || !release_has_binary_asset(&release, binary_name()) {
        return Ok(None);
    }

    let current_tag_matches = installed_binary_release_tag()
        .map(|installed| installed == tag)
        .unwrap_or(false);
    let current_version_matches =
        normalize_release_version(&current) == normalize_release_version(tag);

    if !current_tag_matches && !current_version_matches {
        Ok(Some(crate::update::UpdateInfo {
            version: normalize_release_version(tag),
            url: release_download_url(),
            notes: release["description"].as_str().unwrap_or("").to_string(),
        }))
    } else {
        Ok(None)
    }
}

fn release_has_binary_asset(release: &serde_json::Value, asset_name: &str) -> bool {
    release["assets"]["links"]
        .as_array()
        .map(|links| {
            links
                .iter()
                .any(|link| link["name"].as_str() == Some(asset_name))
        })
        .unwrap_or(false)
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

    // Auto-detect public IP when no public_host is configured
    if config.public_host.is_empty() {
        if let Ok(ip) = crate::network::detect_public_ip().await {
            config.public_host = ip;
        }
    }

    // Write config.toml for native mode
    crate::config::write_config_toml(&config, &RunMode::Native)?;

    let bin = binary_path();
    if !bin.exists() {
        return Err(format!(
            "Native miner binary not found at {}",
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

    let mut cmd = crate::cmd::new(&bin);
    cmd.args(["--config", &config_path.to_string_lossy()])
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
    let cmd_msg = format!("$ {} --config {}", bin.display(), config_path.display());
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
        crate::checklist::trigger_recheck_auto(rc_app, vec!["binary".into(), "version".into()])
            .await;
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
    fn release_download_url_uses_v02_preview_asset_path() {
        let url = release_download_url();
        assert!(url.contains("/releases/v0.2-preview/downloads/"));
        assert!(url.ends_with(binary_name()));
    }

    #[test]
    fn release_asset_detection_finds_current_platform_binary() {
        let release = serde_json::json!({
            "assets": {
                "links": [
                    { "name": binary_name() },
                    { "name": "other-file" }
                ]
            }
        });

        assert!(release_has_binary_asset(&release, binary_name()));
        assert!(!release_has_binary_asset(
            &release,
            "quip-network-node-macos-arm64"
        ));
    }

    #[test]
    fn release_version_normalization_accepts_preview_tags() {
        assert_eq!(normalize_release_version("v0.2-preview"), "0.2-preview");
        assert_eq!(normalize_release_version("0.2-preview"), "0.2-preview");
    }
}
