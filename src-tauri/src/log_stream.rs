// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::SyncSender;
use tauri::Emitter;

#[derive(Serialize, Clone, Debug)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

/// Shared state for the Docker logs streamer.
///
/// `child_pid` holds the PID of the in-flight `docker logs -f` process
/// whenever one is running. Killing this child at stop time unblocks
/// `BufReader::lines()` immediately instead of waiting for the next log
/// line — critical because Docker stop isn't visible to the streamer
/// until the daemon closes the pipe.
pub struct LogStreamState {
    pub handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    pub stop_flag: Arc<Mutex<bool>>,
    pub child_pid: Arc<Mutex<Option<u32>>>,
}

impl Default for LogStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl LogStreamState {
    pub fn new() -> Self {
        LogStreamState {
            handle: Arc::new(Mutex::new(None)),
            stop_flag: Arc::new(Mutex::new(false)),
            child_pid: Arc::new(Mutex::new(None)),
        }
    }

    /// Kill the in-flight `docker logs` child (if any) and clear the PID.
    /// Safe to call when no child is running.
    pub fn kill_child(&self) {
        if let Some(pid) = self.child_pid.lock().unwrap().take() {
            kill_log_child(pid);
        }
    }
}

/// Kill a single child process by PID (not its process group).
/// The docker logs CLI has no workers we need to clean up, so a
/// simple single-process kill is sufficient.
fn kill_log_child(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = crate::cmd::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
}

pub fn parse_log_line(line: &str) -> LogEntry {
    // Format: [file.py:123][node] 2026-01-01T12:00:00+00:00 LEVEL - message
    // Or Python: LEVEL:module:message
    // Otherwise: pass through verbatim.

    // Try structured quip-protocol format
    if line.starts_with('[') {
        if let Some(after_brackets) = line.find("] ").map(|i| {
            let rest = &line[i + 2..];
            if rest.starts_with('[') {
                rest.find("] ").map(|j| &rest[j + 2..])
            } else {
                Some(rest)
            }
        }).flatten() {
            let parts: Vec<&str> =
                after_brackets.splitn(3, ' ').collect();
            if parts.len() >= 2 {
                let level = match parts[1].to_uppercase().as_str() {
                    "ERROR" | "ERROR:" => "ERROR",
                    "WARNING" | "WARNING:" | "WARN" => "WARN",
                    "DEBUG" | "DEBUG:" => "DEBUG",
                    _ => "INFO",
                };
                return LogEntry {
                    timestamp: parts[0].to_string(),
                    level: level.to_string(),
                    message: parts
                        .get(2)
                        .map(|s| s.trim_start_matches("- "))
                        .unwrap_or("")
                        .to_string(),
                };
            }
        }
    }

    // Try Python logging: "LEVEL:module:message"
    if let Some(colon) = line.find(':') {
        let prefix = &line[..colon];
        let level = match prefix {
            "ERROR" => Some("ERROR"),
            "WARNING" => Some("WARN"),
            "INFO" => Some("INFO"),
            "DEBUG" => Some("DEBUG"),
            _ => None,
        };
        if let Some(lvl) = level {
            return LogEntry {
                timestamp: String::new(),
                level: lvl.to_string(),
                message: line[colon + 1..].to_string(),
            };
        }
    }

    // Plain text — pass through verbatim
    LogEntry {
        timestamp: String::new(),
        level: "INFO".to_string(),
        message: line.to_string(),
    }
}

fn node_log_path() -> PathBuf {
    crate::settings::data_dir().join("node.log")
}

// ─── File tailing ────────────────────────────────────────────────────────────

