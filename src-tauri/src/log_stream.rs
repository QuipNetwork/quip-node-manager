// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use tauri::Emitter;

fn default_log_source() -> String {
    "app".to_string()
}

#[derive(Serialize, Clone, Debug)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    /// Origin of the line: `miner`, `validator`, `dashboard`, `postgres`,
    /// `caddy`, or `app`. Defaults to `app` for ops/manager messages and for
    /// callers that omit the field when deserialising.
    #[serde(default = "default_log_source")]
    pub source: String,
}

impl LogEntry {
    fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

/// Shared state for the Docker logs streamer.
///
/// `child_pid` holds the PID of the in-flight `docker compose logs -f` process
/// whenever one is running. Killing this child at stop time unblocks
/// `BufReader::lines()` immediately instead of waiting for the next log
/// line — critical because Docker stop isn't visible to the streamer
/// until the daemon closes the pipe.
pub struct LogStreamState {
    session: Mutex<Option<LogSession>>,
}

struct LogSession {
    stop: Arc<Mutex<bool>>,
    child_pid: Arc<Mutex<Option<u32>>>,
}

impl LogSession {
    fn stop(&self) {
        // Same lock order as child registration: cancellation wins even if
        // Docker is spawning when the user stops or replaces this session.
        let mut stop = self.stop.lock().unwrap();
        *stop = true;
        if let Some(pid) = self.child_pid.lock().unwrap().take() {
            kill_log_child(pid);
        }
    }
}

impl Default for LogStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl LogStreamState {
    pub fn new() -> Self {
        LogStreamState {
            session: Mutex::new(None),
        }
    }

    /// Cancel the current follower and prevent it from reconnecting.
    pub fn stop(&self) {
        if let Some(session) = self.session.lock().unwrap().take() {
            session.stop();
        }
    }

    fn start<F>(&self, sources: Vec<StreamSource>, emit: F)
    where
        F: Fn(LogEntry) -> bool + Send + Sync + 'static,
    {
        let mut current = self.session.lock().unwrap();
        if let Some(previous) = current.take() {
            previous.stop();
        }
        let stop = Arc::new(Mutex::new(false));
        let child_pid = Arc::new(Mutex::new(None));
        *current = Some(LogSession {
            stop: Arc::clone(&stop),
            child_pid: Arc::clone(&child_pid),
        });
        std::thread::spawn(move || {
            let cancelled = Arc::clone(&stop);
            stream_multiplexed(sources, stop, child_pid, move |entry| {
                !*cancelled.lock().unwrap() && emit(entry)
            });
        });
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

/// Parse Caddy's console encoder output: tab-separated
/// `ts<TAB>LEVEL<TAB>logger<TAB>message<TAB>{fields}`. The logger name and the
/// field object are both optional, so everything past the level is kept as the
/// message.
///
/// The stack pins `format console` in the Caddyfile's global `log` block
/// (Caddy would otherwise emit JSON, because stderr is not a terminal under
/// compose). Without this branch every Caddy line falls through to the
/// plain-text case below and a 502 renders as INFO.
fn parse_caddy_console_line(line: &str) -> Option<LogEntry> {
    let mut parts = line.split('\t');
    let timestamp = parts.next()?;
    let level = match parts.next()? {
        "DEBUG" => "DEBUG",
        "INFO" => "INFO",
        "WARN" => "WARN",
        // PANIC and FATAL are terminal; the pane has no louder level than
        // ERROR, so they land there rather than being dropped.
        "ERROR" | "PANIC" | "FATAL" => "ERROR",
        _ => return None,
    };
    let message = parts.collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        return None;
    }
    Some(LogEntry {
        timestamp: timestamp.to_string(),
        level: level.to_string(),
        message,
        source: default_log_source(),
    })
}

pub fn parse_log_line(line: &str) -> LogEntry {
    // Format: [file.py:123][node] 2026-01-01T12:00:00+00:00 LEVEL - message
    // Or Caddy console: ts<TAB>LEVEL<TAB>logger<TAB>message<TAB>{fields}
    // Or Python: LEVEL:module:message
    // Otherwise: pass through verbatim.

    if let Some(entry) = parse_caddy_console_line(line) {
        return entry;
    }

    // Try structured quip-protocol format
    if line.starts_with('[') {
        if let Some(after_brackets) = line.find("] ").and_then(|i| {
            let rest = &line[i + 2..];
            if rest.starts_with('[') {
                rest.find("] ").map(|j| &rest[j + 2..])
            } else {
                Some(rest)
            }
        }) {
            let parts: Vec<&str> = after_brackets.splitn(3, ' ').collect();
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
                    source: default_log_source(),
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
                source: default_log_source(),
            };
        }
    }

