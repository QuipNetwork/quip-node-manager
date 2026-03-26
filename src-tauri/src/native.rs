// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::log_stream::LogEntry;
use crate::settings::{data_dir, RunMode};
use serde::Serialize;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

const PROTOCOL_PROJECT: &str = "piqued%2Fquip-protocol";

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

fn binary_name() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "quip-network-node-macos-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "quip-network-node-macos-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "quip-network-node-linux-x86_64"
    } else if cfg!(target_os = "windows") {
        "quip-network-node-windows-x86_64.exe"
    } else {
        "quip-network-node"
    }
}

fn binary_path() -> std::path::PathBuf {
    data_dir().join("bin").join(binary_name())
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
        use std::process::Command;
        Command::new("tasklist")
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
        let _ = std::process::Command::new("taskkill")
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
pub fn installed_binary_version() -> Option<String> {
    let bin = binary_path();
    if !bin.exists() {
        return None;
    }
    let output = Command::new(&bin)
        .args(["--version"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Output is typically "quip-network-node 0.0.3" or just "0.0.3"
    let version = text
        .trim()
        .rsplit(' ')
        .next()
        .unwrap_or(text.trim())
        .trim_start_matches('v')
        .to_string();
    if version.is_empty() { None } else { Some(version) }
}

/// Download the latest binary from GitLab releases.
#[tauri::command]
pub async fn download_native_binary(
    app: tauri::AppHandle,
) -> Result<String, String> {
    use std::io::Write;

    let name = binary_name();
    let url = format!(
        "https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/{}",
        name
    );

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
        log(format!(
            "Binary size: {:.1} MB",
            t as f64 / 1_048_576.0
        ));
    }

    // Stream to file
    let bin_dir = data_dir().join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("Cannot create bin dir: {}", e))?;

    let dest = binary_path();
    let tmp = dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| format!("Cannot create file: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = 0;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| format!("Download error: {}", e))?;
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
    std::fs::rename(&tmp, &dest)
        .map_err(|e| format!("Cannot install binary: {}", e))?;

    // chmod +x on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)
            .map_err(|e| e.to_string())?;
    }

    let _ = app.emit(
        "binary-download-progress",
        BinaryDownloadProgress {
            downloaded,
            total,
            done: true,
        },
    );

    let version =
        installed_binary_version().unwrap_or("unknown".into());
    log(format!("Installed {} v{}", name, version));

    Ok(version)
}

/// Check if a newer binary is available from GitLab releases.
#[tauri::command]
pub async fn check_binary_update(
) -> Result<Option<crate::update::UpdateInfo>, String> {
    let current = match installed_binary_version() {
        Some(v) => v,
        None => return Ok(None),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "https://gitlab.com/api/v4/projects/{}/releases",
        PROTOCOL_PROJECT
    );
    let releases: Vec<serde_json::Value> = match client
        .get(&url)
        .header("User-Agent", "quip-node-manager")
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => return Ok(None),
    };

    let Some(latest) = releases.first() else {
        return Ok(None);
    };

    let tag = latest["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v');
    if tag.is_empty() {
        return Ok(None);
    }

    if crate::update::parse_semver(tag)
        > crate::update::parse_semver(&current)
    {
        Ok(Some(crate::update::UpdateInfo {
            version: tag.to_string(),
            url: format!(
                "https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/{}",
                binary_name()
            ),
            notes: latest["description"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        }))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn start_native_node(
    app: tauri::AppHandle,
    state: tauri::State<'_, NativeProcessState>,
) -> Result<String, String> {
    // Check for already-running process (in-memory or orphan from PID file)
    if let Some(child) = state.child.lock().unwrap().as_ref() {
        let pid = child.id();
        return Err(format!(
            "Node already running (PID {})",
            pid
        ));
    }
    if let Some(pid) = detect_orphan_node() {
        return Err(format!(
            "Node already running from previous session (PID {}). Stop it first.",
            pid
        ));
    }

    let settings = crate::settings::load_settings();
    let config = settings.node_config;

    // Write config.toml for native mode
    crate::config::write_config_toml(&config, &RunMode::Native)?;

    let bin = binary_path();
    if !bin.exists() {
        return Err(format!(
            "Node binary not found at {}",
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

    let mut cmd = Command::new(&bin);
    cmd.args(["--config", &config_path.to_string_lossy()])
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
    let cmd_msg = format!(
        "$ {} --config {}",
        bin.display(),
        config_path.display()
    );
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
            message: format!("Native node started (PID {})", pid),
        },
    );

    // Start tailing the log file
    let stop_flag = Arc::clone(&state.stop_flag);
    *stop_flag.lock().unwrap() = false;
    start_log_tail(app.clone(), Arc::clone(&stop_flag));

    write_pid(pid);
    *state.child.lock().unwrap() = Some(child);

    Ok(format!("Native node started (PID {})", pid))
}

/// Tail the node-output.log file, emitting lines to the UI.
/// First reads existing content (last 200 lines), then follows new output.
fn start_log_tail(
    app: tauri::AppHandle,
    stop_flag: Arc<Mutex<bool>>,
) {
    let log_path = node_output_log_path();
    std::thread::spawn(move || {
        use std::io::{Read, Seek, SeekFrom};

        // Wait briefly for the file to appear
        for _ in 0..10 {
            if log_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let mut file = match std::fs::File::open(&log_path) {
            Ok(f) => f,
            Err(_) => {
                // No log file — node was started before log redirect.
                let _ = app.emit(
                    "node-log",
                    &LogEntry {
                        timestamp: String::new(),
                        level: "WARN".to_string(),
                        message: "Node is running but no log file found. Stop and restart to enable log capture.".to_string(),
                    },
                );
                return;
            }
        };

        // Read existing content (backfill)
        let mut existing = String::new();
        let _ = file.read_to_string(&mut existing);
        let lines: Vec<&str> = existing.lines().collect();
        let start = lines.len().saturating_sub(200);
        for line in &lines[start..] {
            if *stop_flag.lock().unwrap() {
                return;
            }
            let entry = crate::log_stream::parse_log_line(line);
            let _ = app.emit("node-log", &entry);
        }

        // Now tail: seek to end, poll for new data
        let _ = file.seek(SeekFrom::End(0));
        let mut buf = String::new();
        loop {
            if *stop_flag.lock().unwrap() {
                break;
            }
            buf.clear();
            match file.read_to_string(&mut buf) {
                Ok(0) => {
                    // No new data — sleep and retry
                    std::thread::sleep(
                        std::time::Duration::from_millis(250),
                    );
                }
                Ok(_) => {
                    for line in buf.lines() {
                        if *stop_flag.lock().unwrap() {
                            return;
                        }
                        let entry =
                            crate::log_stream::parse_log_line(line);
                        let _ = app.emit("node-log", &entry);
                    }
                }
                Err(_) => break,
            }
        }
    });
}

/// Start tailing native node logs (for orphan reconnect on app restart).
#[tauri::command]
pub async fn start_native_log_tail(
    app: tauri::AppHandle,
    state: tauri::State<'_, NativeProcessState>,
) -> Result<(), String> {
    let stop_flag = Arc::clone(&state.stop_flag);
    *stop_flag.lock().unwrap() = false;
    start_log_tail(app, stop_flag);
    Ok(())
}

#[tauri::command]
pub async fn stop_native_node(
    state: tauri::State<'_, NativeProcessState>,
) -> Result<(), String> {
    *state.stop_flag.lock().unwrap() = true;

    let mut guard = state.child.lock().unwrap();
    if let Some(ref mut child) = *guard {
        // Kill the entire process group
        kill_pid(child.id());
        let _ = child.wait();
    }
    *guard = None;
    drop(guard);

    // Also kill any orphan process group from a previous session
    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            kill_pid(pid);
        }
    }
    remove_pid();
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