/// Tail a log file: backfill last 200 lines, then follow new output.
/// Handles rotation/truncation by reopening when the file shrinks.
fn tail_file<F>(
    path: &std::path::Path,
    stop: &Mutex<bool>,
    emit: &F,
) where
    F: Fn(LogEntry) -> bool,
{
    let open = || std::fs::File::open(path);
    let mut file = match open() {
        Ok(f) => f,
        Err(_) => return,
    };

    let mut existing = String::new();
    let _ = file.read_to_string(&mut existing);
    let lines: Vec<&str> = existing.lines().collect();
    let start = lines.len().saturating_sub(200);
    for line in &lines[start..] {
        if *stop.lock().unwrap() { return; }
        if !emit(parse_log_line(line)) { return; }
    }

    let mut pos = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let mut buf = String::new();
    loop {
        if *stop.lock().unwrap() { break; }

        let reopened = match std::fs::metadata(path) {
            Ok(meta) if meta.len() < pos => true,
            Err(_) => true,
            _ => false,
        };
        if reopened {
            if let Ok(f) = open() {
                file = f;
                pos = 0;
            } else {
                std::thread::sleep(
                    std::time::Duration::from_millis(500),
                );
                continue;
            }
        }

        buf.clear();
        match file.read_to_string(&mut buf) {
            Ok(0) => {
                std::thread::sleep(
                    std::time::Duration::from_millis(250),
                );
            }
            Ok(n) => {
                pos += n as u64;
                for line in buf.lines() {
                    if *stop.lock().unwrap() { return; }
                    if !emit(parse_log_line(line)) { return; }
                }
            }
            Err(_) => break,
        }
    }
}

// ─── Combined streaming: fallback then node.log ──────────────────────────────

/// The source to stream while waiting for node.log to appear.
pub enum FallbackSource {
    /// Stream `docker logs -f quip-node`
    DockerLogs,
    /// Tail a file (e.g. node-output.log for native stdout capture)
    File(PathBuf),
}

