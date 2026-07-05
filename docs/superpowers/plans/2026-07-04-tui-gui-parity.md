# TUI/GUI Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the ratatui TUI (`quip-node-manager --cli`) use the exact same backend orchestration code as the desktop GUI, so every GUI feature is present and behaves identically in the CLI.

**Architecture:** The GUI's orchestration functions (`start_stack`, `stop_stack`, `pull_compose_images`, `start_native_node`, `stop_native_node`, `download_native_binary`, `restart_to_update`) are coupled to Tauri's `AppHandle` because they report progress via `app.emit(...)`. The TUI has no `AppHandle`, so it reimplemented these with raw `compose_cmd()` calls that have drifted (notably `stop` runs `down`, destroying containers). We introduce a `ProgressSink` trait, extract each orchestration function into a `*_core(&dyn ProgressSink, …)` form, keep the `#[tauri::command]` functions as one-line wrappers over a `TauriSink`, and route the TUI through the same `*_core` functions via a `TuiSink`. Then we delete the TUI's duplicated orchestration and its parallel `FormState` model, and fill the remaining feature gaps (GPU survey, TLS config, health, updates).

**Tech Stack:** Rust, Tauri v2, ratatui, tokio, serde_json.

## Global Constraints

- License header on every new file: `// SPDX-License-Identifier: AGPL-3.0-or-later` (match existing `src-tauri/src/*.rs`).
- Hard limits (from CLAUDE.md): ≤100 lines/function, cyclomatic complexity ≤8, ≤5 positional params, 100-char lines, absolute imports only (no `..`), Google-style docstrings on non-trivial public APIs.
- Zero-warnings: `cargo clippy --all-targets` must be clean after every task.
- No LLM attribution trailers in commits. Imperative mood, ≤72-char subject.
- Run `cargo test` (from `src-tauri/`) after every task; it must stay green (baseline: 126 tests).
- Do NOT change GUI-observable behavior in Phases 0–1 (pure extraction). Existing tests are the guard.
- The TUI is synchronous; it already runs async backend work via `std::thread::spawn` + a `tokio` runtime + `mpsc` channels (see `start_checklist`/`start_port_check` in `tui_app.rs`). Reuse that pattern — do not block the render loop.

---

## File Structure

- Create: `src-tauri/src/progress.rs` — the `ProgressSink` trait + `TauriSink` (emits Tauri events) + `NullSink` (tests). One responsibility: abstract "how progress is reported."
- Create: `src-tauri/src/tui_sink.rs` — `TuiSink` (implements `ProgressSink` by sending `LogEntry`/status over an `mpsc` channel the TUI drains). Lives with the TUI, separate from the Tauri impl.
- Modify: `src-tauri/src/compose.rs` — extract `*_core(sink)` from `start_stack`/`stop_stack`/`pull_compose_images`; commands become wrappers.
- Modify: `src-tauri/src/native.rs` — same for `start_native_node`/`stop_native_node`/`download_native_binary`.
- Modify: `src-tauri/src/update.rs` — same for `restart_to_update` (Phase 4).
- Modify: `src-tauri/src/lib.rs` — register `mod progress;`; command wrappers.
- Modify: `src-tauri/src/tui_app.rs` — route start/stop through `*_core`; add survey; add missing config; delete `run_compose_step` and `FormState` duplication.
- Modify: `src-tauri/src/tui_ui.rs` — title version fix; render new fields/health.

---

## Phase 0 — The `ProgressSink` foundation (GUI behavior unchanged)

### Task 0.1: Define the `ProgressSink` trait and `TauriSink`

**Files:**
- Create: `src-tauri/src/progress.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod progress;`)
- Test: inline `#[cfg(test)]` in `src-tauri/src/progress.rs`

**Interfaces:**
- Produces:
  - `trait ProgressSink: Send + Sync` with methods:
    - `fn log(&self, level: &str, message: &str)`
    - `fn pull_progress(&self, event: serde_json::Value)`
    - `fn stop_started(&self)`
    - `fn stop_complete(&self, success: bool, error: Option<&str>)`
    - `fn binary_download_progress(&self, downloaded: u64, total: Option<u64>, done: bool)`
  - `struct TauriSink { app: tauri::AppHandle }` with `pub fn new(app: tauri::AppHandle) -> Self`, implementing `ProgressSink`.
  - `struct NullSink;` implementing `ProgressSink` as no-ops (for tests).

- [ ] **Step 1: Write the failing test**

