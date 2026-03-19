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

fn parse_log_line(line: &str) -> LogEntry {
    // Try to parse structured log: "2024-01-01T12:00:00Z INFO message"
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() == 3 {
        let ts = parts[0];
        let level = parts[1].to_uppercase();
        let level = match level.as_str() {
            "ERROR" | "ERR" => "ERROR",
            "WARN" | "WARNING" => "WARN",
            "INFO" => "INFO",
            "DEBUG" => "DEBUG",
            _ => "INFO",
        };
        LogEntry {
            timestamp: ts.to_string(),
            level: level.to_string(),
            message: parts[2].to_string(),
        }
    } else {
        LogEntry {
            timestamp: String::new(),
            level: "INFO".to_string(),
            message: line.to_string(),
        }
    }
}

/// Start log streaming without Tauri — sends entries via mpsc channel.
/// Call `*stop.lock().unwrap() = true` to stop the thread.
pub fn start_log_stream_core(tx: SyncSender<LogEntry>, stop: Arc<Mutex<bool>>) {
    std::thread::spawn(move || {
        let mut child = match Command::new("docker")
            .args(["logs", "-f", "--tail", "100", "quip-node"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let mut child = match Command::new("docker")
            .args(["logs", "-f", "--tail", "100", "quip-node"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