/// Stream from the fallback source until node.log appears, then
/// switch to tailing node.log for the real mining activity.
///
/// `child_pid` is populated with the PID of the `docker logs -f` child
/// (when `fallback == DockerLogs`) so the owner can kill it explicitly
/// at stop time. For file-based fallbacks the slot stays `None`.
fn stream_with_fallback<F>(
    fallback: FallbackSource,
    stop: Arc<Mutex<bool>>,
    child_pid: Arc<Mutex<Option<u32>>>,
    emit: F,
) where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
{
    let log_path = node_log_path();
    let emit = Arc::new(emit);

    // node.log is only valid for Phase 2 if it was written during this
    // streamer's lifetime — otherwise it's leftover from a previous run
    // (e.g. an old node_log config) and backfilling it would flood the
    // UI with days-old content. Compare against stream start so the
    // file has to have been touched by the current process to count.
    //
    // This intentionally does *not* gate on `node_log` being set in
    // settings: the binary may write to ~/quip-data/node.log via its own
    // default even when the TOML field is omitted, and in that case we
    // still want Phase 2 to engage so the UI sees the real log stream
    // instead of an empty stdout capture.
    let stream_start = std::time::SystemTime::now();
    let is_current = |path: &std::path::Path| -> bool {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|mtime| mtime >= stream_start)
            .unwrap_or(false)
    };

    // Fast-path: node.log already being actively written when we started
    if is_current(&log_path) {
        if let Ok(meta) = std::fs::metadata(&log_path) {
            if meta.len() > 0 {
                tail_file(&log_path, &stop, &*emit);
                return;
            }
        }
    }

    // Phase 1: stream fallback while polling for node.log
    let stop2 = Arc::clone(&stop);
    let emit2 = Arc::clone(&emit);
    let fallback_stop = Arc::new(Mutex::new(false));
    let fallback_stop2 = Arc::clone(&fallback_stop);

    let fallback_handle = std::thread::spawn(move || {
        // Wrap the stop check: stop if either the global stop or
        // fallback_stop is signalled
        let combined_stop = Mutex::new(false);
        let check_stop = || {
            *stop2.lock().unwrap()
                || *fallback_stop2.lock().unwrap()
        };

        match fallback {
            FallbackSource::DockerLogs => {
                let mut child = match crate::cmd::new("docker")
                    .args([
                        "logs", "-f", "--tail", "100", "quip-node",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(_) => return,
                };

                *child_pid.lock().unwrap() = Some(child.id());

                let stdout = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();

                // Stream stderr on a second thread. quip-protocol's
                // console logger writes to stderr, so without this the
                // UI would see nothing until ~/quip-data/node.log
                // materialised via the Phase 2 poller.
                let stop_err = Arc::clone(&stop2);
                let fallback_stop_err = Arc::clone(&fallback_stop2);
                let emit_err = Arc::clone(&emit2);
                let stderr_thread = std::thread::spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        if *stop_err.lock().unwrap()
                            || *fallback_stop_err.lock().unwrap()
                        {
                            break;
                        }
                        if let Ok(line) = line {
                            if !emit_err(parse_log_line(&line)) {
                                break;
                            }
                        }
                    }
                });

                for line in BufReader::new(stdout).lines() {
                    if check_stop() { break; }
                    if let Ok(line) = line {
                        if !emit2(parse_log_line(&line)) { break; }
                    }
                }
                let _ = child.kill();
                *child_pid.lock().unwrap() = None;
                let _ = stderr_thread.join();
            }
            FallbackSource::File(path) => {
                // Tail the fallback file until told to stop
                tail_file(&path, &combined_stop, &|entry| {
                    if check_stop() { return false; }
                    emit2(entry)
                });
            }
        }
    });

    // Poll for node.log to be touched by the current run. A stale file
    // from a previous session (mtime before stream_start) is ignored,
    // so this loop naturally stays in fallback mode when nothing in
    // this run writes to node.log.
    loop {
        if *stop.lock().unwrap() { break; }
        if is_current(&log_path) {
            if let Ok(meta) = std::fs::metadata(&log_path) {
                if meta.len() > 0 { break; }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Signal fallback to stop and wait for it
    *fallback_stop.lock().unwrap() = true;
    let _ = fallback_handle.join();

    if *stop.lock().unwrap() { return; }

    // Phase 2: tail node.log
    tail_file(&log_path, &stop, &*emit);
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Spawn a thread that streams logs to the Tauri app.
/// Starts with the given fallback, switches to node.log once available.
pub fn start_log_stream_for_app(
    app: tauri::AppHandle,
    stop: Arc<Mutex<bool>>,
    child_pid: Arc<Mutex<Option<u32>>>,
    fallback: FallbackSource,
) {
    std::thread::spawn(move || {
        stream_with_fallback(fallback, stop, child_pid, move |entry| {
            app.emit("node-log", &entry).is_ok()
        });
    });
}

/// Start log streaming without Tauri — sends entries via mpsc channel.
pub fn start_log_stream_core(
    tx: SyncSender<LogEntry>,
    stop: Arc<Mutex<bool>>,
) {
    let child_pid = Arc::new(Mutex::new(None));
    std::thread::spawn(move || {
        stream_with_fallback(
            FallbackSource::DockerLogs,
            stop,
            child_pid,
            move |entry| tx.send(entry).is_ok(),
        );
    });
}

#[tauri::command]
pub async fn start_log_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, LogStreamState>,
) -> Result<(), String> {
    let _ = app.emit(
        "node-log",
        serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": "[log-stream] starting docker logs -f quip-node",
        }),
    );
    // Stop any existing streamer first, including killing its child.
    state.kill_child();
    *state.stop_flag.lock().unwrap() = true;
    std::thread::sleep(std::time::Duration::from_millis(200));
    *state.stop_flag.lock().unwrap() = false;

    let stop_flag = Arc::clone(&state.stop_flag);
    let child_pid = Arc::clone(&state.child_pid);
    let handle = std::thread::spawn(move || {
        stream_with_fallback(
            FallbackSource::DockerLogs,
            stop_flag,
            child_pid,
            move |entry| app.emit("node-log", &entry).is_ok(),
        );
    });

    *state.handle.lock().unwrap() = Some(handle);
    Ok(())
}

#[tauri::command]
pub async fn stop_log_stream(
    state: tauri::State<'_, LogStreamState>,
) -> Result<(), String> {
    // Kill the child FIRST so BufReader::lines() unblocks immediately.
    state.kill_child();
    *state.stop_flag.lock().unwrap() = true;
    Ok(())
}
