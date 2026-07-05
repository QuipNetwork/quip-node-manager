// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ProgressSink` that feeds the TUI's log buffer, so the console UI drives the
//! same orchestration cores as the GUI.

use crate::log_stream::LogEntry;
use crate::progress::ProgressSink;
use std::sync::mpsc::Sender;

pub struct TuiSink {
    tx: Sender<LogEntry>,
}

impl TuiSink {
    pub fn new(tx: Sender<LogEntry>) -> Self {
        Self { tx }
    }

    fn push(&self, level: &str, message: String) {
        let _ = self.tx.send(LogEntry {
            timestamp: String::new(),
            level: level.to_string(),
            message,
        });
    }
}

impl ProgressSink for TuiSink {
    fn log(&self, level: &str, message: &str) {
        self.push(level, message.to_string());
    }

    fn pull_progress(&self, event: serde_json::Value) {
        // The TUI has no progress bars; surface image-level milestones only.
        if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
            if let Some(id) = event.get("id").and_then(|v| v.as_str()) {
                if event.get("parent_id").is_none() && id.starts_with("Image ") {
                    self.push(
                        "INFO",
                        format!("{text} {}", id.trim_start_matches("Image ")),
                    );
                }
            }
        }
    }

    fn pull_complete(&self, _gen: u64, success: bool, error: &str) {
        if !success {
            self.push("ERROR", format!("Pull failed: {error}"));
        }
    }

    fn stop_started(&self) {
        self.push("INFO", "Stopping…".to_string());
    }

    fn stop_complete(&self, success: bool, error: Option<&str>) {
        match (success, error) {
            (true, _) => self.push("INFO", "Stopped".to_string()),
            (false, Some(e)) => self.push("ERROR", format!("Stop failed: {e}")),
            (false, None) => self.push("ERROR", "Stop failed".to_string()),
        }
    }

    fn binary_download_progress(&self, downloaded: u64, total: Option<u64>, done: bool) {
        if done {
            self.push("INFO", "Binary download complete".to_string());
        } else if let Some(total) = total {
            let pct = if total > 0 {
                downloaded * 100 / total
            } else {
                0
            };
            self.push("INFO", format!("Downloading binary… {pct}%"));
        }
    }

    fn dashboard_db_mismatch(&self, message: &str) {
        self.push("ERROR", message.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn tui_sink_forwards_log_lines() {
        let (tx, rx) = mpsc::channel();
        let sink = TuiSink::new(tx);
        sink.log("WARN", "disk low");
        let entry = rx.recv().unwrap();
        assert_eq!(entry.level, "WARN");
        assert_eq!(entry.message, "disk low");
    }
}