    // Plain text — pass through verbatim
    LogEntry {
        timestamp: String::new(),
        level: "INFO".to_string(),
        message: line.to_string(),
        source: default_log_source(),
    }
}

/// Parse docker compose's `servicename  | line` prefix (emitted when
/// `--no-log-prefix` is NOT passed). Returns `(service, rest)` on match.
///
/// Compose right-pads the service name so the `|` column aligns; we trim
/// trailing spaces on the left-hand side.
pub fn parse_compose_prefix(line: &str) -> Option<(&str, &str)> {
    let pipe = line.find(" | ")?;
    let service = line[..pipe].trim_end();
    if service.is_empty() {
        return None;
    }
    // Service names are a single token: letters, digits, hyphen, underscore.
    if !service
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some((service, &line[pipe + 3..]))
}

/// Map a compose service name (YAML key) to a UI `source` tag.
/// `cpu`/`cuda` both map to `miner`. Unknown services return `None`.
pub fn map_compose_service_to_source(service: &str) -> Option<&'static str> {
    // Compose adds a replica number when container_name is not specified.
    let service = match service.rsplit_once('-') {
        Some((name, replica))
            if !replica.is_empty() && replica.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => service,
    };
    match service {
        "cpu" | "cuda" | "quip-cpu" | "quip-cuda" => Some("miner"),
        "quip-validator" | "validator" => Some("validator"),
        "dashboard" | "quip-dashboard" => Some("dashboard"),
        "postgres" | "quip-postgres" => Some("postgres"),
        "caddy" | "quip-caddy" => Some("caddy"),
        _ => None,
    }
}

/// Turn a raw compose log line into a tagged `LogEntry`. Lines with no
/// recognizable service prefix keep `last_source` (or `"app"` when empty).
fn entry_from_compose_line(line: &str, last_source: &mut String) -> LogEntry {
    if let Some((service, rest)) = parse_compose_prefix(line) {
        if let Some(src) = map_compose_service_to_source(service) {
            *last_source = src.to_string();
            return parse_log_line(rest).with_source(src);
        }
    }
    let source = if last_source.is_empty() {
        default_log_source()
    } else {
        last_source.clone()
    };
    parse_log_line(line).with_source(source)
}

// ─── File tailing ────────────────────────────────────────────────────────────

/// Tail a log file: backfill last 200 lines, then follow new output.
/// Handles rotation/truncation by reopening when the file shrinks.
fn tail_file<F>(path: &std::path::Path, stop: &Mutex<bool>, emit: &F)
where
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
        if *stop.lock().unwrap() {
            return;
        }
        if !emit(parse_log_line(line)) {
            return;
        }
    }

    let mut pos = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let mut buf = String::new();
    loop {
        if *stop.lock().unwrap() {
            break;
        }

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
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
        }

        buf.clear();
        match file.read_to_string(&mut buf) {
            Ok(0) => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(n) => {
                pos += n as u64;
                for line in buf.lines() {
                    if *stop.lock().unwrap() {
                        return;
                    }
                    if !emit(parse_log_line(line)) {
                        return;
                    }
                }
            }
            Err(_) => break,
        }
    }
}

// ─── Multi-source fan-in ─────────────────────────────────────────────────────