```rust
// at bottom of src-tauri/src/progress.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_sink_is_a_progress_sink() {
        // Compiles only if NullSink implements the trait and the trait is
        // object-safe (we pass &dyn ProgressSink everywhere).
        let sink: &dyn ProgressSink = &NullSink;
        sink.log("INFO", "hello");
        sink.stop_started();
        sink.stop_complete(true, None);
        sink.binary_download_progress(0, Some(10), false);
        sink.pull_progress(serde_json::json!({"id": "x"}));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test progress::tests::null_sink_is_a_progress_sink`
Expected: FAIL to compile — `progress` module / `ProgressSink` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
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
    /// Stop sequence began (`stop-started`).
    fn stop_started(&self);
    /// Stop sequence finished (`stop-complete`).
    fn stop_complete(&self, success: bool, error: Option<&str>);
    /// Native binary download progress (`binary-download-progress`).
    fn binary_download_progress(&self, downloaded: u64, total: Option<u64>, done: bool);
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
            serde_json::json!({ "timestamp": "", "level": level, "message": message }),
        );
    }
    fn pull_progress(&self, event: serde_json::Value) {
        let _ = self.app.emit("pull-progress", event);
    }
    fn stop_started(&self) {
        let _ = self.app.emit("stop-started", serde_json::json!({}));
    }
    fn stop_complete(&self, success: bool, error: Option<&str>) {
        let _ = self
            .app
            .emit("stop-complete", serde_json::json!({ "success": success, "error": error }));
    }
    fn binary_download_progress(&self, downloaded: u64, total: Option<u64>, done: bool) {
        let _ = self.app.emit(
            "binary-download-progress",
            serde_json::json!({ "downloaded": downloaded, "total": total, "done": done }),
        );
    }
}

/// No-op sink for tests and non-interactive callers.
pub struct NullSink;

impl ProgressSink for NullSink {
    fn log(&self, _level: &str, _message: &str) {}
    fn pull_progress(&self, _event: serde_json::Value) {}
    fn stop_started(&self) {}
    fn stop_complete(&self, _success: bool, _error: Option<&str>) {}
    fn binary_download_progress(&self, _downloaded: u64, _total: Option<u64>, _done: bool) {}
}
```

Add to `src-tauri/src/lib.rs` near the other `pub mod` lines:

```rust
pub mod progress;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test progress:: && cargo clippy --all-targets`
Expected: PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/progress.rs src-tauri/src/lib.rs
git commit -m "Add ProgressSink trait to decouple orchestration from AppHandle"
```

---

### Task 0.2: Extract `pull_compose_images_core(sink)` — the template refactor

**Files:**
- Modify: `src-tauri/src/compose.rs` (`pull_compose_images` at line ~473; the `node-log`/`pull-progress` emitters it uses — the local `emit_pull_progress_json` and the `log`-style helpers around lines 36–57, 289–318)
- Test: existing `cargo test` suite is the guard (no behavior change).

**Interfaces:**
- Consumes: `crate::progress::{ProgressSink, TauriSink}` (Task 0.1).
- Produces: `pub(crate) async fn pull_compose_images_core(sink: &dyn ProgressSink) -> Result<(), String>`. The `#[tauri::command] pub async fn pull_compose_images(app: AppHandle)` becomes a wrapper: `pull_compose_images_core(&TauriSink::new(app)).await`.

- [ ] **Step 1: Read the current function and its emit calls**

Run: `cd src-tauri && sed -n '473,560p' src/compose.rs` and note every `app.emit("node-log", …)` and `app.emit("pull-progress", …)` and calls to `emit_pull_progress_json(app, …)`.

- [ ] **Step 2: Add the core function + convert the command to a wrapper**

Replace the signature and body so the command delegates. Concretely:

```rust
#[tauri::command]
pub async fn pull_compose_images(app: tauri::AppHandle) -> Result<(), String> {
    pull_compose_images_core(&crate::progress::TauriSink::new(app)).await
}

/// Pull the compose stack images, reporting per-image progress through `sink`.
pub(crate) async fn pull_compose_images_core(
    sink: &dyn crate::progress::ProgressSink,
) -> Result<(), String> {
    // ... original body, with every `app.emit("node-log", {level, message})`
    // replaced by `sink.log(level, &message)` and every
    // `app.emit("pull-progress", value)` replaced by `sink.pull_progress(value)`.
}
```

Change `emit_pull_progress_json(app: &AppHandle, …)` to `emit_pull_progress_json(sink: &dyn crate::progress::ProgressSink, …)` and inside it replace the `app.emit("node-log", …)` milestone mirror with `sink.log("INFO", &text)` and `app.emit("pull-progress", value)` with `sink.pull_progress(value)`. Update its call sites.

