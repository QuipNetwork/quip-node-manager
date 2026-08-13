// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ProgressSink` that feeds the TUI's log buffer, so the console UI drives the
//! same orchestration cores as the GUI.

use crate::log_stream::LogEntry;
use crate::progress::ProgressSink;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;

pub struct TuiSink {
    tx: SyncSender<LogEntry>,
    last_pct: AtomicU64,
}

impl TuiSink {
    pub fn new(tx: SyncSender<LogEntry>) -> Self {
        Self {
            tx,
            last_pct: AtomicU64::new(u64::MAX),
        }
    }

    fn push(&self, level: &str, message: String) {
        let _ = self.tx.send(LogEntry {
            timestamp: String::new(),
            level: level.to_string(),
            message,
            source: "app".to_string(),
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
            if let Some(pct) = (downloaded * 100).checked_div(total) {
                let prev_pct = self.last_pct.load(Ordering::Relaxed);
                if pct != prev_pct {
                    self.last_pct.store(pct, Ordering::Relaxed);
                    self.push("INFO", format!("Downloading binary… {pct}%"));
                }
            }
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
        let (tx, rx) = mpsc::sync_channel(64);
        let sink = TuiSink::new(tx);
        sink.log("WARN", "disk low");
        let entry = rx.recv().unwrap();
        assert_eq!(entry.level, "WARN");
        assert_eq!(entry.message, "disk low");
    }

    #[test]
    fn tui_sink_deduplicates_binary_download_progress() {
        let (tx, rx) = mpsc::sync_channel(64);
        let sink = TuiSink::new(tx);

        // Simulate download at 1000 total bytes: 1, 2, 3 all truncate to 0%
        sink.binary_download_progress(1, Some(1000), false);
        sink.binary_download_progress(2, Some(1000), false);
        sink.binary_download_progress(3, Some(1000), false);

        // Now 25% (250 / 1000 = 25) — should emit
        sink.binary_download_progress(250, Some(1000), false);

        // Complete
        sink.binary_download_progress(1000, Some(1000), true);

        // Collect received messages
        let mut messages = Vec::new();
        while let Ok(entry) = rx.try_recv() {
            messages.push(entry.message);
        }

        // Should be 3 messages: 0%, 25%, complete
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], "Downloading binary… 0%");
        assert_eq!(messages[1], "Downloading binary… 25%");
        assert_eq!(messages[2], "Binary download complete");
    }
}
