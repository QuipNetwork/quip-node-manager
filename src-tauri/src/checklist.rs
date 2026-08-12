// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, Semaphore};

use crate::settings::{data_dir, RunMode};

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    Idle,
    Running,
    Pass,
    Warn,
    Fail,
    Skip,
}

/// What the UI should do if the user clicks "Fix" on this item.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "arg")]
pub enum FixKind {
    InstallDocker,
    GenerateSecret,
}

#[derive(Serialize, Clone, Debug)]
pub struct CheckItem {
    pub id: String,
    pub state: CheckState,
    pub label: String,
    pub detail: Option<String>,
    pub required: bool,
    pub fixable: Option<FixKind>,
    pub updated_at_ms: u64,
}

impl CheckItem {
    fn new(id: &str, label: &str, required: bool, fixable: Option<FixKind>) -> Self {
        CheckItem {
            id: id.to_string(),
            state: CheckState::Idle,
            label: label.to_string(),
            detail: None,
            required,
            fixable,
            updated_at_ms: now_ms(),
        }
    }

    fn with_state(mut self, state: CheckState) -> Self {
        self.state = state;
        self.updated_at_ms = now_ms();
        self
    }

    fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── Shared state ─────────────────────────────────────────────────────────────

/// Tauri-managed checklist state.
///
/// Holds the last-known `CheckItem` for every id, plus synchronisation to keep
/// concurrent rechecks from stepping on each other:
/// - Per-id `AsyncMutex<()>` prevents re-entrant rechecks of the same check.
/// - A semaphore caps concurrent checks during a global Recheck-All.
pub struct ChecklistState {
    pub cache: Arc<AsyncMutex<HashMap<String, CheckItem>>>,
    pub locks: Arc<std::sync::Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    pub sem: Arc<Semaphore>,
}

impl ChecklistState {
    pub fn new() -> Self {
        ChecklistState {
            cache: Arc::new(AsyncMutex::new(HashMap::new())),
            locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            sem: Arc::new(Semaphore::new(3)),
        }
    }