- [ ] **Step 3: Build**

Run: `cd src-tauri && cargo build 2>&1 | rg "error" | head`
Expected: no errors. Fix any `app` references left in the moved body (they should all be `sink.*` now).

- [ ] **Step 4: Run tests + clippy**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`
Expected: 126 tests pass; clippy clean. (No behavior change — the GUI still emits the same events via `TauriSink`.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/compose.rs
git commit -m "Extract pull_compose_images_core over a ProgressSink"
```

---

### Task 0.3: Extract `start_stack_core(sink)`

**Files:**
- Modify: `src-tauri/src/compose.rs` (`start_stack` at line ~584; its `node-log` helpers at lines ~36–57, `pull-progress` at ~522, and the pull it performs)

**Interfaces:**
- Produces: `pub(crate) async fn start_stack_core(sink: &dyn crate::progress::ProgressSink) -> Result<(), String>`; `#[tauri::command] start_stack(app)` wraps it.

- [ ] **Step 1: Convert command to wrapper + extract core**

```rust
#[tauri::command]
pub async fn start_stack(app: tauri::AppHandle) -> Result<(), String> {
    start_stack_core(&crate::progress::TauriSink::new(app)).await
}

/// Stage assets, pull, and `up -d` the compose stack, reporting via `sink`.
pub(crate) async fn start_stack_core(
    sink: &dyn crate::progress::ProgressSink,
) -> Result<(), String> {
    // original body; replace app.emit("node-log", …) -> sink.log(...);
    // replace any internal call to pull with pull_compose_images_core(sink).
}
```

Replace the module-local `emit`-style log helpers used inside `start_stack` (lines ~36–57) so they take `sink: &dyn ProgressSink` and call `sink.log(level, msg)`, or inline `sink.log(...)` at each call.

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build 2>&1 | rg error | head`
Expected: no errors.

- [ ] **Step 3: Tests + clippy**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`
Expected: green + clean.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/compose.rs
git commit -m "Extract start_stack_core over a ProgressSink"
```

---

### Task 0.4: Extract `stop_stack_core(sink)`

**Files:**
- Modify: `src-tauri/src/compose.rs` (`stop_stack` at line ~902; `stop-started` at ~903, `stop-complete` at ~925/929)

**Interfaces:**
- Produces: `pub(crate) async fn stop_stack_core(sink: &dyn crate::progress::ProgressSink) -> Result<(), String>`; command wraps it.

- [ ] **Step 1: Convert + extract**

```rust
#[tauri::command]
pub async fn stop_stack(app: tauri::AppHandle) -> Result<(), String> {
    stop_stack_core(&crate::progress::TauriSink::new(app)).await
}

pub(crate) async fn stop_stack_core(
    sink: &dyn crate::progress::ProgressSink,
) -> Result<(), String> {
    // original body; app.emit("stop-started", …) -> sink.stop_started();
    // app.emit("stop-complete", {success:true}) -> sink.stop_complete(true, None);
    // failure branch -> sink.stop_complete(false, Some(&err));
    // app.emit("node-log", …) -> sink.log(...).
}
```

- [ ] **Step 2: Build, test, clippy**

Run: `cd src-tauri && cargo build 2>&1 | rg error; cargo test && cargo clippy --all-targets`
Expected: no errors; green; clean.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/compose.rs
git commit -m "Extract stop_stack_core over a ProgressSink"
```

---

### Task 0.5: Extract native `start_native_node_core` / `stop_native_node_core` / `download_native_binary_core`

**Files:**
- Modify: `src-tauri/src/native.rs` (`start_native_node` ~741, `stop_native_node` ~951, `download_native_binary` ~499; emits at 301,335,520,598,632,773,798,833,872,880,905,925,955,999,1006)

**Interfaces:**
- Produces:
  - `pub(crate) async fn start_native_node_core(sink: &dyn ProgressSink, state: &NativeProcessState) -> Result<String, String>`
  - `pub(crate) async fn stop_native_node_core(sink: &dyn ProgressSink, state: &NativeProcessState) -> Result<(), String>`
  - `pub(crate) async fn download_native_binary_core(sink: &dyn ProgressSink) -> Result<String, String>`
- Each `#[tauri::command]` keeps its current signature (`app`, `state`) and wraps the core with `&TauriSink::new(app)`. `state: State<NativeProcessState>` is passed through as `&state`.

- [ ] **Step 1: Convert each command to a wrapper + extract core**