/// One concurrent input to the unified log streamer.
pub enum StreamSource {
    /// `docker compose logs -f --tail 100` with no service filter and with
    /// the default service prefix (`servicename  | line`). Prefixes are
    /// parsed into `LogEntry.source`.
    ComposeAll,
    /// Tail a host file, tagging every line with a fixed `source`.
    File { path: PathBuf, source: &'static str },
}

/// Sources for the given run mode.
///
/// - Docker: one compose multiplexer covering miner + support services.
/// - Native: host `node-output.log` (miner) plus the same compose
///   multiplexer for the containerized support services.
pub fn sources_for_run_mode(run_mode: &crate::settings::RunMode) -> Vec<StreamSource> {
    match run_mode {
        crate::settings::RunMode::Docker => vec![StreamSource::ComposeAll],
        crate::settings::RunMode::Native => vec![
            StreamSource::File {
                path: crate::settings::data_dir().join("node-output.log"),
                source: "miner",
            },
            StreamSource::ComposeAll,
        ],
    }
}

/// Fan-in every source concurrently into a single tagged stream.
///
/// `child_pid` is populated with the PID of the `docker compose logs -f`
/// child (when a `ComposeAll` source is present) so the owner can kill it
/// at stop time. File-only runs leave the slot `None`.
fn stream_multiplexed<F>(
    sources: Vec<StreamSource>,
    stop: Arc<Mutex<bool>>,
    child_pid: Arc<Mutex<Option<u32>>>,
    emit: F,
) where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
{
    let emit = Arc::new(emit);
    let mut handles = Vec::with_capacity(sources.len());

    for source in sources {
        let stop = Arc::clone(&stop);
        let emit = Arc::clone(&emit);
        let child_pid = Arc::clone(&child_pid);
        handles.push(std::thread::spawn(move || match source {
            StreamSource::ComposeAll => {
                stream_compose_all(&stop, &child_pid, emit);
            }
            StreamSource::File { path, source } => {
                tail_file(&path, &stop, &|entry| emit(entry.with_source(source)));
            }
        }));
    }

    for handle in handles {
        let _ = handle.join();
    }
}

/// Follow all compose services; parse the `service | line` prefix into
/// `source`. Stderr is drained on a second thread (compose / container
/// loggers often write there).
fn stream_compose_all<F>(stop: &Arc<Mutex<bool>>, child_pid: &Arc<Mutex<Option<u32>>>, emit: Arc<F>)
where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
{
    // Reuse the shared builder so log streaming sees the same compose model
    // as the rest of the app, including any operator docker-compose.override.yml.
    // No service argument → follow every service in the project.
    // No `--no-log-prefix` → compose emits `servicename  | line`.
    //
    // Both profiles are required, exactly as the stop path needs them: every
    // service in this stack sits behind a profile, so a profile-less
    // `docker compose logs` resolves an empty service set and prints nothing
    // at all — not an error, just silence. Passing cpu *and* cuda covers the
    // stack whichever miner flavour is active, and the support services come
    // along with either.
    stream_compose_with_command(stop, child_pid, emit, || {
        if !crate::stack_assets::stack_compose_file().exists() {
            return None;
        }
        let mut command = crate::compose::compose_cmd();
        command
            .args(["--profile", "cpu", "--profile", "cuda"])
            .args(["logs", "-f", "--tail", "100"]);
        Some(command)
    });
}

fn stream_compose_with_command<F, C>(
    stop: &Arc<Mutex<bool>>,
    child_pid: &Arc<Mutex<Option<u32>>>,
    emit: Arc<F>,
    build: C,
) where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
    C: Fn() -> Option<std::process::Command>,
{
    while !*stop.lock().unwrap() {
        if let Some(command) = build() {
            stream_compose_once(stop, child_pid, Arc::clone(&emit), command);
        }
        // A fresh install may not have a Compose file yet. Docker can also
        // disconnect while containers are being recreated. Retry until Stop.
        for _ in 0..20 {
            if *stop.lock().unwrap() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn stream_compose_once<F>(
    stop: &Arc<Mutex<bool>>,
    child_pid: &Arc<Mutex<Option<u32>>>,
    emit: Arc<F>,
    mut command: std::process::Command,
) where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
{
    let mut child = match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            if !emit(LogEntry {
                timestamp: String::new(),
                level: "ERROR".into(),
                message: format!("Could not follow Docker logs: {error}. Retrying in two seconds."),
                source: "app".into(),
            }) {
                *stop.lock().unwrap() = true;
            }
            return;
        }
    };
    {
        let cancelled = stop.lock().unwrap();
        if *cancelled {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        *child_pid.lock().unwrap() = Some(child.id());
    }

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return,
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => return,
    };

    // Drain stderr on a second thread. Each stream tracks its own
    // last-source for continuation lines without a recognizable prefix.
    let stop_err = Arc::clone(stop);
    let emit_err = Arc::clone(&emit);
    let pid_err = Arc::clone(child_pid);
    let stderr_thread = std::thread::spawn(move || {
        let mut last_source = String::new();
        for line in BufReader::new(stderr).lines() {
            if *stop_err.lock().unwrap() {
                break;
            }
            if let Ok(line) = line {
                if !emit_err(entry_from_compose_line(&line, &mut last_source)) {
                    *stop_err.lock().unwrap() = true;
                    if let Some(pid) = *pid_err.lock().unwrap() {
                        kill_log_child(pid);
                    }
                    break;
                }
            }
        }
    });

    let mut last_source = String::new();
    for line in BufReader::new(stdout).lines() {
        if *stop.lock().unwrap() {
            break;
        }
        if let Ok(line) = line {
            if !emit(entry_from_compose_line(&line, &mut last_source)) {
                *stop.lock().unwrap() = true;
                break;
            }
        }
    }
    let status = {
        // Stop cannot signal a PID after wait() makes it available for reuse.
        let mut registered = child_pid.lock().unwrap();
        let _ = child.kill();
        let status = child.wait();
        *registered = None;
        status
    };
    let _ = stderr_thread.join();
    if !*stop.lock().unwrap() {
        let message = match status {
            Ok(status) => {
                format!("Docker log follower exited ({status}). Reconnecting in two seconds.")
            }
            Err(error) => {
                format!("Could not reap Docker log follower: {error}. Reconnecting in two seconds.")
            }
        };
        if !emit(LogEntry {
            timestamp: String::new(),
            level: "WARN".into(),
            message,
            source: "app".into(),
        }) {
            *stop.lock().unwrap() = true;
        }
    }
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Spawn a thread that streams logs to the Tauri app from the given sources.
pub fn start_log_stream_for_app(app: tauri::AppHandle, sources: Vec<StreamSource>) {
    use tauri::Manager;
    let state = app.state::<LogStreamState>();
    let emitter = app.clone();
    state.start(sources, move |entry| {
        emitter.emit("node-log", &entry).is_ok()
    });
}

/// Start log streaming without Tauri — sends entries via mpsc channel.
/// Picks sources from the current run mode (Docker: compose-all; Native:
/// node-output.log + compose-all).
pub fn start_log_stream_core(tx: SyncSender<LogEntry>, stop: Arc<Mutex<bool>>) {
    let child_pid = Arc::new(Mutex::new(None));
    let run_mode = crate::settings::load_settings().run_mode;
    let sources = sources_for_run_mode(&run_mode);
    std::thread::spawn(move || {
        stream_multiplexed(sources, stop, child_pid, move |entry| {
            tx.send(entry).is_ok()
        });
    });
}

#[tauri::command]
pub async fn start_log_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, LogStreamState>,
) -> Result<(), String> {
    let run_mode = crate::settings::load_settings().run_mode;
    let sources = sources_for_run_mode(&run_mode);
    let _ = app.emit(
        "node-log",
        serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": "[log-stream] starting multi-source compose logs -f",
            "source": "app",
        }),
    );
    let emitter = app.clone();
    state.start(sources, move |entry| {
        emitter.emit("node-log", &entry).is_ok()
    });
    Ok(())
}

