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
    pub image_tag: String,
    pub port: u16,
    pub public_host: String,
    public_ip: OnceCell<Option<String>>,
}

impl CheckCtx {
    fn from_settings() -> Self {
        let settings = crate::settings::load_settings();
        CheckCtx {
            run_mode: settings.run_mode,
            image_tag: settings.image_tag,
            port: settings.node_config.port,
            public_host: settings.node_config.public_host,
            public_ip: OnceCell::new(),
        }
    }

    /// Memoised public-IP fetch. First caller pays the network cost;
    /// subsequent callers in the same recheck batch reuse the result.
    async fn public_ip(&self) -> Option<String> {
        self.public_ip
            .get_or_init(fetch_public_ip)
            .await
            .clone()
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

fn check_image_present(image_tag: &str) -> bool {
    let image = format!("{}:latest", crate::docker::image_for_tag(image_tag));
    crate::cmd::new("docker")
        .args(["image", "inspect", &image])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

/// Port forwarding check. Tries hairpin NAT first (fast when router supports
/// it), then falls back to asking check.quip.network to probe us from outside.
pub async fn probe_port_forwarding(port: u16, public_ip: Option<String>) -> bool {
    let ip = match public_ip {
        Some(ip) => ip,
        None => return false,
    };
    if probe_hairpin(port, &ip).await {
        return true;
    }
    probe_external(port).await
}

async fn probe_hairpin(port: u16, public_ip: &str) -> bool {
    use tokio::net::{TcpListener, TcpStream};

    let addr = format!("{}:{}", public_ip, port);
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await.ok();

    if let Some(listener) = listener {
        let connect_fut =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr));
        let accept_fut =
            tokio::time::timeout(Duration::from_secs(6), listener.accept());
        let (conn, acc) = tokio::join!(connect_fut, accept_fut);
        conn.map(|r| r.is_ok()).unwrap_or(false)
            && acc.map(|r| r.is_ok()).unwrap_or(false)
    } else {
        // Port in use (node running) — just attempt outbound connect.
        matches!(
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr)).await,
            Ok(Ok(_))
        )
    }
}

async fn probe_external(port: u16) -> bool {
    let client = match make_client(10) {
        Some(c) => c,
        None => return false,
    };
    let url = format!("{}/checkport?port={}", CHECK_SERVICE, port);
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };
    let json: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return false,
    };
    json["reachable"].as_bool().unwrap_or(false)
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

/// IDs of all checks in render order. Filter by mode with `visible_for_mode`.
pub const ALL_CHECK_IDS: &[&str] = &[
    "docker", "wsl", "image", "binary", "version", "secret", "ip", "hostname", "port",
    "firewall",
];

pub fn visible_for_mode(id: &str, run_mode: &RunMode) -> bool {
    match (id, run_mode) {
        ("docker", RunMode::Docker) => true,
        ("wsl", RunMode::Docker) => cfg!(target_os = "windows"),
        ("image", RunMode::Docker) => true,
        ("binary", RunMode::Native) => true,
        // Common to both modes
        ("version", _) | ("secret", _) | ("ip", _) | ("hostname", _) | ("port", _)
        | ("firewall", _) => true,
        _ => false,
    }
}

pub fn visible_ids(run_mode: &RunMode) -> Vec<String> {
    ALL_CHECK_IDS
        .iter()
        .filter(|id| visible_for_mode(id, run_mode))
        .map(|s| s.to_string())
        .collect()
}