For `download_native_binary` (the one already touched for the coalescing guard — keep the guard and the skip-if-current logic inside `_core`):

```rust
#[tauri::command]
pub async fn download_native_binary(app: tauri::AppHandle) -> Result<String, String> {
    download_native_binary_core(&crate::progress::TauriSink::new(app)).await
}

pub(crate) async fn download_native_binary_core(
    sink: &dyn crate::progress::ProgressSink,
) -> Result<String, String> {
    // original body; replace the local `log` closure (which did app.emit("node-log"))
    // with `sink.log("INFO", &msg)`, and the two binary-download-progress emits
    // with `sink.binary_download_progress(downloaded, total, done)`.
}
```

Repeat the mechanical pattern for `start_native_node` and `stop_native_node`: keep the command signature, move the body into `*_core(sink, state)`, replace `app.emit("node-log", …)` → `sink.log(...)`, `app.emit("stop-started"/"stop-complete", …)` → `sink.stop_started()/stop_complete(...)`. Any `crate::checklist::trigger_recheck_auto(app, …)` spawns that need an `AppHandle` stay only in the command wrapper (the TUI has its own recheck path), so move those calls out of `_core` and into the wrapper.

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build 2>&1 | rg error | head`
Expected: no errors.

- [ ] **Step 3: Tests + clippy**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`
Expected: green + clean.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/native.rs
git commit -m "Extract native start/stop/download cores over a ProgressSink"
```

---

## Phase 1 — Route the TUI through the shared cores

### Task 1.1: Add `TuiSink`

**Files:**
- Create: `src-tauri/src/tui_sink.rs`
- Modify: `src-tauri/src/lib.rs` (`mod tui_sink;` behind the same cfg as the other `tui_*` modules)
- Test: inline test in `tui_sink.rs`

**Interfaces:**
- Consumes: `crate::progress::ProgressSink`, `crate::log_stream::LogEntry`.
- Produces: `struct TuiSink { tx: std::sync::mpsc::Sender<LogEntry> }` with `pub fn new(tx: Sender<LogEntry>) -> Self`, implementing `ProgressSink` by converting each callback into a `LogEntry` pushed onto `tx` (pull-progress → a summarized `LogEntry` line; stop/download → `LogEntry` lines).

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test tui_sink::tests::tui_sink_forwards_log_lines`
Expected: FAIL — module/type undefined.

- [ ] **Step 3: Implement `TuiSink`**

```rust
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
                    self.push("INFO", format!("{text} {}", id.trim_start_matches("Image ")));
                }
            }
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
            let pct = if total > 0 { downloaded * 100 / total } else { 0 };
            self.push("INFO", format!("Downloading binary… {pct}%"));
        }
    }
}
```

Confirm `LogEntry`'s fields (`timestamp`, `level`, `message`) via `rg "pub struct LogEntry" -A6 src/log_stream.rs`; adjust if the field names differ.

- [ ] **Step 4: Run test + clippy**