#[tauri::command]
pub async fn stop_log_stream(state: tauri::State<'_, LogStreamState>) -> Result<(), String> {
    state.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_a_session_never_resets_previous_cancellation_or_child_ownership() {
        let state = LogStreamState::new();
        state.start(vec![], |_| true);
        let (old_stop, old_pid) = {
            let session = state.session.lock().unwrap();
            let session = session.as_ref().unwrap();
            (Arc::clone(&session.stop), Arc::clone(&session.child_pid))
        };
        state.start(vec![], |_| true);
        assert!(*old_stop.lock().unwrap());
        {
            let session = state.session.lock().unwrap();
            let session = session.as_ref().unwrap();
            assert!(!*session.stop.lock().unwrap());
            assert!(!Arc::ptr_eq(&session.child_pid, &old_pid));
        }
        state.stop();
        assert!(state.session.lock().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn follower_waits_for_staging_and_retries_a_failed_process() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);
        let stop = Arc::new(Mutex::new(false));
        let child_pid = Arc::new(Mutex::new(None));
        let entries = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&entries);
        stream_compose_with_command(
            &stop,
            &child_pid,
            Arc::new(move |entry: LogEntry| {
                let done = entry.message == "ready";
                recorded.lock().unwrap().push(entry);
                !done
            }),
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    return None;
                }
                let mut command = std::process::Command::new("bash");
                command.args([
                    "-c",
                    if attempt == 1 {
                        "exit 3"
                    } else {
                        "printf 'quip-validator | ready\\n'"
                    },
                ]);
                Some(command)
            },
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(entries
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry.message.contains("exit status: 3")));
        let entries = entries.lock().unwrap();
        assert_eq!(entries.last().unwrap().source, "validator");
        assert_eq!(entries.last().unwrap().message, "ready");
        assert!(child_pid.lock().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn stop_unblocks_a_silent_follower_and_prevents_reconnect() {
        let stop = Arc::new(Mutex::new(false));
        let child_pid = Arc::new(Mutex::new(None));
        let session = LogSession {
            stop: Arc::clone(&stop),
            child_pid: Arc::clone(&child_pid),
        };
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            stream_compose_with_command(&stop, &child_pid, Arc::new(|_| true), || {
                let mut command = std::process::Command::new("sleep");
                command.arg("30");
                Some(command)
            });
            done_tx.send(()).unwrap();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while session.child_pid.lock().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "follower never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        session.stop();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(session.child_pid.lock().unwrap().is_none());
    }

    #[test]
    #[ignore = "Requires Docker, Compose, and a cached alpine:3 image"]
    fn docker_follower_attaches_before_services_exist_and_tracks_later_starts() {
        let project = format!("quip-log-test-{}", std::process::id());
        let directory = std::env::temp_dir().join(&project);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("compose.yml");
        let stop = Arc::new(Mutex::new(false));
        let pid = Arc::new(Mutex::new(None));
        let session = LogSession {
            stop: Arc::clone(&stop),
            child_pid: Arc::clone(&pid),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let file = path.clone();
        let name = project.clone();
        let follower = std::thread::spawn(move || {
            stream_compose_with_command(
                &stop,
                &pid,
                Arc::new(move |entry| tx.send(entry).is_ok()),
                || {
                    if !file.exists() {
                        let _ = waiting_tx.send(());
                        return None;
                    }
                    let mut command = crate::cmd::new("docker");
                    command
                        .args(["compose", "--project-name", &name, "-f"])
                        .arg(&file)
                        .args(["logs", "-f", "--tail", "100"]);
                    Some(command)
                },
            );
        });
        let result = std::panic::catch_unwind(|| {
            waiting_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            std::fs::write(
                &path,
                r#"services:
  quip-validator:
    image: alpine:3
    command: [sh, -c, 'echo validator-ready; exec sleep 60']
  dashboard:
    image: alpine:3
    command: [sh, -c, 'echo dashboard-ready; exec sleep 60']
"#,
            )
            .unwrap();
            for (service, expected, source) in [
                ("quip-validator", "validator-ready", "validator"),
                ("dashboard", "dashboard-ready", "dashboard"),
            ] {
                let output = crate::cmd::new("docker")
                    .args(["compose", "--project-name", &project, "-f"])
                    .arg(&path)
                    .args(["up", "-d", "--pull", "never", service])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                loop {
                    let entry = rx
                        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                        .unwrap();
                    if entry.message.contains(expected) {
                        assert_eq!(entry.source, source);
                        break;
                    }
                }
            }
        });
        session.stop();
        follower.join().unwrap();
        let cleanup = crate::cmd::new("docker")
            .args(["compose", "--project-name", &project, "-f"])
            .arg(&path)
            .args(["down", "--remove-orphans"])
            .output()
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            cleanup.status.success(),
            "{}",
            String::from_utf8_lossy(&cleanup.stderr)
        );
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
    }

    // ── parse_compose_prefix ───────────────────────────────────────────────

    #[test]
    fn compose_prefix_basic() {
        let (svc, rest) = parse_compose_prefix("cpu | hello world").unwrap();
        assert_eq!(svc, "cpu");
        assert_eq!(rest, "hello world");
    }

    #[test]
    fn compose_prefix_padded_service_name() {
        // Compose right-pads short names so the pipe column aligns.
        let (svc, rest) = parse_compose_prefix("cpu              | Starting miner").unwrap();
        assert_eq!(svc, "cpu");
        assert_eq!(rest, "Starting miner");
    }

    #[test]
    fn compose_prefix_hyphenated_service() {
        let (svc, rest) = parse_compose_prefix("quip-validator | peer connected").unwrap();
        assert_eq!(svc, "quip-validator");
        assert_eq!(rest, "peer connected");
    }

    #[test]
    fn compose_prefix_rejects_plain_line() {
        assert!(parse_compose_prefix("just a normal log line").is_none());
        assert!(parse_compose_prefix("ERROR:module:msg").is_none());
    }

    #[test]
    fn compose_prefix_rejects_spaces_in_service() {
        // A " | " mid-message after free text is not a compose prefix.
        assert!(parse_compose_prefix("note: foo | bar").is_none());
    }

    // ── map_compose_service_to_source ──────────────────────────────────────

    #[test]
    fn map_cpu_cuda_to_miner() {
        assert_eq!(map_compose_service_to_source("cpu"), Some("miner"));
        assert_eq!(map_compose_service_to_source("cuda"), Some("miner"));
    }

    #[test]
    fn map_support_services() {
        assert_eq!(
            map_compose_service_to_source("quip-validator"),
            Some("validator")
        );
        assert_eq!(
            map_compose_service_to_source("dashboard"),
            Some("dashboard")
        );
        assert_eq!(map_compose_service_to_source("postgres"), Some("postgres"));
        assert_eq!(map_compose_service_to_source("caddy"), Some("caddy"));
    }

    /// Compose prefixes lines with the *container* name, not the YAML service
    /// key, whenever `container_name:` is set — which it is for every service
    /// in our stack. These are the exact prefixes `docker compose logs` emits
    /// against a running stack, so the container-name aliases are load-bearing,
    /// not defensive. Missing `quip-cpu`/`quip-cuda` silently mistags every
    /// miner line with whatever service logged before it.
    #[test]
    fn map_container_names_as_emitted_by_compose() {
        for (prefix, want) in [
            ("quip-cpu", "miner"),
            ("quip-cuda", "miner"),
            ("quip-validator", "validator"),
            ("quip-dashboard", "dashboard"),
            ("quip-postgres", "postgres"),
            ("quip-caddy", "caddy"),
        ] {
            assert_eq!(
                map_compose_service_to_source(prefix),
                Some(want),
                "container prefix {prefix} should map to {want}"
            );
        }
    }

    #[test]
    fn map_unknown_service_is_none() {
        assert_eq!(map_compose_service_to_source("faucet"), None);
        assert_eq!(map_compose_service_to_source("unknown"), None);
    }

    // ── entry_from_compose_line (prefix + last-source carry) ───────────────

    #[test]
    fn compose_line_tags_known_service_and_strips_prefix() {
        let mut last = String::new();
        let entry = entry_from_compose_line("dashboard | ready on :3000", &mut last);
        assert_eq!(entry.source, "dashboard");
        assert_eq!(entry.message, "ready on :3000");
        assert_eq!(last, "dashboard");
    }

    #[test]
    fn compose_line_without_prefix_keeps_last_source() {
        let mut last = "validator".to_string();
        let entry = entry_from_compose_line("continuation without prefix", &mut last);
        assert_eq!(entry.source, "validator");
        assert_eq!(entry.message, "continuation without prefix");
    }

    #[test]
    fn compose_line_without_prefix_or_history_defaults_to_app() {
        let mut last = String::new();
        let entry = entry_from_compose_line("orphan line", &mut last);
        assert_eq!(entry.source, "app");
        assert_eq!(entry.message, "orphan line");
    }

    #[test]
    fn compose_line_unknown_service_prefix_keeps_last_source() {
        let mut last = "miner".to_string();
        let entry = entry_from_compose_line("faucet | drip", &mut last);
        assert_eq!(entry.source, "miner");
        // Full line kept when the service is not in our map.
        assert_eq!(entry.message, "faucet | drip");
        assert_eq!(last, "miner");
    }

    // ── parse_log_line ─────────────────────────────────────────────────────

    #[test]
    fn parse_plain_text() {
        let e = parse_log_line("hello world");
        assert_eq!(e.level, "INFO");
        assert_eq!(e.message, "hello world");
        assert!(e.timestamp.is_empty());
        assert_eq!(e.source, "app");
    }

    /// A real 502 as Caddy's console encoder writes it, with the Caddyfile's
    /// filter block already applied. Before this branch existed the whole line
    /// fell through to the plain-text case and rendered as INFO, so proxy
    /// failures were invisible to the pane's level colouring.
    #[test]
    fn parse_caddy_console_error() {
        let e = parse_log_line(
            "2026/08/16 05:22:02.881\tERROR\thttp.log.error\t\
             dial tcp 192.168.107.3:9944: connect: connection refused\t\
             {\"request\":{\"method\":\"GET\",\"host\":\"quip-caddy:8088\",\
             \"uri\":\"/rpc\"},\"status\":502}",
        );
        assert_eq!(e.level, "ERROR");
        assert_eq!(e.timestamp, "2026/08/16 05:22:02.881");
        assert!(e
            .message
            .starts_with("http.log.error dial tcp 192.168.107.3:9944"));
        assert!(e.message.ends_with("\"status\":502}"));
    }

    #[test]
    fn parse_caddy_console_levels() {
        for (raw, want) in [
            ("INFO", "INFO"),
            ("WARN", "WARN"),
            ("DEBUG", "DEBUG"),
            ("ERROR", "ERROR"),
            ("PANIC", "ERROR"),
            ("FATAL", "ERROR"),
        ] {
            let e = parse_log_line(&format!(
                "2026/08/16 05:22:02.881\t{raw}\thttp\tserver running"
            ));
            assert_eq!(e.level, want, "level {raw} should map to {want}");
            assert_eq!(e.message, "http server running");
        }
    }

    /// Caddy emits its first two startup lines (`using config from file`,
    /// `adapted config to JSON`) before the global `log` block takes effect, so
    /// they stay JSON. They must still pass through rather than being dropped.
    #[test]
    fn parse_caddy_pre_config_json_falls_through_to_plain_text() {
        let raw = "{\"level\":\"info\",\"ts\":1786887944.88,\"msg\":\"using config from file\"}";
        let e = parse_log_line(raw);
        assert_eq!(e.level, "INFO");
        assert_eq!(e.message, raw);
    }

    /// A tab in a miner line must not be mistaken for the console encoder.
    #[test]
    fn parse_tabbed_non_caddy_line_is_not_treated_as_console() {
        let e = parse_log_line("solution\t10448\tsubmitted");
        assert_eq!(e.level, "INFO");
        assert_eq!(e.message, "solution\t10448\tsubmitted");
    }

    #[test]
    fn parse_python_level_prefix() {
        let e = parse_log_line("ERROR:module:something broke");
        assert_eq!(e.level, "ERROR");
        assert_eq!(e.message, "module:something broke");

        let e = parse_log_line("WARNING:mod:careful");
        assert_eq!(e.level, "WARN");
        assert_eq!(e.message, "mod:careful");

        let e = parse_log_line("INFO:mod:ok");
        assert_eq!(e.level, "INFO");

        let e = parse_log_line("DEBUG:mod:detail");
        assert_eq!(e.level, "DEBUG");
    }

    #[test]
    fn parse_structured_quip_format_single_bracket() {
        // [file.py:123] 2026-01-01T12:00:00+00:00 INFO - hello
        let e = parse_log_line("[file.py:123] 2026-01-01T12:00:00+00:00 INFO - hello");
        assert_eq!(e.timestamp, "2026-01-01T12:00:00+00:00");
        assert_eq!(e.level, "INFO");
        assert_eq!(e.message, "hello");
        assert_eq!(e.source, "app");
    }

    #[test]
    fn parse_structured_quip_format_double_bracket() {
        // [file.py:123][node] 2026-01-01T12:00:00+00:00 ERROR - boom
        let e = parse_log_line("[file.py:123][node] 2026-01-01T12:00:00+00:00 ERROR - boom");
        assert_eq!(e.timestamp, "2026-01-01T12:00:00+00:00");
        assert_eq!(e.level, "ERROR");
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn parse_structured_warn_and_debug() {
        let e = parse_log_line("[a.py:1] 2026-01-01T00:00:00Z WARN - careful");
        assert_eq!(e.level, "WARN");
        assert_eq!(e.message, "careful");

        let e = parse_log_line("[a.py:1] 2026-01-01T00:00:00Z DEBUG - detail");
        assert_eq!(e.level, "DEBUG");
        assert_eq!(e.message, "detail");
    }

    #[test]
    fn with_source_overrides_default() {
        let e = parse_log_line("x").with_source("miner");
        assert_eq!(e.source, "miner");
        assert_eq!(e.message, "x");
    }
}