    fn lock_for(&self, id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

impl Default for ChecklistState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Per-check context ────────────────────────────────────────────────────────

/// Shared read-only input to every check, plus memoised network lookups.
pub struct CheckCtx {
    pub run_mode: RunMode,
    pub image_tag: crate::settings::ImageTag,
    /// Public Caddy/API host port. In v0.2 this is HTTP/WebSocket over TCP.
    pub port: u16,
    /// Host-exposed validator libp2p port. The container still binds 30333.
    pub validator_port: u16,
    pub public_host: String,
    /// `true` iff the user has a [dwave] block in their NodeConfig (i.e.
    /// they're intending QPU mining). Controls visibility of `dwave-key`.
    pub has_dwave_config: bool,
    /// `true` iff the [dwave] block has a non-empty token.
    pub dwave_token_set: bool,
    /// AppHandle for emitting diagnostic log lines (e.g. raw responses from
    /// check.quip.network). `None` when the ctx is constructed from a
    /// non-Tauri caller like the TUI — probes then run silently.
    pub app: Option<AppHandle>,
    public_ip: OnceCell<Option<String>>,
}

impl CheckCtx {
    fn from_settings(app: Option<AppHandle>) -> Self {
        let settings = crate::settings::load_settings();
        let has_dwave_config = settings.node_config.dwave_config.is_some();
        let dwave_token_set = settings
            .node_config
            .dwave_config
            .as_ref()
            .map(|d| !d.token.trim().is_empty())
            .unwrap_or(false);
        CheckCtx {
            run_mode: settings.run_mode,
            image_tag: settings.image_tag,
            port: settings.node_config.port,
            validator_port: settings.node_config.validator_port,
            public_host: settings.node_config.public_host,
            has_dwave_config,
            dwave_token_set,
            app,
            public_ip: OnceCell::new(),
        }
    }

    /// Emit a diagnostic log line to the `node-log` event so users see the
    /// probe details in the app's console drawer. No-op when `app` is None
    /// (TUI / tests).
    fn log_probe(&self, level: &str, message: impl Into<String>) {
        let Some(app) = &self.app else { return };
        let entry = serde_json::json!({
            "timestamp": "",
            "level": level,
            "message": format!("[probe] {}", message.into()),
            "source": "app",
        });
        let _ = app.emit("node-log", entry);
    }

    /// Memoised public-IP fetch. First caller pays the network cost;
    /// subsequent callers in the same recheck batch reuse the result.
    async fn public_ip(&self) -> Option<String> {
        self.public_ip.get_or_init(fetch_public_ip).await.clone()
    }
}

// ─── Low-level probes ─────────────────────────────────────────────────────────

fn check_docker() -> bool {
    crate::cmd::new("docker")
        .args(["info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn decode_wsl_output(bytes: &[u8]) -> String {
    // wsl.exe emits UTF-16LE with BOM on Windows; fall back to UTF-8 otherwise.
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    }
}

#[cfg(target_os = "windows")]
fn check_wsl() -> (bool, String) {
    if let Ok(out) = crate::cmd::new("wsl")
        .args(["--list", "--verbose"])
        .output()
    {
        if out.status.success() {
            let text = decode_wsl_output(&out.stdout);
            let has_distro = text.lines().skip(1).any(|l| !l.trim().is_empty());
            if has_distro {
                return (true, "WSL installed with distro".into());
            }
            return (
                false,
                "WSL installed but no distro \u{2014} run: wsl --install -d Ubuntu".into(),
            );
        }
    }
    if let Ok(out) = crate::cmd::new("wsl").args(["--version"]).output() {
        if out.status.success() {
            return (true, "WSL detected (distro list unavailable)".into());
        }
    }
    if let Ok(out) = crate::cmd::new("wsl").args(["--status"]).output() {
        if out.status.success() {
            return (true, "WSL detected (distro list unavailable)".into());
        }
    }
    (
        false,
        "WSL not detected (Docker Desktop will confirm) \u{2014} run: wsl --install".into(),
    )
}

fn check_secret_exists() -> bool {
    data_dir().join("node-secret.json").exists()
}

const CHECK_SERVICE: &str = "https://check.quip.network";

fn make_client(timeout_secs: u64) -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .ok()
}

/// Public IP as check.quip.network sees us, falling back to ipify when the
/// service is down.
///
/// This is the same value the start paths render into the miner's
/// `public_host`, so the `ip` checklist row and the address the node actually
/// advertises cannot disagree.
pub(crate) async fn fetch_public_ip() -> Option<String> {
    if let Some(ip) = fetch_ip_check_service().await {
        return Some(ip);
    }
    fetch_ip_ipify().await
}

async fn fetch_ip_check_service() -> Option<String> {
    let client = make_client(5)?;
    let resp = client
        .get(format!("{}/ip", CHECK_SERVICE))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().await.ok()?;
    let ip = json["ip"].as_str()?.trim().to_string();
    if ip.is_empty() {
        None
    } else {
        Some(ip)
    }
}

async fn fetch_ip_ipify() -> Option<String> {
    let client = make_client(10)?;
    let resp = client.get("https://api.ipify.org").send().await.ok()?;
    let text = resp.text().await.ok()?;
    let ip = text.trim().to_string();
    if ip.is_empty() {
        None
    } else {
        Some(ip)
    }
}

/// Outcome of `probe_port_forwarding`. v0.2 uses `checkport` TCP probes for
/// both public API and validator reachability checks. If the port is free
/// locally we temporarily bind it so the external probe has something to hit;
/// if it is already bound we probe the service currently holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortProbeResult {
    /// The port was already bound locally and the external TCP probe reached
    /// this host.
    Verified,
    /// The port was free locally, and the external TCP probe reached our
    /// temporary listener.
    ForwardReady,
    /// TCP forwarding did not reach this host.
    Unreachable,
    /// check.quip.network itself was unreachable or errored (network error,
    /// HTTP 5xx, non-JSON body). We can't confirm external reachability, so
    /// we surface a warning rather than a misleading green check.
    Unverified,
    /// check.quip.network rate-limited the request. We couldn't verify
    /// reachability, so this is a warning (not a green check) — the user can
    /// recheck once the cool-down expires. The retry time is preserved for UX.
    RateLimited {
        /// Seconds until the ban is expected to lift, per the service's
        /// `retry_after_seconds` field.
        retry_after_secs: u64,
        /// Which endpoint got limited.
        endpoint: &'static str,
    },
}

impl PortProbeResult {
    /// True only when check.quip.network positively confirmed the port was
    /// reachable. A rate-limit, service error, or any other non-confirmation
    /// is *not* a pass — we never claim reachability we didn't measure.
    pub fn is_externally_reachable(self) -> bool {
        matches!(self, Self::Verified | Self::ForwardReady)
    }
}

/// Result of a single call to `/checkport`. Distinguishes "no response from
/// the host at all" from "host responded, just not as expected" and from
/// service-side errors we shouldn't blame the user for.
enum ProbeOutcome {
    /// `/checkport` reported `reachable:true`: the external TCP connect got a
    /// SYN-ACK, so the router forward works and something is listening. The
    /// only passing case.
    HostResponded,
    /// `/checkport` reported `reachable:false`: the external TCP connect could
    /// not be established — the SYN timed out, or the host/router replied with
    /// a RST ("connection refused"). Either way the port is not reachable.
    Unreachable,
    /// HTTP 429 with `retry_after_seconds`. We can't verify right now, so
    /// we surface a warning but preserve the retry time for the UX.
    RateLimited(u64),
    /// Any other failure — HTTP 5xx, non-JSON body, client-side network
    /// error. Treated as lenient-pass (not the user's fault).
    ServiceError,
}

/// TCP forwarding check. One `/checkport` probe runs:
///
///   - If something is already bound to `port` locally, probe that service.
///   - If the port is free, hold a temporary listener for the probe.
///
/// GUI-facing entry point. `ctx.app` is used to emit the full check.quip.network
/// request URL, HTTP status, and response body into `node-log` so users can
/// copy/paste the raw output when asking for support.
/// How long a check.quip.network port-probe result stays fresh. The service
/// rate-limits aggressively, so we reuse the last result for any re-run that
/// isn't an explicit user Retry (mode/TLS toggles, auto-rechecks, reloads).
const PORT_PROBE_TTL: Duration = Duration::from_secs(5 * 60);

/// Per-port cache of the last probe result + when it was taken. Process-global
/// because probes run from per-recheck `CheckCtx` instances that don't persist.
fn port_probe_cache() -> &'static Mutex<HashMap<u16, (PortProbeResult, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<u16, (PortProbeResult, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_port_probe(port: u16) -> Option<PortProbeResult> {
    let cache = port_probe_cache().lock().ok()?;
    let (result, taken_at) = cache.get(&port)?;
    (taken_at.elapsed() < PORT_PROBE_TTL).then_some(*result)
}

fn store_port_probe(port: u16, result: PortProbeResult) {
    if let Ok(mut cache) = port_probe_cache().lock() {
        let _ = cache.insert(port, (result, Instant::now()));
    }
}

/// Drop all cached probe results so the next probe hits check.quip.network.
/// Called on a user-initiated Retry (not on auto/background rechecks).
pub(crate) fn clear_port_probe_cache() {
    if let Ok(mut cache) = port_probe_cache().lock() {
        cache.clear();
    }
}

/// Cached front door for the port probe: serve a <5-minute-old result if we
/// have one, otherwise probe and store. A user Retry clears the cache first
/// (see `run_recheck`), so it always re-probes.
async fn probe_port_forwarding_with_ctx(ctx: &CheckCtx, port: u16) -> PortProbeResult {
    if let Some(cached) = cached_port_probe(port) {
        ctx.log_probe(
            "INFO",
            format!("port {port} \u{2014} using cached check.quip.network result (<5m old)"),
        );
        return cached;
    }
    let result = probe_port_forwarding_uncached(ctx, port).await;
    store_port_probe(port, result);
    result
}

async fn probe_port_forwarding_uncached(ctx: &CheckCtx, port: u16) -> PortProbeResult {
    use tokio::net::TcpListener;

    match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
        Err(e) => {
            ctx.log_probe(
                "INFO",
                format!(
                    "port {} in use locally ({}) \u{2014} using TCP /checkport probe",
                    port, e
                ),
            );
            match probe_external_tcp(ctx, port).await {
                ProbeOutcome::HostResponded => PortProbeResult::Verified,
                ProbeOutcome::Unreachable => PortProbeResult::Unreachable,
                ProbeOutcome::RateLimited(retry) => PortProbeResult::RateLimited {
                    retry_after_secs: retry,
                    endpoint: "checkport",
                },
                // check.quip.network is down/misbehaving — we can't confirm
                // reachability, so report it as unverified (a warning) rather
                // than a green check we haven't earned.
                ProbeOutcome::ServiceError => PortProbeResult::Unverified,
            }
        }
        Ok(listener) => {
            ctx.log_probe(
                "INFO",
                format!(
                    "port {} is free locally \u{2014} holding temp listener, using TCP probe",
                    port
                ),
            );
            let accept_task = tokio::spawn(async move {
                loop {
                    if listener.accept().await.is_err() {
                        break;
                    }
                }
            });
            let outcome = probe_external_tcp(ctx, port).await;
            accept_task.abort();
            match outcome {
                ProbeOutcome::HostResponded => PortProbeResult::ForwardReady,
                ProbeOutcome::Unreachable => PortProbeResult::Unreachable,
                ProbeOutcome::RateLimited(retry) => PortProbeResult::RateLimited {
                    retry_after_secs: retry,
                    endpoint: "checkport",
                },
                // check.quip.network isn't cooperating — no connectivity
                // signal either way, so report unverified (a warning).
                ProbeOutcome::ServiceError => PortProbeResult::Unverified,
            }
        }
    }
}

