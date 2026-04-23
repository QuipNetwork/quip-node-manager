// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
    PullImage,
    DownloadBinary,
    GenerateSecret,
    /// Delegate to another check's fix (e.g. version → image or binary).
    Delegate(String),
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
    pub port: u16,
    pub public_host: String,
    /// "Is the compose stack expected to be running?" — drives visibility of
    /// dashboard/TLS/postgres-related checks.
    pub dashboard_enabled: bool,
    pub tls_enabled: bool,
    /// The port the native binary exposes for dashboard telemetry in Native
    /// mode. Passed into `rest-port-native` without recomputing defaults.
    pub native_rest_port: u16,
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
    /// `true` iff `docker compose ps` reports any service as running — our
    /// own stack legitimately holds ports 20080 / 80 / 443 in that case, so
    /// the port-conflict checks should pass rather than warn.
    stack_running: OnceCell<bool>,
}

impl CheckCtx {
    fn from_settings(app: Option<AppHandle>) -> Self {
        let settings = crate::settings::load_settings();
        let native_rest_port =
            crate::compose::native_rest_port(&settings.node_config);
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
            public_host: settings.node_config.public_host,
            dashboard_enabled: settings.dashboard_enabled,
            tls_enabled: settings.tls_enabled,
            native_rest_port,
            has_dwave_config,
            dwave_token_set,
            app,
            public_ip: OnceCell::new(),
            stack_running: OnceCell::new(),
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
        });
        let _ = app.emit("node-log", entry);
    }

    /// Memoised public-IP fetch. First caller pays the network cost;
    /// subsequent callers in the same recheck batch reuse the result.
    async fn public_ip(&self) -> Option<String> {
        self.public_ip
            .get_or_init(fetch_public_ip)
            .await
            .clone()
    }

    /// Memoised "is our compose stack currently up?" probe. Any service
    /// reporting `running=true` counts — covers healthy, starting, and
    /// unhealthy-but-running cases, all of which hold their published
    /// ports. Errors (docker missing, stack never started) fall back to
    /// `false` so the port bind-test runs as before.
    async fn stack_running(&self) -> bool {
        *self
            .stack_running
            .get_or_init(|| async {
                crate::compose::get_stack_status()
                    .await
                    .map(|s| s.services.iter().any(|svc| svc.running))
                    .unwrap_or(false)
            })
            .await
    }

    /// Whether `docker compose` is expected to have anything to run. False
    /// only when Native mode + dashboard disabled.
    fn compose_will_run(&self) -> bool {
        self.run_mode != RunMode::Native || self.dashboard_enabled
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
    if let Ok(out) = crate::cmd::new("wsl").args(["--list", "--verbose"]).output() {
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

/// `docker image inspect <ref>` — true iff the image is already present on
/// the local daemon. Used by the stack-images aggregator.
fn docker_image_present(image_ref: &str) -> bool {
    crate::cmd::new("docker")
        .args(["image", "inspect", image_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Images the current profile + service list expects to find locally.
/// In Native mode the node image is excluded (the binary runs on the host).
fn required_stack_images(ctx: &CheckCtx) -> Vec<String> {
    let mut images = Vec::new();
    if ctx.run_mode == RunMode::Docker {
        images.push(format!(
            "{}:latest",
            crate::compose::image_for_tag(ctx.image_tag)
        ));
    }
    if ctx.dashboard_enabled {
        images.push(
            "registry.gitlab.com/quip.network/dashboard.quip.network:latest"
                .into(),
        );
        images.push("postgres:16".into());
        if ctx.tls_enabled {
            images.push("caddy:2-alpine".into());
        }
    }
    images
}

/// Bindability test for a TCP port. Used by port-dashboard / port-tls /
/// rest-port-native to flag conflicts before the user presses Start.
fn tcp_port_bindable(port: u16) -> bool {
    let addr: std::net::SocketAddr =
        format!("0.0.0.0:{}", port).parse().unwrap();
    std::net::TcpListener::bind(addr).is_ok()
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

async fn fetch_public_ip() -> Option<String> {
    if let Some(ip) = fetch_ip_check_service().await {
        return Some(ip);
    }
    fetch_ip_ipify().await
}

async fn fetch_ip_check_service() -> Option<String> {
    let client = make_client(5)?;
    let resp = client.get(format!("{}/ip", CHECK_SERVICE)).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().await.ok()?;
    let ip = json["ip"].as_str()?.trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

async fn fetch_ip_ipify() -> Option<String> {
    let client = make_client(10)?;
    let resp = client.get("https://api.ipify.org").send().await.ok()?;
    let text = resp.text().await.ok()?;
    let ip = text.trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

/// Outcome of `probe_port_forwarding`. The probe picks between a TCP
/// forward check and a full QUIC handshake check based on whether the
/// node is already bound to the port — that determines what we can
/// actually verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortProbeResult {
    /// Node is running and a full QUIC/QUIP handshake completed over UDP.
    /// End-to-end reachability is verified.
    Verified,
    /// Node is running but the QUIC handshake failed. UDP is likely not
    /// forwarded, a firewall is blocking it, or the node isn't speaking
    /// QUIP correctly.
    QuicHandshakeFailed,
    /// Node isn't running; the router is forwarding TCP to this host
    /// (verified via a temp listener). Once the node starts, a recheck
    /// should escalate this to `Verified`.
    ForwardReady,
    /// Node isn't running and TCP forwarding doesn't work either. The
    /// router isn't reflecting traffic to this host or a firewall is
    /// dropping it.
    Unreachable,
    /// check.quip.network rate-limited the request. We can't verify right
    /// now but the port may well be fine — treat as passing until the
    /// cool-down expires and a real recheck can run.
    RateLimited {
        /// Seconds until the ban is expected to lift, per the service's
        /// `retry_after_seconds` field.
        retry_after_secs: u64,
        /// Which endpoint got limited: "checkport" or "checkconn".
        endpoint: &'static str,
    },
}

impl PortProbeResult {
    pub fn is_externally_reachable(self) -> bool {
        matches!(
            self,
            Self::Verified | Self::ForwardReady | Self::RateLimited { .. }
        )
    }
}

/// Result of a single call to `/checkport` or `/checkconn`. Distinguishes
/// "no response from the host at all" (definitive fail) from "host
/// responded, just not as expected" (router forwards — pass) and from
/// service-side errors we shouldn't blame the user for.
enum ProbeOutcome {
    /// Service returned success-key=true OR success-key=false with an
    /// error indicating the host *did* respond (protocol-level mismatch,
    /// RST, handshake succeeded but status response missing, etc.). From
    /// the router-forwarding perspective these are all passing cases.
    HostResponded,
    /// Service returned success-key=false AND the error indicates no
    /// response at the UDP/TCP layer (timeout, unreachable, no route).
    Timeout,
    /// HTTP 429 with `retry_after_seconds`. We can't verify right now,
    /// so we optimistically pass but preserve the retry time for the UX.
    RateLimited(u64),
    /// Any other failure — HTTP 5xx, non-JSON body, client-side network
    /// error. Treated as lenient-pass (not the user's fault).
    ServiceError,
}

/// Classify an `error` string from check.quip.network as a pure
/// connect-level timeout (no response from the host) vs any other kind
/// of failure (host responded, just not with a full protocol success).
///
/// Conservative heuristic — when in doubt, treat as responded. That
/// matches the "only failure to connect is a fail" rule.
fn is_connect_timeout(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("unreachable")
        || lower.contains("no route")
}

/// Port forwarding check. One probe runs, not two:
///
///   - If something is already bound to `port` locally (i.e. we can't
///     `TcpListener::bind` it), the node is presumably running — so we
///     ask `check.quip.network/checkconn` to do a real QUIP handshake.
///     A pass there proves UDP forwarding + node health end-to-end.
///
///   - If the port is free, the node isn't running and a QUIC handshake
///     can't possibly succeed. We instead verify the TCP forward by
///     holding a temp listener for the duration of `/checkport`. Users
///     can click Recheck after starting the node to escalate to the
///     QUIC verification path.
/// GUI-facing entry point. `ctx.app` is used to emit the full check.quip.network
/// request URL, HTTP status, and response body into `node-log` so users can
/// copy/paste the raw output when asking for support.
async fn probe_port_forwarding_with_ctx(
    ctx: &CheckCtx,
    port: u16,
) -> PortProbeResult {
    use tokio::net::TcpListener;

    match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
        Err(e) => {
            ctx.log_probe(
                "INFO",
                format!(
                    "port {} in use locally ({}) \u{2014} assuming node is up; using QUIC probe",
                    port, e
                ),
            );
            match probe_external_quic(ctx, port).await {
                ProbeOutcome::HostResponded => PortProbeResult::Verified,
                ProbeOutcome::Timeout => PortProbeResult::QuicHandshakeFailed,
                ProbeOutcome::RateLimited(retry) => PortProbeResult::RateLimited {
                    retry_after_secs: retry,
                    endpoint: "checkconn",
                },
                // Lenient-pass on service error: we can't blame the user
                // when check.quip.network is down, misbehaving, or
                // unreachable from our end. The port is locally bound,
                // which is the best signal we have.
                ProbeOutcome::ServiceError => PortProbeResult::Verified,
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
                ProbeOutcome::Timeout => PortProbeResult::Unreachable,
                ProbeOutcome::RateLimited(retry) => PortProbeResult::RateLimited {
                    retry_after_secs: retry,
                    endpoint: "checkport",
                },
                // Lenient-pass on service error — no connectivity signal
                // either way when check.quip.network isn't cooperating.
                ProbeOutcome::ServiceError => PortProbeResult::ForwardReady,
            }
        }
    }
}

/// Plain wrapper for callers without a `CheckCtx` (TUI). Runs silently.
pub async fn probe_port_forwarding(port: u16) -> PortProbeResult {
    let ctx = CheckCtx::from_settings(None);
    probe_port_forwarding_with_ctx(&ctx, port).await
}

async fn probe_external_quic(ctx: &CheckCtx, port: u16) -> ProbeOutcome {
    fetch_probe_json(ctx, "checkconn", port, "quip", 15).await
}

async fn probe_external_tcp(ctx: &CheckCtx, port: u16) -> ProbeOutcome {
    fetch_probe_json(ctx, "checkport", port, "reachable", 10).await
}

/// Shared HTTP fetcher for `/checkport` and `/checkconn`. Every step
/// (URL, network error, HTTP status, response body) is emitted to
/// `node-log` via `ctx.log_probe` so users can see service-side errors
/// like `"handshake timeout"`, `"alpn mismatch"`, or `"connection refused"`
/// without having to reproduce the request by hand.
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
        ctx.log_probe("ERROR", format!("{}: failed to build HTTP client", endpoint));
        return ProbeOutcome::ServiceError;
    };

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            ctx.log_probe(
                "ERROR",
                format!("{}: network error talking to check.quip.network: {}", endpoint, e),
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
        format!("{}\u{2026}(truncated, {} bytes total)", &body[..1024], body.len())
    } else {
        body.clone()
    };
    ctx.log_probe(
        if status.is_success() { "INFO" } else { "ERROR" },
        format!("{} \u{2192} HTTP {} {}", endpoint, status.as_u16(), body_for_log),
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
    let success = json.get(success_key).and_then(|v| v.as_bool()).unwrap_or(false);
    if success {
        return ProbeOutcome::HostResponded;
    }
    // success-key is false — classify by the error string. A pure connect
    // timeout means the host didn't respond at all. Anything else (ALPN
    // mismatch, RST, TLS error, banner timeout, etc.) means the host IS
    // reachable at the transport layer, so the router forward is working.
    let error_str = json
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if is_connect_timeout(error_str) {
        ProbeOutcome::Timeout
    } else {
        ProbeOutcome::HostResponded
    }
}

async fn check_hostname_dns(hostname: &str) -> Option<bool> {
    let client = make_client(10)?;
    let resp = client
        .get(format!("{}/checkhostname?hostname={}", CHECK_SERVICE, hostname))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().await.ok()?;
    Some(json["match"].as_bool().unwrap_or(false))
}

// ─── Local firewall probe (platform-specific) ─────────────────────────────────

#[cfg(target_os = "macos")]
fn os_firewall_check(port: u16) -> Option<(bool, String)> {
    let out = crate::cmd::new("/usr/libexec/ApplicationFirewall/socketfilterfw")
        .arg("--getglobalstate")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    if text.contains("disabled") || text.contains("state = 0") {
        Some((true, format!("macOS Firewall: Port {} open (UDP+TCP)", port)))
    } else if text.contains("enabled") || text.contains("state = 1") {
        Some((
            true,
            format!(
                "macOS Firewall: Port {} open (ensure app allowed for UDP+TCP)",
                port
            ),
        ))
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn os_firewall_check(port: u16) -> Option<(bool, String)> {
    let out = crate::cmd::new("ufw").args(["status"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.to_lowercase().contains("inactive") {
        return Some((true, "ufw inactive".to_string()));
    }
    if text.to_lowercase().contains("active") {
        let has_udp = text.contains(&format!("{}/udp", port));
        let has_tcp = text.contains(&format!("{}/tcp", port));
        if has_udp && has_tcp {
            return Some((true, format!("ufw allows {}/udp and {}/tcp", port, port)));
        }
        let mut missing = Vec::new();
        if !has_udp {
            missing.push(format!("{}/udp", port));
        }
        if !has_tcp {
            missing.push(format!("{}/tcp", port));
        }
        return Some((
            false,
            format!(
                "ufw active \u{2014} run: sudo ufw allow {}",
                missing.join(" && sudo ufw allow ")
            ),
        ));
    }
    None
}

#[cfg(target_os = "windows")]
fn os_firewall_check(port: u16) -> Option<(bool, String)> {
    let state = crate::cmd::new("netsh")
        .args(["advfirewall", "show", "allprofiles", "state"])
        .output()
        .ok()?;
    let state_text = String::from_utf8_lossy(&state.stdout).to_lowercase();
    let all_off = state_text
        .lines()
        .filter(|l| l.contains("state"))
        .all(|l| l.contains("off"));
    if all_off {
        return Some((true, "Windows Firewall disabled".to_string()));
    }
    let rule = crate::cmd::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", "name=all", "dir=in"])
        .output()
        .ok()?;
    let rule_text = String::from_utf8_lossy(&rule.stdout);
    let port_str = port.to_string();
    let mut found_udp = false;
    let mut found_tcp = false;
    let mut cur_proto = String::new();
    let mut port_match = false;
    let mut is_allow = false;
    for line in rule_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if port_match && is_allow {
                if cur_proto == "udp" { found_udp = true; }
                if cur_proto == "tcp" { found_tcp = true; }
            }
            cur_proto.clear();
            port_match = false;
            is_allow = false;
            continue;
        }
        if let Some((key, val)) = trimmed.split_once(':') {
            let key = key.trim().to_lowercase();
            let val = val.trim().to_lowercase();
            if key == "protocol" { cur_proto = val.clone(); }
            if key == "localport" && val.contains(&port_str) { port_match = true; }
            if key == "action" && val == "allow" { is_allow = true; }
        }
    }
    if port_match && is_allow {
        if cur_proto == "udp" { found_udp = true; }
        if cur_proto == "tcp" { found_tcp = true; }
    }
    if found_udp && found_tcp {
        Some((true, format!("Windows Firewall allows {}/udp and {}/tcp", port, port)))
    } else {
        let mut missing = Vec::new();
        if !found_udp { missing.push("UDP"); }
        if !found_tcp { missing.push("TCP"); }
        Some((
            false,
            format!(
                "Windows Firewall may block {} \u{2014} add inbound {} rule(s) for port {}",
                missing.join("+"),
                missing.join(" and "),
                port
            ),
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn os_firewall_check(_port: u16) -> Option<(bool, String)> {
    None
}

fn check_local_firewall(port: u16) -> (bool, String) {
    if let Some(result) = os_firewall_check(port) {
        return result;
    }
    let addr: std::net::SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    let udp_ok = UdpSocket::bind(addr).is_ok();
    let tcp_ok = std::net::TcpListener::bind(addr).is_ok();
    match (udp_ok, tcp_ok) {
        (true, true) => (true, format!("Port {} bindable locally (UDP+TCP)", port)),
        (true, false) => (false, format!("Port {}: UDP bindable but TCP blocked", port)),
        (false, true) => (false, format!("Port {}: TCP bindable but UDP blocked", port)),
        (false, false) => (false, format!("Cannot bind port {} (UDP+TCP)", port)),
    }
}

// ─── Check registry ───────────────────────────────────────────────────────────

/// IDs of all checks in render order. Filter by `visible_for_mode`.
pub const ALL_CHECK_IDS: &[&str] = &[
    "docker",
    "docker-compose",
    "stack-assets",
    "wsl",
    "stack-images",
    "binary",
    "version",
    "secret",
    "ip",
    "hostname",
    "port",
    "port-dashboard",
    "port-tls",
    "rest-port-native",
    "firewall",
    "dwave-key",
];

/// Whether `id` is shown to the user for the current settings + run_mode.
pub fn visible_for_mode(id: &str, ctx: &CheckCtx) -> bool {
    match id {
        // Docker daemon + compose itself — required whenever compose will run.
        "docker" | "docker-compose" | "stack-assets" | "stack-images" => {
            ctx.compose_will_run()
        }
        // Windows-only WSL probe. Docker mode only (Native is macOS-only).
        "wsl" => ctx.run_mode == RunMode::Docker && cfg!(target_os = "windows"),
        // Binary is native-mode only.
        "binary" => ctx.run_mode == RunMode::Native,
        // New per-port bind checks. Only applicable when compose will run AND
        // that profile actually binds the port.
        "port-dashboard" => {
            ctx.compose_will_run()
                && ctx.dashboard_enabled
                && !ctx.tls_enabled
        }
        "port-tls" => {
            ctx.compose_will_run() && ctx.dashboard_enabled && ctx.tls_enabled
        }
        // Native + dashboard makes the native binary bind a REST port that
        // Docker containers reach via host.docker.internal.
        "rest-port-native" => {
            ctx.run_mode == RunMode::Native && ctx.dashboard_enabled
        }
        // Visible whenever the user has a [dwave] block in NodeConfig
        // (i.e. they've opted into QPU mining). Passes if the token is
        // non-empty, fails otherwise.
        "dwave-key" => ctx.has_dwave_config,
        // Everything else is always visible.
        "version" | "secret" | "ip" | "hostname" | "port" | "firewall" => true,
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
        "docker" => CheckItem::new(id, "Docker installed & running", true, Some(FixKind::InstallDocker)),
        "docker-compose" => CheckItem::new(id, "Docker Compose v2 available", true, Some(FixKind::InstallDocker)),
        "stack-assets" => CheckItem::new(id, "Stack files staged (compose.yml + Caddyfile)", true, None),
        "wsl" => CheckItem::new(id, "WSL installed with distro", false, None),
        "stack-images" => CheckItem::new(id, "Stack images available", true, Some(FixKind::PullImage)),
        "binary" => CheckItem::new(id, "Node binary available", true, Some(FixKind::DownloadBinary)),
        "version" => CheckItem::new(id, "Node version up to date", false, Some(FixKind::Delegate(match ctx.run_mode {
            RunMode::Docker => "stack-images".into(),
            RunMode::Native => "binary".into(),
        }))),
        "secret" => CheckItem::new(id, "Node secret configured", true, Some(FixKind::GenerateSecret)),
        "ip" => CheckItem::new(id, "Public IP reachable", false, None),
        "hostname" => CheckItem::new(id, "Hostname accessible to internet", false, None),
        "port" => CheckItem::new(id, &format!("Port {} — press Recheck to test", ctx.port), false, None),
        "port-dashboard" => CheckItem::new(id, "Dashboard port 20080 available", false, None),
        "port-tls" => CheckItem::new(id, "TLS ports 80 + 443 available", false, None),
        "rest-port-native" => CheckItem::new(id, &format!("Native REST port {} available", ctx.native_rest_port), false, None),
        "firewall" => CheckItem::new(id, "Local firewall allows port (UDP+TCP)", false, None),
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
    let ok = tokio::task::spawn_blocking(check_docker).await.unwrap_or(false);
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
    let state = if ok { CheckState::Pass } else { CheckState::Warn };
    base.with_state(state).with_label(label)
}

#[cfg(not(target_os = "windows"))]
async fn run_check_wsl(ctx: &CheckCtx) -> CheckItem {
    idle_item("wsl", ctx).with_state(CheckState::Skip).with_detail("non-Windows platform")
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
        base.with_state(CheckState::Fail).with_detail(
            "install Docker Desktop, which ships with the `docker compose` CLI plugin",
        )
    }
}

async fn run_check_stack_assets(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("stack-assets", ctx);
    let ok = crate::stack_assets::stack_compose_file().exists()
        && crate::stack_assets::stack_caddyfile().exists();
    if ok {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Warn).with_detail(
            "stack files not staged yet — they'll be written on next Start",
        )
    }
}

async fn run_check_stack_images(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("stack-images", ctx);
    let images = required_stack_images(ctx);
    if images.is_empty() {
        return base.with_state(CheckState::Skip)
            .with_detail("no compose images needed for this profile");
    }
    let missing: Vec<String> = tokio::task::spawn_blocking(move || {
        images
            .into_iter()
            .filter(|img| !docker_image_present(img))
            .collect()
    })
    .await
    .unwrap_or_default();
    if missing.is_empty() {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail)
            .with_detail(format!("missing: {}", missing.join(", ")))
    }
}

async fn run_check_binary(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("binary", ctx);
    let ok = tokio::task::spawn_blocking(crate::native::is_binary_available)
        .await
        .unwrap_or(false);
    if ok {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail).with_detail("run Download & Install")
    }
}

async fn run_check_secret(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("secret", ctx);
    if check_secret_exists() {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail).with_detail("run Generate Secret")
    }
}

async fn run_check_ip(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("ip", ctx);
    match ctx.public_ip().await {
        Some(ip) => base.with_state(CheckState::Pass).with_label(format!("Public IP: {}", ip)),
        None => base.with_state(CheckState::Warn).with_label("Public IP unreachable"),
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

    let state = if passed { CheckState::Pass } else { CheckState::Warn };
    base.with_state(state).with_label(format!("{} accessible to internet", hostname))
}

async fn run_check_port(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("port", ctx);
    let port = ctx.port;
    let (state, label) = match probe_port_forwarding_with_ctx(ctx, port).await {
        PortProbeResult::Verified => (
            CheckState::Pass,
            format!("Port {} verified (host responded)", port),
        ),
        PortProbeResult::QuicHandshakeFailed => (
            CheckState::Warn,
            format!(
                "Port {} node is running but UDP/QUIC timed out \u{2014} check UDP forward + firewall",
                port
            ),
        ),
        PortProbeResult::ForwardReady => (
            CheckState::Pass,
            format!(
                "Port {} TCP forward ready \u{2014} start the node and recheck to verify QUIC",
                port
            ),
        ),
        PortProbeResult::Unreachable => (
            CheckState::Warn,
            format!(
                "Port {} not reachable \u{2014} check router forward + firewall",
                port
            ),
        ),
        PortProbeResult::RateLimited { retry_after_secs, endpoint } => (
            CheckState::Pass,
            format!(
                "Port {} (couldn't verify: rate-limited by check.quip.network via /{} \u{2014} retry in {}s)",
                port, endpoint, retry_after_secs
            ),
        ),
    };
    base.with_state(state).with_label(label)
}

async fn run_check_firewall(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("firewall", ctx);
    let port = ctx.port;
    let (ok, label) = tokio::task::spawn_blocking(move || check_local_firewall(port))
        .await
        .unwrap_or((false, "Firewall check failed".into()));
    let state = if ok { CheckState::Pass } else { CheckState::Warn };
    base.with_state(state).with_label(label)
}

async fn run_check_port_dashboard(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("port-dashboard", ctx);
    // Our own stack holds 20080 when up — a bind failure then is expected,
    // not a conflict. Skip the bind test and pass.
    if ctx.stack_running().await {
        return base.with_state(CheckState::Pass);
    }
    let ok = tokio::task::spawn_blocking(|| tcp_port_bindable(20080))
        .await
        .unwrap_or(false);
    if ok {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Warn).with_detail(
            "TCP 20080 already in use — another service will conflict with the dashboard",
        )
    }
}

async fn run_check_port_tls(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("port-tls", ctx);
    // Our own caddy holds 80/443 when up — ditto port-dashboard.
    if ctx.stack_running().await {
        return base.with_state(CheckState::Pass);
    }
    let (ok_80, ok_443) = tokio::task::spawn_blocking(|| {
        (tcp_port_bindable(80), tcp_port_bindable(443))
    })
    .await
    .unwrap_or((false, false));
    match (ok_80, ok_443) {
        (true, true) => base.with_state(CheckState::Pass),
        (false, true) => base.with_state(CheckState::Warn).with_detail(
            "TCP :80 in use — Caddy's ACME HTTP-01 challenge will fail",
        ),
        (true, false) => base.with_state(CheckState::Warn).with_detail(
            "TCP :443 in use — Caddy cannot serve HTTPS",
        ),
        (false, false) => base.with_state(CheckState::Warn).with_detail(
            "TCP :80 and :443 both in use — free them before enabling TLS",
        ),
    }
}

async fn run_check_rest_port_native(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("rest-port-native", ctx);
    let port = ctx.native_rest_port;
    let ok = tokio::task::spawn_blocking(move || tcp_port_bindable(port))
        .await
        .unwrap_or(false);
    if ok {
        base.with_state(CheckState::Pass).with_detail(
            "Native node binds 127.0.0.1; dashboard reaches it via Docker Desktop's host.docker.internal",
        )
    } else {
        base.with_state(CheckState::Warn)
            .with_detail(format!("TCP {} already in use", port))
    }
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

async fn run_check_version(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("version", ctx);
    match ctx.run_mode {
        RunMode::Docker => {
            match crate::update::check_image_update(ctx.image_tag).await {
                Ok(Some(info)) if info.update_available => base
                    .with_state(CheckState::Warn)
                    .with_label("Node image outdated \u{2014} pull latest"),
                Ok(_) => base.with_state(CheckState::Pass).with_label("Node image up to date"),
                Err(e) => base
                    .with_state(CheckState::Warn)
                    .with_label("Node version (unable to check)")
                    .with_detail(e),
            }
        }
        RunMode::Native => match crate::native::check_binary_update().await {
            Ok(Some(info)) => base
                .with_state(CheckState::Warn)
                .with_label(format!("Node outdated \u{2014} v{} available", info.version)),
            Ok(None) => base.with_state(CheckState::Pass).with_label("Node binary up to date"),
            Err(e) => base
                .with_state(CheckState::Warn)
                .with_label("Node version (unable to check)")
                .with_detail(e),
        },
    }
}

/// Dispatch by id. Unknown ids return a Skip item.
async fn run_check_by_id(id: &str, ctx: &CheckCtx) -> CheckItem {
    match id {
        "docker" => run_check_docker(ctx).await,
        "docker-compose" => run_check_docker_compose(ctx).await,
        "stack-assets" => run_check_stack_assets(ctx).await,
        "wsl" => run_check_wsl(ctx).await,
        "stack-images" => run_check_stack_images(ctx).await,
        "binary" => run_check_binary(ctx).await,
        "secret" => run_check_secret(ctx).await,
        "ip" => run_check_ip(ctx).await,
        "hostname" => run_check_hostname(ctx).await,
        "port" => run_check_port(ctx).await,
        "port-dashboard" => run_check_port_dashboard(ctx).await,
        "port-tls" => run_check_port_tls(ctx).await,
        "rest-port-native" => run_check_rest_port_native(ctx).await,
        "firewall" => run_check_firewall(ctx).await,
        "version" => run_check_version(ctx).await,
        "dwave-key" => run_check_dwave_key(ctx).await,
        _ => idle_item(id, ctx).with_state(CheckState::Skip).with_detail("unknown check id"),
    }
}

// ─── Event emission ───────────────────────────────────────────────────────────

fn emit_item(app: &AppHandle, item: &CheckItem) {
    let _ = app.emit("checklist-update", item);
}

/// Append a `[checklist]` entry to the node-log so the console shows every
/// state transition alongside the node's own output.
fn emit_log(app: &AppHandle, auto: bool, verb: &str, item: &CheckItem, level: &str) {
    let prefix = if auto { "[checklist] [auto] " } else { "[checklist] " };
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
        let base = cache
            .get(id)
            .cloned()
            .unwrap_or_else(|| idle_item(id, ctx));
        let running = base.with_state(CheckState::Running);
        cache.insert(id.to_string(), running.clone());
        running
    };
    emit_item(app, &running);
    emit_log(app, auto, verb_for_state(&running.state).0, &running, verb_for_state(&running.state).1);

    // Run the check.
    let final_item = run_check_by_id(id, ctx).await;

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
    let native_rest_port =
        crate::compose::native_rest_port(&settings.node_config);
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
        public_host: settings.node_config.public_host,
        dashboard_enabled: settings.dashboard_enabled,
        tls_enabled: settings.tls_enabled,
        native_rest_port,
        has_dwave_config,
        dwave_token_set,
        app: None,
        public_ip: OnceCell::new(),
        stack_running: OnceCell::new(),
    };
    let mut results = Vec::new();
    for id in visible_ids(&ctx) {
        results.push(run_check_by_id(&id, &ctx).await);
    }
    results
}

/// Convenience for TUI port-only recheck. Returns a plain bool since the
/// TUI doesn't render the richer four-state diagnostic the GUI uses.
pub async fn probe_port_forwarding_with_default_ip(port: u16) -> bool {
    probe_port_forwarding(port).await.is_externally_reachable()
}