Run: `cd src-tauri && cargo test tui_sink:: && cargo clippy --all-targets`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tui_sink.rs src-tauri/src/lib.rs
git commit -m "Add TuiSink so the TUI can drive orchestration cores"
```

---

### Task 1.2: Replace TUI Docker start/stop with the shared cores

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (`start_node_docker` ~608, `stop_node` ~711, delete `run_compose_step` ~652)

**Interfaces:**
- Consumes: `compose::start_stack_core`, `compose::stop_stack_core`, `TuiSink`, the TUI's existing log channel `Sender<LogEntry>`.

- [ ] **Step 1: Identify the TUI's log sender**

Run: `rg -n "log_tx|Sender<LogEntry>|drain_logs|log_buf" src/tui_app.rs`. The TUI already drains a log channel each loop; reuse that `Sender`. If the start path currently runs on the UI thread, spawn a thread + `tokio` runtime exactly like `start_checklist` does.

- [ ] **Step 2: Rewrite `start_node_docker` to call the core**

```rust
fn start_node_docker(&mut self, config: &crate::settings::NodeConfig) {
    let mut settings = self.settings.clone();
    settings.node_config = config.clone();
    settings.image_tag = self.form.image_tag;
    settings.run_mode = RunMode::Docker;
    if let Err(e) = crate::settings::save_settings(&settings) {
        self.set_status(format!("Save error: {e}"));
        return;
    }
    let tx = self.log_tx.clone(); // the TUI's LogEntry sender
    std::thread::spawn(move || {
        let sink = crate::tui_sink::TuiSink::new(tx);
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _ = rt.block_on(crate::compose::start_stack_core(&sink));
    });
    self.set_status("Starting compose stack…");
    self.config_expanded = false;
}
```

This deletes the inline `sync_stack_assets`/`write_env_file`/`down`/`pull`/`up` sequence — `start_stack_core` does all of it (it stages assets, writes env, pulls with progress, and `up -d`). Verify `start_stack_core` reads `run_mode`/`image_tag` from persisted settings (it does, via `load_settings`), which is why we `save_settings` first.

- [ ] **Step 3: Rewrite `stop_node` (Docker branch) to call the core — fixes the `down` bug**

```rust
fn stop_node(&mut self) {
    match self.form.run_mode() {
        RunMode::Docker => {
            let tx = self.log_tx.clone();
            std::thread::spawn(move || {
                let sink = crate::tui_sink::TuiSink::new(tx);
                let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                let _ = rt.block_on(crate::compose::stop_stack_core(&sink));
            });
        }
        RunMode::Native => { /* replaced in Task 1.3 */ }
    }
    self.set_status("Stopping…");
    self.config_expanded = true;
    self.refresh_status();
}
```

Delete the `run_compose_step` helper (no longer referenced) and the `compose_cmd().args(["down"])` calls.

- [ ] **Step 4: Build, test, clippy**

Run: `cd src-tauri && cargo build 2>&1 | rg error; cargo test && cargo clippy --all-targets`
Expected: no errors; green; clean. (If `self.log_tx` doesn't exist under that name, use the actual field discovered in Step 1.)

- [ ] **Step 5: Manual verification (Docker host)**

Run the TUI, Start, then Stop. Confirm via `docker ps -a` that Stop leaves containers **Exited** (not removed) — the `down`→`stop` fix.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tui_app.rs
git commit -m "Route TUI Docker start/stop through shared cores (fixes stop=down)"
```

---

### Task 1.3: Replace TUI Native start/stop with the shared cores

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (`start_node_native` ~674, native branch of `stop_node`)

**Interfaces:**
- Consumes: `native::start_native_node_core(sink, state)`, `native::stop_native_node_core(sink, state)`, `NativeProcessState`.

- [ ] **Step 1: Check whether the TUI holds a `NativeProcessState`**

Run: `rg -n "NativeProcessState" src/tui_app.rs src/native.rs`. If the TUI has none, construct one owned by `TuiApp` (`NativeProcessState::default()` or the constructor `native.rs` uses) so the same child-handle/orphan logic applies.

- [ ] **Step 2: Rewrite the native branches to call the cores**

```rust
fn start_node_native(&mut self) {
    let tx = self.log_tx.clone();
    let state = self.native_state.clone(); // Arc-wrapped NativeProcessState
    std::thread::spawn(move || {
        let sink = crate::tui_sink::TuiSink::new(tx);
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _ = rt.block_on(crate::native::start_native_node_core(&sink, &state));
    });
    self.set_status("Starting native miner…");
}
```

This inherits binary auto-download, orphan detection, and validator-RPC wait for free. Do the same for the native branch of `stop_node` calling `stop_native_node_core(&sink, &state)`.

- [ ] **Step 3: Build, test, clippy**