/// Plain wrapper for callers without a `CheckCtx` (TUI). Runs silently.
pub async fn probe_port_forwarding(port: u16) -> PortProbeResult {
    let ctx = CheckCtx::from_settings(None);
    probe_port_forwarding_with_ctx(&ctx, port).await
}

async fn probe_external_tcp(ctx: &CheckCtx, port: u16) -> ProbeOutcome {
    fetch_probe_json(ctx, "checkport", port, "reachable", 10).await
}

/// Shared HTTP fetcher for check.quip.network. Every step (URL, network
/// error, HTTP status, response body) is emitted to `node-log` via
/// `ctx.log_probe` so users can see service-side errors without having to
/// reproduce the request by hand.
///
/// Classifies the result into `ProbeOutcome` — callers use that to
/// decide which `PortProbeResult` variant to surface.
async fn fetch_probe_json(
    ctx: &CheckCtx,
    endpoint: &str,
    port: u16,
    success_key: &str,
    timeout_secs: u64,
) -> ProbeOutcome {
    let url = format!("{}/{}?port={}", CHECK_SERVICE, endpoint, port);
    ctx.log_probe("INFO", format!("GET {}", url));

    let Some(client) = make_client(timeout_secs) else {
        ctx.log_probe(
            "ERROR",
            format!("{}: failed to build HTTP client", endpoint),
        );
        return ProbeOutcome::ServiceError;
    };

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            ctx.log_probe(
                "ERROR",
                format!(
                    "{}: network error talking to check.quip.network: {}",
                    endpoint, e
                ),
            );
            return ProbeOutcome::ServiceError;
        }
    };
    let status = resp.status();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("<body read error: {}>", e));
    let body_for_log = if body.len() > 1024 {
        format!(
            "{}\u{2026}(truncated, {} bytes total)",
            &body[..1024],
            body.len()
        )
    } else {
        body.clone()
    };
    ctx.log_probe(
        if status.is_success() { "INFO" } else { "ERROR" },
        format!(
            "{} \u{2192} HTTP {} {}",
            endpoint,
            status.as_u16(),
            body_for_log
        ),
    );

    if status.as_u16() == 429 {
        let retry = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|j| j.get("retry_after_seconds").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        return ProbeOutcome::RateLimited(retry);
    }
    if !status.is_success() {
        return ProbeOutcome::ServiceError;
    }

    let Ok(json) = serde_json::from_str::<Value>(&body) else {
        return ProbeOutcome::ServiceError;
    };
    let reachable = json
        .get(success_key)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    classify_checkport(reachable)
}

/// Classify a parsed `/checkport` `reachable` flag into a `ProbeOutcome`.
///
/// `/checkport` performs a single plain-TCP connect from check.quip.network to
/// the caller's public IP, so the result is binary: `reachable:true` is a
/// SYN-ACK (forward works *and* something is listening); `reachable:false` is
/// either a timeout (SYN dropped) or a RST/"connection refused" — in every
/// `false` case the prober could not open a TCP connection, so the port is not
/// externally reachable.
fn classify_checkport(reachable: bool) -> ProbeOutcome {
    if reachable {
        ProbeOutcome::HostResponded
    } else {
        ProbeOutcome::Unreachable
    }
}

async fn check_hostname_dns(hostname: &str) -> Option<bool> {
    let client = make_client(10)?;
    let resp = client
        .get(format!(
            "{}/checkhostname?hostname={}",
            CHECK_SERVICE, hostname
        ))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().await.ok()?;
    Some(json["match"].as_bool().unwrap_or(false))
}

