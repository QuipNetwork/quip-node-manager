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

pub struct LogStreamState(
    pub Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    pub Arc<Mutex<bool>>,
);

impl LogStreamState {
    pub fn new() -> Self {
        LogStreamState(
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(false)),
        )
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
fn stream_with_fallback<F>(
    fallback: FallbackSource,
    stop: Arc<Mutex<bool>>,
    emit: F,
) where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
{
    let log_path = node_log_path();
    let emit = Arc::new(emit);

    // If node.log already exists and has content, skip the fallback
    if log_path.exists() {
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

    // Poll for node.log to appear with content
    loop {
        if *stop.lock().unwrap() { break; }
        if log_path.exists() {
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
    fallback: FallbackSource,
) {
    std::thread::spawn(move || {
        stream_with_fallback(fallback, stop, move |entry| {
            app.emit("node-log", &entry).is_ok()
        });
    });
}

/// Start log streaming without Tauri — sends entries via mpsc channel.
pub fn start_log_stream_core(
    tx: SyncSender<LogEntry>,
    stop: Arc<Mutex<bool>>,
) {
    std::thread::spawn(move || {
        stream_with_fallback(
            FallbackSource::DockerLogs,
            stop,
            move |entry| tx.send(entry).is_ok(),
        );
    });
}

#[tauri::command]
pub async fn start_log_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, LogStreamState>,
) -> Result<(), String> {
    {
        let mut stop = state.1.lock().unwrap();
        *stop = true;
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    {
        let mut stop = state.1.lock().unwrap();
        *stop = false;
    }

    let stop_flag = Arc::clone(&state.1);
    let handle = std::thread::spawn(move || {
        stream_with_fallback(
            FallbackSource::DockerLogs,
            stop_flag,
            move |entry| app.emit("node-log", &entry).is_ok(),
        );
    });

    *state.0.lock().unwrap() = Some(handle);
    Ok(())
}

#[tauri::command]
pub async fn stop_log_stream(
    state: tauri::State<'_, LogStreamState>,
) -> Result<(), String> {
    let mut stop = state.1.lock().unwrap();
    *stop = true;
    Ok(())
}