Run: `cd src-tauri && cargo build 2>&1 | rg error; cargo test && cargo clippy --all-targets`
Expected: no errors; green; clean.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/tui_app.rs
git commit -m "Route TUI native start/stop through shared cores"
```

---

## Phase 2 — GPU/hardware survey in the TUI (the reported bug)

### Task 2.1: Run the hardware survey on TUI startup and populate GPU devices

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (constructor/`new`, and the GPU form init)
- Modify: `src-tauri/src/tui_ui.rs` (GPU render at ~374 — no change needed if `gpu_device_configs` is populated)

**Interfaces:**
- Consumes: `crate::hardware::run_survey() -> HardwareSurvey` (no `AppHandle`), `HardwareSurvey.gpu_devices: Vec<GpuDevice>`, `NodeConfig.gpu_device_configs`.
- Produces: a helper `merge_surveyed_gpus(node_config: &mut NodeConfig, survey: &HardwareSurvey)` that adds a `GpuDeviceConfig` for each detected device not already present, preserving saved `enabled`/`utilization`/`yielding`.

- [ ] **Step 1: Write the failing test**

```rust
// in src-tauri/src/tui_app.rs #[cfg(test)] mod tests
#[test]
fn merge_surveyed_gpus_adds_detected_and_preserves_saved() {
    use crate::hardware::{GpuDevice, HardwareSurvey};
    let mut nc = crate::settings::NodeConfig::default();
    // A previously-saved device 0 that the user had enabled.
    nc.gpu_device_configs = vec![crate::settings::GpuDeviceConfig {
        index: 0, enabled: true, utilization: 55, yielding: true,
    }];
    let survey = HardwareSurvey {
        gpu_backend: "cuda".into(),
        gpu_devices: vec![
            GpuDevice { index: 0, name: "RTX 3060".into(), memory_mb: Some(12288) },
            GpuDevice { index: 1, name: "RTX 3060".into(), memory_mb: Some(12288) },
        ],
        ..HardwareSurvey::default()
    };
    merge_surveyed_gpus(&mut nc, &survey);
    assert_eq!(nc.gpu_device_configs.len(), 2);
    assert!(nc.gpu_device_configs[0].enabled); // preserved
    assert_eq!(nc.gpu_device_configs[0].utilization, 55); // preserved
    assert!(!nc.gpu_device_configs[1].enabled); // new device defaults off
}
```

Confirm `GpuDeviceConfig`'s exact fields via `rg "pub struct GpuDeviceConfig" -A6 src/settings.rs` and `HardwareSurvey`/`GpuDevice` via `rg "pub struct HardwareSurvey|pub struct GpuDevice" -A8 src/hardware.rs`; adjust literals to match (e.g., if `HardwareSurvey` has no `Default`, build it explicitly).

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test tui_app::tests::merge_surveyed_gpus_adds_detected_and_preserves_saved`
Expected: FAIL — `merge_surveyed_gpus` undefined.

- [ ] **Step 3: Implement `merge_surveyed_gpus` + call it at TUI startup**

```rust
/// Add a GpuDeviceConfig for each surveyed device missing from `node_config`,
/// preserving any saved enabled/utilization/yielding for existing indices.
fn merge_surveyed_gpus(
    node_config: &mut crate::settings::NodeConfig,
    survey: &crate::hardware::HardwareSurvey,
) {
    for dev in &survey.gpu_devices {
        if !node_config.gpu_device_configs.iter().any(|c| c.index == dev.index) {
            node_config.gpu_device_configs.push(crate::settings::GpuDeviceConfig {
                index: dev.index,
                enabled: false,
                utilization: 80,
                yielding: false,
            });
        }
    }
    node_config.gpu_device_configs.sort_by_key(|c| c.index);
}
```

In `TuiApp::new` (after loading settings, before building `FormState`), run the survey and merge:

```rust
let survey = crate::hardware::run_survey();
merge_surveyed_gpus(&mut settings.node_config, &survey);
```

Store `survey` on `TuiApp` if the render needs the device names (the GPU render currently shows `GPU {index}` — optionally extend it to include `dev.name` from the stored survey).

- [ ] **Step 4: Run test + clippy**

Run: `cd src-tauri && cargo test tui_app:: && cargo clippy --all-targets`
Expected: PASS, clean.

- [ ] **Step 5: Manual verification (GPU host)**

Run `quip-node-manager --cli` on the RTX 3060 box. Confirm the Configuration → GPU section lists `GPU 0` (RTX 3060) instead of "No GPUs detected".

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tui_app.rs
git commit -m "Run hardware survey in the TUI so GPUs are detected"
```

---

## Phase 3 — Remove the duplicated model and fill config gaps

### Task 3.1: Fix the stale TUI title version

**Files:**
- Modify: `src-tauri/src/tui_ui.rs:70`

- [ ] **Step 1: Replace the hardcoded version**

```rust
fn title_span() -> Span<'static> {
    Span::styled(
        concat!(" Quip Node Manager v", env!("CARGO_PKG_VERSION"), " "),
        // ... keep the existing Style
    )
}
```

- [ ] **Step 2: Build + clippy**

Run: `cd src-tauri && cargo build 2>&1 | rg error; cargo clippy --all-targets`
Expected: no errors; clean.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tui_ui.rs
git commit -m "Show real version in TUI title (was hardcoded v0.1.0)"
```

---

### Task 3.2: Add v0.2 TLS/Caddy config to the TUI

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (`FocusId`, `FormState`, `activate`, `to_node_config`/settings mapping)
- Modify: `src-tauri/src/tui_ui.rs` (render a TLS subsection)

**Interfaces:**
- Consumes: the same `AppSettings` fields the GUI writes — confirm via `rg -n "tls_enabled|hostname|cert_email|zerossl_api_key" src/settings.rs` and how `app.js` maps them (`tls-enabled`, `hostname`, `cert-email`, `zerossl-api-key`).

- [ ] **Step 1: Add focus ids + form fields**

