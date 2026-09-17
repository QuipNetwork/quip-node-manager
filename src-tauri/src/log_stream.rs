// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
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

/// Shared state for the log streamer.
///
/// Every source is a file tailer that checks `stop` between reads, so
/// cancellation no longer needs a child process to kill: setting the flag ends
/// each tailer within one poll interval. The stack's own collector does the
/// merging now, so there is no `docker compose logs -f` child to manage.
pub struct LogStreamState {
    session: Mutex<Option<LogSession>>,
}

struct LogSession {
    stop: Arc<Mutex<bool>>,
}

impl LogSession {
    fn stop(&self) {
        *self.stop.lock().unwrap() = true;
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
        *current = Some(LogSession {
            stop: Arc::clone(&stop),
        });
        std::thread::spawn(move || {
            let cancelled = Arc::clone(&stop);
            stream_multiplexed(sources, stop, move |entry| {
                !*cancelled.lock().unwrap() && emit(entry)
            });
        });
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

/// Split one line of the collector's merged log into its parts.
///
/// `syslog-ng/syslog-ng.conf` writes `${ISODATE} ${PROGRAM} ${MESSAGE}`, where
/// PROGRAM is the container name Docker's syslog driver sends as `tag`. Verified
/// against a live collector:
///
/// ```text
/// 2026-09-12T21:50:48+00:00 quip-validator block imported
/// ```
///
/// Returns `(timestamp, program, message)`. A message can be empty, so a line
/// with only two fields still parses.
pub fn parse_merged_line(line: &str) -> Option<(&str, &str, &str)> {
    let (timestamp, rest) = line.split_once(' ')?;
    // ISODATE always starts with the year, which is what separates a merged
    // line from a raw one that merely contains spaces.
    if !timestamp.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (program, message) = rest.split_once(' ').unwrap_or((rest, ""));
    if program.is_empty() {
        return None;
    }
    // Container names are a single token: letters, digits, hyphen, underscore.
    if !program
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some((timestamp, program, message))
}

/// Map a compose service key or container tag to a UI `source` tag.
/// `cpu`/`cuda` both map to `miner`. Unknown names return `None`.
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

/// Turn one merged-log line into a tagged `LogEntry`.
///
/// The collector tags every line individually, so unlike the old compose
/// stream there are no untagged continuation lines to carry a previous source
/// onto — this needs no state between lines.
///
/// The collector's own ISODATE is used only when the message carries no
/// timestamp of its own, so a structured miner or Caddy line keeps the time it
/// reported rather than the time the collector received it.
fn entry_from_merged_line(line: &str) -> LogEntry {
    let Some((timestamp, program, message)) = parse_merged_line(line) else {
        return parse_log_line(line);
    };
    // `parse_log_line` already defaults the source to `app`, so an unmapped
    // container simply keeps that rather than duplicating the default here.
    let mut entry = parse_log_line(message);
    if let Some(source) = map_compose_service_to_source(program) {
        entry = entry.with_source(source);
    }
    if entry.timestamp.is_empty() {
        entry.timestamp = timestamp.to_string();
    }
    entry
}

// ─── File tailing ────────────────────────────────────────────────────────────

/// Identity of whatever file currently sits at `path`.
///
/// Length alone cannot detect the collector's rename-then-SIGHUP rotation: the
/// replacement file can already be longer than our offset in the renamed one by
/// the time we look, and we would then keep reading the renamed inode forever
/// while the pane sits silent. Comparing identity catches the swap whatever the
/// two sizes are.
///
/// Windows has no cheap stable equivalent through `std`, so this reports `None`
/// there and the length check below remains the only signal.
#[cfg(unix)]
fn file_identity(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &std::path::Path) -> Option<(u64, u64)> {
    None
}

/// Tail a log file: backfill last 200 lines, then follow new output.
///
/// Reopens when the file at `path` is no longer the one we hold, or when it is
/// shorter than our offset. That covers the collector's rotation and a plain
/// truncation.
///
/// Waits for the file instead of giving up when it is missing. The merged log
/// does not exist until the collector receives its first line, and streaming
/// starts before the stack is up.
fn tail_file<F, P>(path: &std::path::Path, stop: &Mutex<bool>, to_entry: &P, emit: &F)
where
    F: Fn(LogEntry) -> bool,
    P: Fn(&str) -> LogEntry,
{
    let open = || std::fs::File::open(path);
    let mut file = loop {
        if *stop.lock().unwrap() {
            return;
        }
        match open() {
            Ok(f) => break f,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    };

    let mut existing = String::new();
    let _ = file.read_to_string(&mut existing);
    let lines: Vec<&str> = existing.lines().collect();
    let start = lines.len().saturating_sub(200);
    for line in &lines[start..] {
        if *stop.lock().unwrap() {
            return;
        }
        if !emit(to_entry(line)) {
            return;
        }
    }

    let mut pos = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let mut identity = file_identity(path);
    let mut buf = String::new();
    loop {
        if *stop.lock().unwrap() {
            break;
        }

        let reopened = match std::fs::metadata(path) {
            Ok(meta) => meta.len() < pos || file_identity(path) != identity,
            Err(_) => true,
        };
        if reopened {
            if let Ok(f) = open() {
                file = f;
                pos = 0;
                identity = file_identity(path);
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
                    if !emit(to_entry(line)) {
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
    /// Tail the stack's merged log, written by the quip-syslog collector.
    /// Every line carries the emitting container's name, which is parsed into
    /// `LogEntry.source`.
    MergedLog { path: PathBuf },
    /// Tail a host file, tagging every line with a fixed `source`.
    File { path: PathBuf, source: &'static str },
}

/// Sources for the given run mode.
///
/// - Docker: the collector's merged log, which already covers every container.
/// - Native: host `node-output.log` for the miner, which runs outside Docker
///   and so never reaches the collector, plus the merged log for the
///   containerized support services.
pub fn sources_for_run_mode(run_mode: &crate::settings::RunMode) -> Vec<StreamSource> {
    let merged = StreamSource::MergedLog {
        path: crate::stack_assets::merged_log_file(),
    };
    match run_mode {
        crate::settings::RunMode::Docker => vec![merged],
        crate::settings::RunMode::Native => vec![
            StreamSource::File {
                path: crate::settings::data_dir().join("node-output.log"),
                source: "miner",
            },
            merged,
        ],
    }
}

/// Fan-in every source concurrently into a single tagged stream.
fn stream_multiplexed<F>(sources: Vec<StreamSource>, stop: Arc<Mutex<bool>>, emit: F)
where
    F: Fn(LogEntry) -> bool + Send + Sync + 'static,
{
    let emit = Arc::new(emit);
    let mut handles = Vec::with_capacity(sources.len());

    for source in sources {
        let stop = Arc::clone(&stop);
        let emit = Arc::clone(&emit);
        handles.push(std::thread::spawn(move || match source {
            StreamSource::MergedLog { path } => {
                tail_file(&path, &stop, &entry_from_merged_line, &|entry| emit(entry));
            }
            StreamSource::File { path, source } => {
                tail_file(&path, &stop, &parse_log_line, &|entry| {
                    emit(entry.with_source(source))
                });
            }
        }));
    }

    for handle in handles {
        let _ = handle.join();
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
/// Picks sources from the current run mode (Docker: the merged log; Native:
/// node-output.log plus the merged log).
pub fn start_log_stream_core(tx: SyncSender<LogEntry>, stop: Arc<Mutex<bool>>) {
    let run_mode = crate::settings::load_settings().run_mode;
    let sources = sources_for_run_mode(&run_mode);
    std::thread::spawn(move || {
        stream_multiplexed(sources, stop, move |entry| tx.send(entry).is_ok());
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
            "message": "[log-stream] tailing the merged stack log",
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
    fn replacing_a_session_cancels_the_previous_one() {
        let state = LogStreamState::new();
        state.start(vec![], |_| true);
        let old_stop = {
            let session = state.session.lock().unwrap();
            Arc::clone(&session.as_ref().unwrap().stop)
        };
        state.start(vec![], |_| true);
        // The replaced session stays cancelled, and the new one starts live —
        // the flag is now the only thing that stops a tailer, so a shared or
        // reset flag would leave the old tailers running forever.
        assert!(*old_stop.lock().unwrap());
        {
            let session = state.session.lock().unwrap();
            let session = session.as_ref().unwrap();
            assert!(!*session.stop.lock().unwrap());
            assert!(!Arc::ptr_eq(&session.stop, &old_stop));
        }
        state.stop();
        assert!(state.session.lock().unwrap().is_none());
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

    // ── merged stack log ───────────────────────────────────────────────────

    /// Captured verbatim from a live collector (syslog-ng 4.11 with the
    /// vendored config, fed through Docker's syslog driver with
    /// `tag: "{{.Name}}"`), so the parser is pinned to observed output rather
    /// than to a reading of the template.
    const MERGED_VALIDATOR: &str = "2026-09-12T21:50:48+00:00 quip-validator block imported";
    const MERGED_MINER: &str = "2026-09-12T21:50:49+00:00 quip-cpu \
        [miner.py:42] 2026-01-01T12:00:00+00:00 INFO - attempt submitted";

    #[test]
    fn merged_line_splits_timestamp_program_and_message() {
        let (timestamp, program, message) = parse_merged_line(MERGED_VALIDATOR).unwrap();
        assert_eq!(timestamp, "2026-09-12T21:50:48+00:00");
        assert_eq!(program, "quip-validator");
        assert_eq!(message, "block imported");
    }

    #[test]
    fn merged_line_tags_source_from_the_container_name() {
        let entry = entry_from_merged_line(MERGED_VALIDATOR);
        assert_eq!(entry.source, "validator");
        assert_eq!(entry.message, "block imported");
        // The message carries no time of its own, so the collector's is used.
        assert_eq!(entry.timestamp, "2026-09-12T21:50:48+00:00");
    }

    /// A structured miner line keeps the time the miner reported, not the time
    /// the collector received it — those differ by the queueing delay, and the
    /// miner's is the one that lines up with the rest of its output.
    #[test]
    fn merged_line_prefers_the_message_timestamp() {
        let entry = entry_from_merged_line(MERGED_MINER);
        assert_eq!(entry.source, "miner");
        assert_eq!(entry.level, "INFO");
        assert_eq!(entry.message, "attempt submitted");
        assert_eq!(entry.timestamp, "2026-01-01T12:00:00+00:00");
    }

    #[test]
    fn merged_line_with_an_empty_message_still_parses() {
        let (_, program, message) =
            parse_merged_line("2026-09-12T21:50:48+00:00 quip-cpu").unwrap();
        assert_eq!(program, "quip-cpu");
        assert!(message.is_empty());
    }

    /// Without the leading-digit check, `ERROR: could not reach host` would
    /// parse as timestamp `ERROR:` and program `could`, mistagging the line.
    #[test]
    fn non_merged_lines_fall_through_to_plain_parsing() {
        assert!(parse_merged_line("just a normal log line").is_none());
        assert!(parse_merged_line("ERROR: could not reach host").is_none());
        let entry = entry_from_merged_line("just a normal log line");
        assert_eq!(entry.source, "app");
        assert_eq!(entry.message, "just a normal log line");
    }

    /// The faucet writes to the merged file but has no source mapping. Those
    /// lines land on `app` rather than inheriting whichever service logged
    /// last, which is what the old compose stream did.
    #[test]
    fn unmapped_container_falls_back_to_app() {
        let entry = entry_from_merged_line("2026-09-12T21:50:48+00:00 quip-faucet drip sent");
        assert_eq!(entry.source, "app");
        assert_eq!(entry.message, "drip sent");
    }

    /// The merged file preserves the message, so a Caddy line keeps the tabs
    /// `parse_caddy_console_line` splits on and a 502 reads as ERROR.
    ///
    /// Captured from a live collector running the narrowed sanitize call
    /// (`--no-ctrl-chars --invalid-chars '\n\r'`, nodes.quip.network!28).
    /// Before that, the bare `$(sanitize ${MESSAGE})` rewrote `/` and every
    /// control character to `_`: the same line arrived as
    /// `2026_08_16 05:22:02.881_ERROR_...` and fell through to plain text as
    /// INFO, and every URL in every service's output was rewritten the same
    /// way.
    ///
    /// This covers the parsing side only, against a captured line. What pins
    /// the upstream template is
    /// `stack_assets::tests::collector_template_keeps_slashes_and_tabs`, which
    /// asserts on the embedded config itself.
    #[test]
    fn merged_lines_keep_caddy_levels_and_urls() {
        let captured = "2026-09-13T10:07:17+00:00 quip-caddy \
            2026/08/16 05:22:02.881\tERROR\thttp.log.error\t\
            dial tcp 192.168.107.3:9944: connect: connection refused";
        let entry = entry_from_merged_line(captured);
        assert_eq!(entry.source, "caddy");
        assert_eq!(entry.level, "ERROR");
        // The Caddy branch sets the time from the message, so the collector's
        // receive time is not substituted for it.
        assert_eq!(entry.timestamp, "2026/08/16 05:22:02.881");
        assert!(entry.message.contains("dial tcp 192.168.107.3:9944"));

        // Slashes survive in ordinary miner output too.
        let miner = entry_from_merged_line(
            "2026-09-13T10:07:17+00:00 quip-cpu validators=ws://quip-validator:9944 dir=/data/logs",
        );
        assert_eq!(miner.source, "miner");
        assert_eq!(
            miner.message,
            "validators=ws://quip-validator:9944 dir=/data/logs"
        );
    }

    /// The tailer now waits for a file that does not exist yet, because the
    /// merged log is not created until the collector's first line. Stop has to
    /// break that wait, or the thread outlives the session.
    #[cfg(unix)]
    #[test]
    fn stop_unblocks_a_tailer_waiting_for_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("quip-tail-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("never-created.log");
        let stop = Arc::new(Mutex::new(false));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = Arc::clone(&stop);
        std::thread::spawn(move || {
            tail_file(&path, &waiter, &parse_log_line, &|_| true);
            let _ = done_tx.send(());
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        *stop.lock().unwrap() = true;
        done_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("tailer ignored stop while waiting for the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The collector rotates by renaming the live file and signalling
    /// syslog-ng to recreate the path. The tailer must follow the path to the
    /// new inode; holding the renamed one means the pane goes quiet after the
    /// first 10 MB and never recovers.
    #[cfg(unix)]
    #[test]
    fn tailer_follows_the_path_across_a_rotation() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("quip-tail-rot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quip-node.log");
        std::fs::write(&path, "2026-09-12T00:00:00+00:00 quip-cpu before\n").unwrap();

        let stop = Arc::new(Mutex::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let follow_path = path.clone();
        let follow_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            tail_file(
                &follow_path,
                &follow_stop,
                &entry_from_merged_line,
                &|entry| tx.send(entry).is_ok(),
            );
        });
        let first = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("no backfill");
        assert_eq!(first.message, "before");

        std::fs::rename(&path, dir.join("quip-node.log.1")).unwrap();
        let mut recreated = std::fs::File::create(&path).unwrap();
        writeln!(recreated, "2026-09-12T00:00:01+00:00 quip-validator after").unwrap();
        recreated.flush().unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let entry = rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("tailer never picked up the rotated file");
            if entry.message == "after" {
                assert_eq!(entry.source, "validator");
                break;
            }
        }
        *stop.lock().unwrap() = true;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
