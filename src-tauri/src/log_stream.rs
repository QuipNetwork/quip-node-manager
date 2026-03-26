// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::SyncSender;
use tauri::Emitter;

#[derive(Serialize, Clone, Debug)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

pub struct LogStreamState(pub Arc<Mutex<Option<std::thread::JoinHandle<()>>>>, pub Arc<Mutex<bool>>);

impl LogStreamState {
    pub fn new() -> Self {
        LogStreamState(Arc::new(Mutex::new(None)), Arc::new(Mutex::new(false)))
    }
}

pub fn parse_log_line(line: &str) -> LogEntry {
    // Format: [file.py:123][node] 2026-01-01T12:00:00+00:00 LEVEL - message
    // Or Python: LEVEL:module:message
    // Otherwise: pass through verbatim.

    // Try structured quip-protocol format
    if line.starts_with('[') {
        // Find the timestamp after the ] ] prefix
        if let Some(after_brackets) = line.find("] ").map(|i| {
            let rest = &line[i + 2..];
            // There might be a second bracket pair
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

/// Start log streaming without Tauri — sends entries via mpsc channel.
/// Call `*stop.lock().unwrap() = true` to stop the thread.
pub fn start_log_stream_core(tx: SyncSender<LogEntry>, stop: Arc<Mutex<bool>>) {
    std::thread::spawn(move || {
        // Use shell to merge stdout+stderr so we get both entrypoint
        // and Python logging output in one stream.
        let mut child = match Command::new("sh")
            .args(["-c", "docker logs -f --tail 100 quip-node 2>&1"])
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };

        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            if *stop.lock().unwrap() {
                break;
            }
            if let Ok(line) = line {
                let entry = parse_log_line(&line);
                if tx.send(entry).is_err() {
                    break;
                }
            }
        }
        let _ = child.kill();
    });
}

#[tauri::command]
pub async fn start_log_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, LogStreamState>,
) -> Result<(), String> {
    // Signal any existing stream to stop
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
        let mut child = match Command::new("sh")
            .args(["-c", "docker logs -f --tail 100 quip-node 2>&1"])
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };

        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            if *stop_flag.lock().unwrap() {
                break;
            }
            if let Ok(line) = line {
                let entry = parse_log_line(&line);
                let _ = app.emit("node-log", &entry);
            }
        }
        let _ = child.kill();
    });

    *state.0.lock().unwrap() = Some(handle);
    Ok(())
}

#[tauri::command]
pub async fn stop_log_stream(state: tauri::State<'_, LogStreamState>) -> Result<(), String> {
    let mut stop = state.1.lock().unwrap();
    *stop = true;
    Ok(())
}