Add `TlsEnabled`, `Hostname`, `CertEmail`, `ZerosslApiKey` to `FocusId`; add matching `String`/`bool` fields to `FormState`; map them in `FormState::from_settings` and back in `apply`/`to_*`. Follow the exact pattern of the existing `qpu_api_key` field (checkbox + text edits).

- [ ] **Step 2: Render the TLS subsection**

In `tui_ui.rs`, after the D-Wave block, render a `TLS / Caddy` group: an enable checkbox, then (when enabled) Hostname, ACME Email, ZeroSSL API Key fields, mirroring the D-Wave rendering block.

- [ ] **Step 3: Build, test, clippy**

Run: `cd src-tauri && cargo build 2>&1 | rg error; cargo test && cargo clippy --all-targets`
Expected: no errors; green; clean.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/tui_app.rs src-tauri/src/tui_ui.rs
git commit -m "Expose v0.2 TLS/Caddy config in the TUI"
```

---

### Task 3.3: Collapse `FormState` duplication (guardrail against future drift)

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (`FormState`)

**Interfaces:**
- Goal: `FormState` holds only genuinely-transient edit state (in-progress text buffers); all committed values read/write `AppSettings`/`NodeConfig` directly, so a new GUI config field is automatically available to the TUI form mapping.

- [ ] **Step 1: Audit which `FormState` fields mirror `NodeConfig`**

Run: `rg -n "form\.[a-z_]+" src/tui_app.rs src/tui_ui.rs | sort -u`. For each, decide: keep (transient) or replace with direct `self.settings.node_config.<field>` access.

- [ ] **Step 2: Replace mirrored fields with direct accessors**

Change render/edit sites to read `self.settings.node_config.*` and write on commit. Remove the now-dead `FormState` fields and their `from_settings`/`to_node_config` lines. Keep `edit_buf`/`run_mode_idx`/`image_tag` if they're genuinely UI-only.

- [ ] **Step 3: Build, test, clippy**

Run: `cd src-tauri && cargo build 2>&1 | rg error; cargo test && cargo clippy --all-targets`
Expected: no errors; green; clean.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/tui_app.rs
git commit -m "Reduce FormState to transient edit state; read config directly"
```

---

### Task 3.4: Swap local secret regen for the shared generator; add CPU-enable + data-dir change

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (`regenerate_secret` ~758; add CPU-enable focus/field; add data-dir edit + restart)

**Interfaces:**
- Consumes: `crate::secret::generate_node_secret()` (confirm exact path via `rg -n "fn generate_node_secret" src/secret.rs`); `crate::settings::set_data_dir`-equivalent (confirm the command body in `lib.rs`).

- [ ] **Step 1: Replace RNG block with the shared call**

```rust
fn regenerate_secret(&mut self) {
    match crate::secret::generate_node_secret() {
        Ok(secret) => { self.node_secret = secret; self.set_status("Secret regenerated"); }
        Err(e) => self.set_status(format!("Secret error: {e}")),
    }
}
```

- [ ] **Step 2: Add CPU-enable toggle + data-dir edit**

Add a `CpuEnabled` focus/checkbox bound to the same `NodeConfig` field the GUI's `cpu-enabled` writes. Add a data-dir text field that on commit calls the same backend `set_data_dir` logic the GUI uses, then triggers a restart/reload.

