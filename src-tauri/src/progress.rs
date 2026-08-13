// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI-agnostic progress reporting. Orchestration code (compose/native) takes a
//! `&dyn ProgressSink` instead of a Tauri `AppHandle`, so the GUI (TauriSink)
//! and the TUI (TuiSink) drive identical backend logic.

use tauri::Emitter;

/// A sink for progress/log events emitted during long-running orchestration
/// (compose up/stop/pull, native spawn/stop, binary download).
pub trait ProgressSink: Send + Sync {
    /// A console log line (mirrors the `node-log` Tauri event).
    fn log(&self, level: &str, message: &str);
    /// A raw `docker compose --progress json` event (the `pull-progress` event).
    fn pull_progress(&self, event: serde_json::Value);
    /// The pull sequence finished (`pull-complete`). `gen` identifies the pull
    /// generation so the frontend can discard stale events.
    fn pull_complete(&self, gen: u64, success: bool, error: &str);
    /// Stop sequence began (`stop-started`).
    fn stop_started(&self);
    /// Stop sequence finished (`stop-complete`).
    fn stop_complete(&self, success: bool, error: Option<&str>);
    /// Native binary download progress (`binary-download-progress`).
    fn binary_download_progress(&self, downloaded: u64, total: Option<u64>, done: bool);
    /// Dashboard Postgres credential mismatch detected (`dashboard-db-mismatch`).
    fn dashboard_db_mismatch(&self, message: &str);
}

/// Emits progress as Tauri events for the desktop GUI frontend.
pub struct TauriSink {
    app: tauri::AppHandle,
}

impl TauriSink {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl ProgressSink for TauriSink {
    fn log(&self, level: &str, message: &str) {
        let _ = self.app.emit(
            "node-log",
            serde_json::json!({
                "timestamp": "",
                "level": level,
                "message": message,
                "source": "app",
            }),
        );
    }
    fn pull_progress(&self, event: serde_json::Value) {
        let _ = self.app.emit("pull-progress", event);
    }
    fn pull_complete(&self, gen: u64, success: bool, error: &str) {
        let _ = self.app.emit(
            "pull-complete",
            serde_json::json!({ "gen": gen, "success": success, "error": error }),
        );
    }
    fn stop_started(&self) {
        let _ = self.app.emit("stop-started", serde_json::json!({}));
    }
    fn stop_complete(&self, success: bool, error: Option<&str>) {
        let _ = self.app.emit(
            "stop-complete",
            serde_json::json!({ "success": success, "error": error }),
        );
    }
    fn binary_download_progress(&self, downloaded: u64, total: Option<u64>, done: bool) {
        let _ = self.app.emit(
            "binary-download-progress",
            serde_json::json!({ "downloaded": downloaded, "total": total, "done": done }),
        );
    }
    fn dashboard_db_mismatch(&self, message: &str) {
        let _ = self.app.emit(
            "dashboard-db-mismatch",
            serde_json::json!({ "message": message }),
        );
    }
}

/// No-op sink for tests and non-interactive callers.
pub struct NullSink;

impl ProgressSink for NullSink {
    fn log(&self, _level: &str, _message: &str) {}
    fn pull_progress(&self, _event: serde_json::Value) {}
    fn pull_complete(&self, _gen: u64, _success: bool, _error: &str) {}
    fn stop_started(&self) {}
    fn stop_complete(&self, _success: bool, _error: Option<&str>) {}
    fn binary_download_progress(&self, _downloaded: u64, _total: Option<u64>, _done: bool) {}
    fn dashboard_db_mismatch(&self, _message: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_sink_is_a_progress_sink() {
        // Compiles only if NullSink implements the trait and the trait is
        // object-safe (we pass &dyn ProgressSink everywhere).
        let sink: &dyn ProgressSink = &NullSink;
        sink.log("INFO", "hello");
        sink.pull_progress(serde_json::json!({"id": "x"}));
        sink.pull_complete(1, true, "");
        sink.stop_started();
        sink.stop_complete(true, None);
        sink.binary_download_progress(0, Some(10), false);
        sink.dashboard_db_mismatch("test");
    }
}