// ─── public_host start precondition ───────────────────────────────────────────

/// Operator message when `public_host` is missing or empty.
pub(crate) const PUBLIC_HOST_REQUIRED: &str = concat!(
    "public_host is required. It is the address peers use to reach this node ",
    "from outside the host. Set it in Settings to a public DNS name or public IP.",
);

/// Operator message when `public_host` is a loopback or unspecified address.
pub(crate) const PUBLIC_HOST_NOT_ADVERTISABLE: &str = concat!(
    "public_host is required. It is the address peers use to reach this node ",
    "from outside the host. localhost, 127.0.0.1, and 0.0.0.0 are not reachable ",
    "from outside the host.",
);

/// Refuse to start when peers would have no usable address.
///
/// Empty, whitespace, and unparseable values fail as unset. Loopback and
/// unspecified addresses (localhost, 127.0.0.1, 0.0.0.0, ::1, ::) fail
/// because they are never reachable from outside the host.
pub(crate) fn require_public_host(public_host: &str) -> Result<(), String> {
    let Some(host) = crate::hostnames::public_host_name(public_host) else {
        return Err(PUBLIC_HOST_REQUIRED.to_string());
    };
    if is_unadvertisable_host(&host) {
        return Err(PUBLIC_HOST_NOT_ADVERTISABLE.to_string());
    }
    Ok(())
}

fn is_unadvertisable_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback() || ip.is_unspecified(),
        Err(_) => false,
    }
}

// ─── Check registry ───────────────────────────────────────────────────────────

/// IDs of all checks in render order. Filter by `visible_for_mode`.
pub const ALL_CHECK_IDS: &[&str] = &[
    "docker",
    "docker-compose",
    "wsl",
    "binary",
    "secret",
    "ip",
    "hostname",
    "port",
    "port-validator",
    "dwave-key",
];

/// Whether `id` is shown to the user for the current settings + run_mode.
pub fn visible_for_mode(id: &str, ctx: &CheckCtx) -> bool {
    match id {
        // Docker daemon + compose itself — v0.2 always runs compose services
        // in both Docker and Native manager modes, so these are always shown.
        // Image availability isn't pre-checked: Start always runs
        // `docker compose pull`, so a separate pre-flight image check (and its
        // load-time pull) would be redundant.
        "docker" | "docker-compose" => true,
        // Windows-only WSL probe. Docker mode only (Native is macOS-only).
        "wsl" => ctx.run_mode == RunMode::Docker && cfg!(target_os = "windows"),
        // Binary is native-mode only.
        "binary" => ctx.run_mode == RunMode::Native,
        // Visible whenever the user has a [dwave] block in NodeConfig
        // (i.e. they've opted into QPU mining). Passes if the token is
        // non-empty, fails otherwise.
        "dwave-key" => ctx.has_dwave_config,
        // Everything else is always visible: the two externally-probed
        // ports — public API + validator libp2p.
        "secret" | "ip" | "hostname" | "port" | "port-validator" => true,
        _ => false,
    }
}

pub fn visible_ids(ctx: &CheckCtx) -> Vec<String> {
    ALL_CHECK_IDS
        .iter()
        .filter(|id| visible_for_mode(id, ctx))
        .map(|s| s.to_string())
        .collect()
}

/// Fresh Idle placeholder for the given id. Used to seed the cache on
/// startup and on mode switch, so the UI can render the full list before
/// any check has run.
fn idle_item(id: &str, ctx: &CheckCtx) -> CheckItem {
    match id {
        "docker" => CheckItem::new(
            id,
            "Docker installed & running",
            true,
            Some(FixKind::InstallDocker),
        ),
        "docker-compose" => CheckItem::new(
            id,
            "Docker Compose v2 available",
            true,
            Some(FixKind::InstallDocker),
        ),
        "wsl" => CheckItem::new(id, "WSL installed with distro", false, None),
        // binary is not separately "fixable": its Retry recheck downloads the
        // binary as part of running the check.
        "binary" => CheckItem::new(id, "Native miner binary available", true, None),
        "secret" => CheckItem::new(
            id,
            "Node secret configured",
            true,
            Some(FixKind::GenerateSecret),
        ),
        "ip" => CheckItem::new(id, "Public IP reachable", false, None),
        "hostname" => CheckItem::new(id, "Hostname accessible to internet", false, None),
        "port" => CheckItem::new(
            id,
            &format!("Public API port {} — press Retry to test", ctx.port),
            false,
            None,
        ),
        "port-validator" => CheckItem::new(
            id,
            &format!("Validator P2P port {} reachable", ctx.validator_port),
            false,
            None,
        ),
        "dwave-key" => CheckItem::new(id, "D-Wave API token configured", true, None),
        _ => CheckItem::new(id, id, false, None),
    }
}

// ─── Per-check async runners ──────────────────────────────────────────────────
//
// Each returns the terminal CheckItem (state = Pass/Warn/Fail/Skip).
// The dispatcher wraps these with Running→emit→run→emit transitions.

async fn run_check_docker(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("docker", ctx);
    let ok = tokio::task::spawn_blocking(check_docker)
        .await
        .unwrap_or(false);
    if ok {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail)
            .with_detail("Docker is not running — start Docker Desktop or the Docker daemon")
    }
}

#[cfg(target_os = "windows")]
async fn run_check_wsl(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("wsl", ctx);
    let (ok, label) = tokio::task::spawn_blocking(check_wsl)
        .await
        .unwrap_or((false, "WSL check failed".into()));
    let state = if ok {
        CheckState::Pass
    } else {
        CheckState::Warn
    };
    base.with_state(state).with_label(label)
}