- [ ] **Step 3: Build, test, clippy; Commit**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`

```bash
git add src-tauri/src/tui_app.rs
git commit -m "TUI: shared secret generator, CPU-enable toggle, data-dir change"
```

---

## Phase 4 — Status/health/update parity

### Task 4.1: Health panel in the TUI

**Files:**
- Modify: `src-tauri/src/tui_app.rs` (periodic `get_health` poll like the 5s status refresh)
- Modify: `src-tauri/src/tui_ui.rs` (render infra/chain/participation lines under status)

**Interfaces:**
- Consumes: `crate::health::get_health()` (confirm signature via `rg -n "pub async fn get_health|pub fn get_health" src/health.rs`; it may need only settings/state, no `AppHandle`).

- [ ] **Step 1: Poll health on the existing refresh cadence**

In the main-loop refresh (currently every 5s), spawn a thread that runs `get_health()` and sends the `HealthReport` over an `mpsc` the loop drains into `app.health`.

- [ ] **Step 2: Render three health lines** matching the GUI (Infrastructure / Chain / Participation with state + detail).

- [ ] **Step 3: Build, test, clippy; Commit**

```bash
git add src-tauri/src/tui_app.rs src-tauri/src/tui_ui.rs
git commit -m "Add node health panel to the TUI"
```

---

### Task 4.2: Update checks + Restart-to-Update in the TUI

**Files:**
- Modify: `src-tauri/src/update.rs` (extract `restart_to_update_core(sink, …)` per the Phase 0 pattern; the step-runner already calls the cores)
- Modify: `src-tauri/src/tui_app.rs` (poll `check_app_update`/image/binary; add a Restart-to-Update action)
- Modify: `src-tauri/src/tui_ui.rs` (show an "update available" marker + action)

**Interfaces:**
- Consumes: `check_app_update`/`check_image_update`/`check_binary_update` (all no `AppHandle` for the check itself — confirm), and `restart_to_update_core(sink)`.

- [ ] **Step 1: Extract `restart_to_update_core`** exactly like Task 0.5 (the plan pattern), routing its per-step progress through `sink`. The command wraps it with `TauriSink`.

- [ ] **Step 2: Poll update checks** on a long cadence in the TUI; store `update_available` and render a marker; wire an action that runs `restart_to_update_core(&TuiSink)`.

- [ ] **Step 3: Build, test, clippy; Commit**

```bash
git add src-tauri/src/update.rs src-tauri/src/tui_app.rs src-tauri/src/tui_ui.rs
git commit -m "TUI: update checks and Restart-to-Update via shared cores"
```

---

### Task 4.3: Native log tail + dashboard DB reset + log copy/clear in the TUI

**Files:**
- Modify: `src-tauri/src/tui_app.rs`, `src-tauri/src/tui_ui.rs`

**Interfaces:**
- Consumes: `native::start_native_log_tail`-equivalent core, `compose::reset_dashboard_database` (confirm it needs no `AppHandle`, or extract a `_core`).

- [ ] **Step 1: In native mode, tail `node.log`** into the TUI log buffer (reuse the log_stream Phase-2 tail the GUI uses).
- [ ] **Step 2: Add a Reset-dashboard-DB action** calling the shared function.
- [ ] **Step 3: Add log copy (to a file, since a TUI has no clipboard) and clear actions.**
- [ ] **Step 4: Build, test, clippy; Commit**

```bash
git add src-tauri/src/tui_app.rs src-tauri/src/tui_ui.rs
git commit -m "TUI: native log tail, dashboard reset, log copy/clear"
```

---

## Phase 5 — Guardrail against re-drift

### Task 5.1: Regression test that the TUI uses shared cores, not raw compose

**Files:**
- Create/Modify: a test in `src-tauri/src/tui_app.rs` `#[cfg(test)]` or a source-lint test.

- [ ] **Step 1: Write a source-guard test**

```rust
#[test]
fn tui_does_not_reimplement_compose_stop_via_down() {
    // Guard against the stop=down regression returning. The TUI must route
    // through compose::stop_stack_core, never `docker compose down`.
    let src = include_str!("tui_app.rs");
    assert!(
        !src.contains("args([\"down\"])"),
        "TUI must not call `docker compose down` directly; use stop_stack_core"
    );
    assert!(
        !src.contains("run_compose_step"),
        "TUI must not have its own compose step runner"
    );
}
```

- [ ] **Step 2: Run + clippy**

Run: `cd src-tauri && cargo test tui_app::tests::tui_does_not_reimplement_compose_stop_via_down && cargo clippy --all-targets`
Expected: PASS, clean.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tui_app.rs
git commit -m "Guard against TUI re-drifting from shared orchestration"
```

---

## Self-Review Notes

- **Spec coverage:** Every ❌/⚠️ row in the survey maps to a task — start/stop (1.2/1.3), stop=down bug (1.2), GPU survey (2.1), TLS (3.2), FormState drift (3.3), secret/CPU/data-dir (3.4), health (4.1), updates+restart (4.2), native log tail/dashboard reset/log copy (4.3), title version (3.1), guardrail (5.1). Dashboard *iframe* is intentionally out of scope (a TUI cannot embed a web view); only its Reset action is ported.
- **Ordering:** Phase 0 must land before Phase 1 (cores before callers). Phases 2–5 are independent of each other and each is shippable alone.
- **Risk:** Phases 0–1 are the highest-value and highest-risk (touch GUI code paths). The existing 126 tests + the "no GUI behavior change" rule are the guard; if any GUI test changes output, the extraction was not faithful — revert and redo that function.
- **Field-name caveats:** several tasks note "confirm exact fields via `rg`" because `LogEntry`, `GpuDeviceConfig`, `HardwareSurvey`, and the health/update signatures must be read at implementation time rather than trusted from this plan.