/// Fresh Idle placeholder for the given id. Used to seed the cache on
/// startup and on mode switch, so the UI can render the full list before
/// any check has run.
fn idle_item(id: &str, ctx: &CheckCtx) -> CheckItem {
    match id {
        "docker" => CheckItem::new(id, "Docker installed & running", true, Some(FixKind::InstallDocker)),
        "wsl" => CheckItem::new(id, "WSL installed with distro", false, None),
        "image" => CheckItem::new(id, "Node image available", true, Some(FixKind::PullImage)),
        "binary" => CheckItem::new(id, "Node binary available", true, Some(FixKind::DownloadBinary)),
        "version" => CheckItem::new(id, "Node version up to date", false, Some(FixKind::Delegate(match ctx.run_mode {
            RunMode::Docker => "image".into(),
            RunMode::Native => "binary".into(),
        }))),
        "secret" => CheckItem::new(id, "Node secret configured", true, Some(FixKind::GenerateSecret)),
        "ip" => CheckItem::new(id, "Public IP reachable", false, None),
        "hostname" => CheckItem::new(id, "Hostname accessible to internet", false, None),
        "port" => CheckItem::new(id, &format!("Port {} — press Recheck to test", ctx.port), false, None),
        "firewall" => CheckItem::new(id, "Local firewall allows port (UDP+TCP)", false, None),
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

async fn run_check_image(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("image", ctx);
    let image_tag = ctx.image_tag.clone();
    let ok = tokio::task::spawn_blocking(move || check_image_present(&image_tag))
        .await
        .unwrap_or(false);
    if ok {
        base.with_state(CheckState::Pass)
    } else {
        base.with_state(CheckState::Fail).with_detail("run Pull Image to download")
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
    let ip_opt = ctx.public_ip().await;
    let port = ctx.port;
    let ok = probe_port_forwarding(port, ip_opt).await;
    let (state, label) = if ok {
        (
            CheckState::Pass,
            format!("Port {} forwarded (ensure both UDP+TCP on router)", port),
        )
    } else {
        (
            CheckState::Warn,
            format!("Port {} not reachable \u{2014} forward UDP+TCP on router", port),
        )
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

async fn run_check_version(ctx: &CheckCtx) -> CheckItem {
    let base = idle_item("version", ctx);
    match ctx.run_mode {
        RunMode::Docker => {
            match crate::update::check_image_update(ctx.image_tag.clone()).await {
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
        "wsl" => run_check_wsl(ctx).await,
        "image" => run_check_image(ctx).await,
        "binary" => run_check_binary(ctx).await,
        "secret" => run_check_secret(ctx).await,
        "ip" => run_check_ip(ctx).await,
        "hostname" => run_check_hostname(ctx).await,
        "port" => run_check_port(ctx).await,
        "firewall" => run_check_firewall(ctx).await,
        "version" => run_check_version(ctx).await,
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
    for id in visible_ids(&ctx.run_mode) {
        cache.insert(id.clone(), idle_item(&id, ctx));
    }
}

/// Shared implementation between the `recheck` Tauri command and the
/// `trigger_recheck_auto` helper used by docker/native action handlers.
async fn run_recheck(app: AppHandle, ids: Option<Vec<String>>, auto: bool) -> Result<(), String> {
    let state: tauri::State<'_, ChecklistState> = app.state();
    let ctx = Arc::new(CheckCtx::from_settings());

    let ids = match ids {
        Some(ids) if !ids.is_empty() => ids,
        _ => {
            // Global recheck: seed the cache for the current mode, then run all.
            seed_cache(&state, &ctx).await;
            visible_ids(&ctx.run_mode)
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
    let settings = crate::settings::load_settings();
    let ids = visible_ids(&settings.run_mode);
    Ok(ids
        .into_iter()
        .map(|id| {
            cache
                .get(&id)
                .cloned()
                .unwrap_or_else(|| idle_item(&id, &CheckCtx::from_settings()))
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
    let ctx = CheckCtx {
        run_mode: run_mode.clone(),
        image_tag: settings.image_tag,
        port: settings.node_config.port,
        public_host: settings.node_config.public_host,
        public_ip: OnceCell::new(),
    };
    let mut results = Vec::new();
    for id in visible_ids(run_mode) {
        results.push(run_check_by_id(&id, &ctx).await);
    }
    results
}

/// Convenience for TUI port-only recheck: fetches the public IP internally.
pub async fn probe_port_forwarding_with_default_ip(port: u16) -> bool {
    let ip = fetch_public_ip().await;
    probe_port_forwarding(port, ip).await
}