#[cfg(not(target_os = "windows"))]
async fn run_check_wsl(ctx: &CheckCtx) -> CheckItem {
    idle_item("wsl", ctx)
        .with_state(CheckState::Skip)
        .with_detail("non-Windows platform")
}

async fn run_check_docker_compose(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("docker-compose", ctx);
    // `docker compose version` exits 0 iff the v2+ CLI plugin is installed.
    // The legacy v1 was a separate `docker-compose` (hyphen) binary and
    // couldn't be invoked as `docker compose`, so we don't need to parse
    // the output string (Docker has already rev'd past v2 — e.g. v5.1.2
    // in Docker 29).
    let ok = tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args(["compose", "version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if ok {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail)
            .with_detail("install Docker Desktop, which ships with the `docker compose` CLI plugin")
    }
}

/// An outdated binary was detected: fetch the update now (a retry downloads
/// updates too) and report the result. Falls back to a Warn if there's no app
/// handle or the download fails.
async fn fetch_binary_update(ctx: &CheckCtx, base: CheckItem, version: &str) -> CheckItem {
    let outdated = base
        .clone()
        .with_state(CheckState::Warn)
        .with_label(format!(
            "Native miner outdated \u{2014} v{} available",
            version
        ));
    let Some(app) = &ctx.app else {
        return outdated;
    };
    match crate::native::download_native_binary(app.clone()).await {
        Ok(_) => base
            .with_state(CheckState::Pass)
            .with_label(format!("Native miner updated to v{}", version)),
        Err(e) => outdated.with_detail(e),
    }
}

/// Report an available binary update without downloading it. Used by automatic
/// rechecks (a Warn never blocks Start); the user remediates via the
/// Restart-to-Update flow or an explicit checklist Retry.
fn binary_update_available_item(base: CheckItem, installed: &str, version: &str) -> CheckItem {
    base.with_state(CheckState::Warn).with_label(format!(
        "Native miner update available \u{2014} v{version} (installed v{installed})"
    ))
}

/// Native miner binary. A missing binary is always downloaded (the required
/// check can't otherwise pass). An *available update* is downloaded only on a
/// user-initiated Retry (`auto == false`); an automatic recheck — e.g. the one
/// fired after a stop — reports it as a Warn without downloading. That matters
/// during Restart-to-Update: its plan already has a dedicated DownloadBinary
/// step, so an auto recheck that also downloaded would race it, re-fetching the
/// whole binary into the same fixed temp path. A current binary is left
/// untouched (the PyInstaller binary is large, so downloads are gated on need).
async fn run_check_binary(ctx: &CheckCtx, auto: bool) -> CheckItem {
    let base = idle_item("binary", ctx);
    let mut available = tokio::task::spawn_blocking(crate::native::is_binary_available)
        .await
        .unwrap_or(false);

    // Missing → download before reporting. Skipped when there's no app handle.
    if !available {
        // Capture the download error verbatim — download_native_binary already
        // returns a precise, user-actionable reason (release feed unreachable,
        // artifact not built yet, HTTP status, write error). Replacing it with a
        // generic "check your connection" hid the real cause.
        let mut download_err: Option<String> = None;
        if let Some(app) = &ctx.app {
            if let Err(e) = crate::native::download_native_binary(app.clone()).await {
                download_err = Some(e);
            }
            available = tokio::task::spawn_blocking(crate::native::is_binary_available)
                .await
                .unwrap_or(false);
        }
        if !available {
            let detail = download_err
                .unwrap_or_else(|| "no app handle available to start the download".to_string());
            return base
                .with_state(CheckState::Fail)
                .with_label("Native miner binary download failed")
                .with_detail(detail);
        }
    }

    match crate::native::resolve_binary_versions().await {
        Ok(v) => {
            let installed = v.installed.as_deref().unwrap_or("unknown");
            match (v.update, v.latest) {
                // A newer release exists. A user Retry fetches it now; an
                // automatic recheck reports it without downloading so it can't
                // race the Restart-to-Update flow's own DownloadBinary step.
                (Some(info), _) if !auto => fetch_binary_update(ctx, base, &info.version).await,
                (Some(info), _) => binary_update_available_item(base, installed, &info.version),
                // Confirmed current: show both numbers so the user can see what
                // was discovered, not just a green check.
                (None, Some(latest)) => base.with_state(CheckState::Pass).with_label(format!(
                    "Native miner binary up to date (installed v{installed}, latest v{latest})"
                )),
                // Installed, but the release feed was unreachable — we can't
                // confirm it's current. Green would over-claim, so warn (the
                // binary still runs — a Warn never blocks start).
                (None, None) => base
                    .with_state(CheckState::Warn)
                    .with_label(format!(
                        "Native miner binary installed (v{installed}); couldn't reach release feed"
                    ))
                    .with_detail("update check skipped — could not list quip-miner releases"),
            }
        }
        // Rare: HTTP client couldn't even be built. Binary still runs.
        Err(e) => base
            .with_state(CheckState::Warn)
            .with_label("Native miner binary installed (update check failed)")
            .with_detail(e),
    }
}

async fn run_check_secret(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("secret", ctx);
    if check_secret_exists() {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail)
            .with_detail("run Generate Secret")
    }
}

async fn run_check_ip(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("ip", ctx);
    match ctx.public_ip().await {
        Some(ip) => base
            .with_state(CheckState::Pass)
            .with_label(format!("Public IP: {}", ip)),
        None => base
            .with_state(CheckState::Warn)
            .with_label("Public IP unreachable"),
    }
}

async fn run_check_hostname(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("hostname", ctx);
    let ip_opt = ctx.public_ip().await;
    let public_host = ctx.public_host.clone();

    let (hostname, passed) = if !public_host.is_empty() {
        let host_only = public_host.split(':').next().unwrap_or(&public_host);
        let passed = if host_only.parse::<std::net::IpAddr>().is_ok() {
            ip_opt.is_some()
        } else {
            match check_hostname_dns(host_only).await {
                Some(matched) => matched,
                None => ip_opt.is_some(),
            }
        };
        (public_host, passed)
    } else {
        let ip = ip_opt.as_deref().unwrap_or("unknown").to_string();
        let passed = ip != "unknown";
        (ip, passed)
    };

    let state = if passed {
        CheckState::Pass
    } else {
        CheckState::Warn
    };
    base.with_state(state)
        .with_label(format!("{} accessible to internet", hostname))
}

/// Map a `PortProbeResult` to a checklist state + label. `noun` distinguishes
/// the public API port from the validator P2P port in the message. Shared by
/// both port checks so they stay consistent.
fn port_probe_state_label(result: PortProbeResult, noun: &str, port: u16) -> (CheckState, String) {
    match result {
        PortProbeResult::Verified => (
            CheckState::Pass,
            format!("{noun} port {port} reachable (host responded)"),
        ),
        PortProbeResult::ForwardReady => (
            CheckState::Pass,
            format!("{noun} port {port} TCP forward ready"),
        ),
        PortProbeResult::Unreachable => (
            CheckState::Warn,
            format!("{noun} port {port} not reachable \u{2014} check router forward + firewall"),
        ),
        PortProbeResult::Unverified => (
            CheckState::Warn,
            format!(
                "{noun} port {port} \u{2014} couldn't externally verify \
                 (check.quip.network unreachable)"
            ),
        ),
        PortProbeResult::RateLimited {
            retry_after_secs,
            endpoint,
        } => (
            CheckState::Warn,
            format!(
                "{noun} port {port} \u{2014} couldn't externally verify \
                 (/{endpoint} rate-limited, retry in {retry_after_secs}s)"
            ),
        ),
    }
}

async fn run_check_port(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("port", ctx);
    let result = probe_port_forwarding_with_ctx(ctx, ctx.port).await;
    let (state, label) = port_probe_state_label(result, "Public API", ctx.port);
    base.with_state(state).with_label(label)
}

async fn run_check_port_validator(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("port-validator", ctx);
    let result = probe_port_forwarding_with_ctx(ctx, ctx.validator_port).await;
    let (state, label) = port_probe_state_label(result, "Validator P2P", ctx.validator_port);
    base.with_state(state).with_label(label)
}

async fn run_check_dwave_key(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("dwave-key", ctx);
    if ctx.dwave_token_set {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail)
            .with_detail("set the D-Wave API token in [dwave] before starting a QPU node")
    }
}

/// Dispatch by id. Unknown ids return a Skip item. `auto` distinguishes a
/// system-triggered recheck from a user Retry; only the binary check reads it
/// (to gate the update download).
async fn run_check_by_id(id: &str, ctx: &CheckCtx, auto: bool) -> CheckItem {
    match id {
        "docker" => run_check_docker(ctx).await,
        "docker-compose" => run_check_docker_compose(ctx).await,
        "wsl" => run_check_wsl(ctx).await,
        "binary" => run_check_binary(ctx, auto).await,
        "secret" => run_check_secret(ctx).await,
        "ip" => run_check_ip(ctx).await,
        "hostname" => run_check_hostname(ctx).await,
        "port" => run_check_port(ctx).await,
        "port-validator" => run_check_port_validator(ctx).await,
        "dwave-key" => run_check_dwave_key(ctx).await,
        _ => idle_item(id, ctx)
            .with_state(CheckState::Skip)
            .with_detail("unknown check id"),
    }
}

// ─── Event emission ───────────────────────────────────────────────────────────

fn emit_item(app: &AppHandle, item: &CheckItem) {
    let _ = app.emit("checklist-update", item);
}

/// Append a `[checklist]` entry to the node-log so the console shows every
/// state transition alongside the node's own output.
fn emit_log(app: &AppHandle, auto: bool, verb: &str, item: &CheckItem, level: &str) {
    let prefix = if auto {
        "[checklist] [auto] "
    } else {
        "[checklist] "
    };
    let detail = item
        .detail
        .as_ref()
        .map(|d| format!(" \u{2014} {}", d))
        .unwrap_or_default();
    let message = format!("{}{}: {}{}", prefix, verb, item.label, detail);
    let entry = serde_json::json!({
        "timestamp": "",
        "level": level,
        "message": message,
        "source": "app",
    });
    let _ = app.emit("node-log", entry);
}

fn verb_for_state(state: &CheckState) -> (&'static str, &'static str) {
    match state {
        CheckState::Running => ("rechecking", "INFO"),
        CheckState::Pass => ("ok", "INFO"),
        CheckState::Warn => ("warn", "WARN"),
        CheckState::Fail => ("fail", "ERROR"),
        CheckState::Skip => ("skip", "INFO"),
        CheckState::Idle => ("idle", "INFO"),
    }
}

// ─── Recheck dispatcher ───────────────────────────────────────────────────────

/// Run a single check: set Running → emit → run → set terminal → emit.
///
/// `auto` prefixes console log entries with `[auto]` so users can distinguish
/// rechecks they triggered from rechecks the system ran after an action.
async fn recheck_one(
    app: &AppHandle,
    state: &ChecklistState,
    ctx: &CheckCtx,
    id: &str,
    auto: bool,
) {
    // If this check is already running, drop the request rather than queue.
    let per_id = state.lock_for(id);
    let _guard = match per_id.try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    let _permit = state.sem.acquire().await.ok();

    // Transition to Running.
    let running = {
        let mut cache = state.cache.lock().await;
        let base = cache.get(id).cloned().unwrap_or_else(|| idle_item(id, ctx));
        let running = base.with_state(CheckState::Running);
        cache.insert(id.to_string(), running.clone());
        running
    };
    emit_item(app, &running);
    emit_log(
        app,
        auto,
        verb_for_state(&running.state).0,
        &running,
        verb_for_state(&running.state).1,
    );

    // Run the check.
    let final_item = run_check_by_id(id, ctx, auto).await;

    {
        let mut cache = state.cache.lock().await;
        cache.insert(id.to_string(), final_item.clone());
    }
    emit_item(app, &final_item);
    let (verb, level) = verb_for_state(&final_item.state);
    emit_log(app, auto, verb, &final_item, level);
}

/// Seed the cache with Idle entries for every id visible in the current
/// run-mode (overwrites any stale entries from a different mode).
async fn seed_cache(state: &ChecklistState, ctx: &CheckCtx) {
    let mut cache = state.cache.lock().await;
    cache.clear();
    for id in visible_ids(ctx) {
        cache.insert(id.clone(), idle_item(&id, ctx));
    }
}

/// Shared implementation between the `recheck` Tauri command and the
/// `trigger_recheck_auto` helper used by docker/native action handlers.
async fn run_recheck(app: AppHandle, ids: Option<Vec<String>>, auto: bool) -> Result<(), String> {
    let state: tauri::State<'_, ChecklistState> = app.state();
    let ctx = Arc::new(CheckCtx::from_settings(Some(app.clone())));

    // A user-initiated Retry forces fresh external probes; auto/background
    // rechecks reuse the 5-minute cache so we don't trip check.quip.network's
    // rate limit.
    if !auto {
        clear_port_probe_cache();
    }

    let ids = match ids {
        Some(ids) if !ids.is_empty() => ids,
        _ => {
            // Global recheck: seed the cache for the current mode, then run all.
            seed_cache(&state, &ctx).await;
            visible_ids(&ctx)
        }
    };

    // Fire off all rechecks concurrently; the per-id lock drops duplicates
    // and the semaphore caps real parallelism at 3.
    let mut handles = Vec::with_capacity(ids.len());
    for id in ids {
        let app = app.clone();
        let ctx = ctx.clone();
        handles.push(tokio::spawn(async move {
            let state: tauri::State<'_, ChecklistState> = app.state();
            recheck_one(&app, &state, &ctx, &id, auto).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

// ─── Tauri commands ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_checklist(
    state: tauri::State<'_, ChecklistState>,
) -> Result<Vec<CheckItem>, String> {
    let cache = state.cache.lock().await;
    let ctx = CheckCtx::from_settings(None);
    let ids = visible_ids(&ctx);
    Ok(ids
        .into_iter()
        .map(|id| {
            cache
                .get(&id)
                .cloned()
                .unwrap_or_else(|| idle_item(&id, &ctx))
        })
        .collect())
}

#[tauri::command]
pub async fn recheck(app: AppHandle, ids: Option<Vec<String>>) -> Result<(), String> {
    run_recheck(app, ids, false).await
}

/// Helper for docker/native action handlers to fire a follow-on recheck of
/// specific ids after their operation completes. Logged with `[auto]` prefix.
pub async fn trigger_recheck_auto(app: AppHandle, ids: Vec<String>) {
    let _ = run_recheck(app, Some(ids), true).await;
}

// ─── TUI helpers ──────────────────────────────────────────────────────────────
//
// The TUI runs outside Tauri and can't use the recheck command directly,
// so it gets a simple sequential "run everything" API plus a default-IP
// port probe. These are thin wrappers over the same per-check functions
// the GUI uses, so there's no separate code path for the TUI to drift on.

/// Run every check visible for `run_mode` sequentially and return the
/// final CheckItems. For non-Tauri callers (TUI).
pub async fn run_all_checks(run_mode: &RunMode) -> Vec<CheckItem> {
    let settings = crate::settings::load_settings();
    let dwave_token_set = settings
        .node_config
        .dwave_config
        .as_ref()
        .map(|d| !d.token.trim().is_empty())
        .unwrap_or(false);
    let has_dwave_config = settings.node_config.dwave_config.is_some();
    let ctx = CheckCtx {
        run_mode: run_mode.clone(),
        image_tag: settings.image_tag,
        port: settings.node_config.port,
        validator_port: settings.node_config.validator_port,
        public_host: settings.node_config.public_host,
        has_dwave_config,
        dwave_token_set,
        app: None,
        public_ip: OnceCell::new(),
    };
    let mut results = Vec::new();
    for id in visible_ids(&ctx) {
        // TUI "run everything" is user-initiated (auto = false). It never has an
        // app handle, so no download happens regardless.
        results.push(run_check_by_id(&id, &ctx, false).await);
    }
    results
}

/// Convenience for the TUI public API port recheck. Returns a plain bool since
/// the TUI doesn't render the richer diagnostic the GUI uses.
pub async fn probe_public_api_port_with_default_ip(port: u16) -> bool {
    probe_port_forwarding(port).await.is_externally_reachable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ImageTag;

    fn test_ctx() -> CheckCtx {
        CheckCtx {
            run_mode: RunMode::Docker,
            image_tag: ImageTag::Cpu,
            port: 20049,
            validator_port: 30333,
            public_host: String::new(),
            has_dwave_config: false,
            dwave_token_set: false,
            app: None,
            public_ip: OnceCell::new(),
        }
    }

    #[test]
    fn v02_port_labels_use_api_and_validator_defaults() {
        let ctx = test_ctx();

        assert_eq!(
            idle_item("port", &ctx).label,
            "Public API port 20049 \u{2014} press Retry to test"
        );
        assert_eq!(
            idle_item("port-validator", &ctx).label,
            "Validator P2P port 30333 reachable"
        );
    }

    #[test]
    fn auto_recheck_reports_binary_update_without_downloading() {
        // Regression guard for the double-download race: the post-stop auto
        // recheck must report an available update, not fetch it (the
        // Restart-to-Update flow's DownloadBinary step owns the download). The
        // item is a Warn (never blocks Start) showing both versions.
        let base = idle_item("binary", &test_ctx());
        let item = binary_update_available_item(base, "0.2.1-rc4", "0.2.1-rc39");
        assert_eq!(item.state, CheckState::Warn);
        assert!(item.label.contains("update available"));
        assert!(item.label.contains("v0.2.1-rc39"));
        assert!(item.label.contains("installed v0.2.1-rc4"));
    }

    #[test]
    fn image_availability_is_not_pre_checked() {
        // Start always runs `docker compose pull`, so there is no separate
        // pre-flight image-availability check (the old "stack-images" check is
        // gone, as is the standalone "version" freshness check). Native still
        // verifies its host binary.
        assert!(!ALL_CHECK_IDS.contains(&"version"));
        assert!(
            !ALL_CHECK_IDS.contains(&"stack-images"),
            "stack-images pre-flight check should be removed (we pull on Start)"
        );
        assert!(ALL_CHECK_IDS.contains(&"binary"));
    }

    #[test]
    fn checklist_only_probes_public_api_and_validator_ports() {
        // Internal-plumbing port checks and the duplicative local firewall
        // checks are gone — only the two internet-reachability probes remain.
        for removed in [
            "port-dashboard",
            "port-tls",
            "rest-port-native",
            "firewall",
            "firewall-api",
            "firewall-validator",
        ] {
            assert!(
                !ALL_CHECK_IDS.contains(&removed),
                "{removed} should be removed from the checklist"
            );
        }
    }

    #[test]
    fn unverified_probe_warns_that_it_could_not_externally_verify() {
        let (state, label) =
            port_probe_state_label(PortProbeResult::Unverified, "Public API", 20049);
        assert_eq!(state, CheckState::Warn);
        assert!(
            label.contains("couldn't externally verify"),
            "label was: {label}"
        );
    }

    #[test]
    fn connection_refused_is_not_reachable() {
        // From the field: GET /checkport?port=30333 → HTTP 200
        // {"reachable":false,"error":"dial tcp 96.233.112.201:30333: connect:
        // connection refused"}. A plain-TCP RST means the prober never reached
        // our listener, so the port is NOT externally reachable — this must map
        // to the not-reachable outcome (Warn/orange), never a pass.
        let outcome = classify_checkport(false);
        assert!(
            matches!(outcome, ProbeOutcome::Unreachable),
            "connection refused should classify as not-reachable"
        );
    }

    #[test]
    fn reachable_true_is_the_only_passing_outcome() {
        // Guards the success path so the binary classifier can't regress into
        // failing a genuinely reachable port.
        assert!(matches!(
            classify_checkport(true),
            ProbeOutcome::HostResponded
        ));
    }

    #[test]
    fn reachable_probe_results_still_pass() {
        for result in [PortProbeResult::Verified, PortProbeResult::ForwardReady] {
            let (state, _) = port_probe_state_label(result, "Validator P2P", 30033);
            assert_eq!(state, CheckState::Pass);
        }
    }

    #[test]
    fn rate_limited_probe_warns_not_passes() {
        let result = PortProbeResult::RateLimited {
            retry_after_secs: 2909,
            endpoint: "checkport",
        };
        let (state, label) = port_probe_state_label(result, "Validator P2P", 30333);
        assert_eq!(state, CheckState::Warn);
        assert!(
            label.contains("couldn't externally verify"),
            "label was: {label}"
        );
        assert!(label.contains("2909s"), "label was: {label}");
        // Only a positive response from check.quip.network counts as reachable.
        assert!(!result.is_externally_reachable());
    }

    #[test]
    fn validator_probe_immediately_follows_public_api_probe() {
        let ctx = test_ctx();
        let ids = visible_ids(&ctx);
        let api = ids.iter().position(|id| id == "port").unwrap();
        let validator = ids.iter().position(|id| id == "port-validator").unwrap();
        assert_eq!(validator, api + 1);
    }

    #[test]
    fn validator_port_check_is_visible_and_warning_only() {
        let ctx = test_ctx();
        let item = idle_item("port-validator", &ctx);
        assert!(!item.required);
        assert_eq!(item.fixable, None);
    }

    #[test]
    fn empty_public_host_fails_the_start_precondition() {
        let config = crate::settings::NodeConfig::default();
        let err = require_public_host(&config.public_host).expect_err("empty must fail");
        assert!(
            err.contains("public_host"),
            "message must name the field: {err}"
        );
        assert!(
            err.contains("required"),
            "message must say the field is required: {err}"
        );
        assert!(
            err.contains("outside the host"),
            "message must say what public_host is for: {err}"
        );
    }

    #[test]
    fn loopback_public_host_fails_the_start_precondition() {
        for host in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "0.0.0.0",
            "127.0.0.1:20049",
            "::1",
            "[::1]",
        ] {
            let config = crate::settings::NodeConfig {
                public_host: host.to_string(),
                ..crate::settings::NodeConfig::default()
            };
            let err =
                require_public_host(&config.public_host).expect_err(&format!("{host} must fail"));
            assert!(
                err.contains("public_host"),
                "host {host:?} message must name the field: {err}"
            );
        }
    }

    #[test]
    fn advertisable_public_host_passes_the_start_precondition() {
        for host in [
            "node.example.com",
            "203.0.113.9",
            "https://node.example.com:9443/rpc",
            "2001:db8::1",
        ] {
            require_public_host(host).unwrap_or_else(|e| panic!("{host:?} must pass: {e}"));
        }
    }

    /// The public host is covered by the `port` rows, which probe it end to
    /// end. A dedicated row would duplicate that with a weaker signal.
    #[test]
    fn there_is_no_standalone_public_host_row() {
        assert!(!ALL_CHECK_IDS.contains(&"public-host"));
    }
}
