// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use serde_json::Value;
use std::net::UdpSocket;
use std::time::Duration;
use tauri::Emitter;

use crate::settings::{data_dir, RunMode};

#[derive(Serialize, Clone, Debug)]
pub struct CheckItem {
    pub id: String,
    pub passed: bool,
    pub label: String,
}

fn check_docker() -> bool {
    crate::cmd::new("docker")
        .args(["info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_image_present() -> bool {
    let cpu_image =
        "registry.gitlab.com/piqued/quip-protocol/quip-network-node-cpu:latest";
    crate::cmd::new("docker")
        .args(["image", "inspect", cpu_image])
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

// ─── Public IP ────────────────────────────────────────────────────────────────

async fn fetch_public_ip() -> Option<String> {
    // Try check.quip.network /ip first
    if let Some(ip) = fetch_ip_check_service().await {
        return Some(ip);
    }
    // Fallback: api.ipify.org
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
    if ip.is_empty() { None } else { Some(ip) }
}

async fn fetch_ip_ipify() -> Option<String> {
    let client = make_client(10)?;
    let resp = client
        .get("https://api.ipify.org")
        .send()
        .await
        .ok()?;
    let text = resp.text().await.ok()?;
    let ip = text.trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

// ─── Port check ───────────────────────────────────────────────────────────────

/// Bind a TCP listener on `port` and wait up to 15 seconds for any inbound
/// connection (netcat-style: `nc -l <port>`). Returns true the moment a
/// connection is accepted, false on timeout or bind error.
///
/// This is intentionally passive — it does not send any outbound probes.
/// The caller (or the user via an external tool) is expected to initiate the
/// connection from outside the network.
/// Check port forwarding by connecting to our own public IP (hairpin NAT).
///
/// Binds a temporary listener on the port (so there is something to connect to
/// even when the node is stopped), fetches our public IP, then tries to connect
/// to `public_ip:port` from this machine. If the connection is accepted the
/// router is doing hairpin NAT and the port is forwarded.
///
/// When the port is already occupied (node running), skips binding and connects
/// directly — the node itself will accept and immediately see the client
/// disconnect, which is harmless.
pub async fn probe_port_forwarding(port: u16) -> bool {
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};

    let public_ip = match fetch_public_ip().await {
        Some(ip) => ip,
        None => return false,
    };
    let addr = format!("{}:{}", public_ip, port);

    // Try to bind a listener. If the port is already in use (node running) we
    // skip binding and just attempt the outbound connect — the node will accept.
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await.ok();

    if let Some(listener) = listener {
        // Concurrently connect outbound and accept inbound.
        let connect_fut =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr));
        let accept_fut =
            tokio::time::timeout(Duration::from_secs(6), listener.accept());

        let (conn, acc) = tokio::join!(connect_fut, accept_fut);
        conn.map(|r| r.is_ok()).unwrap_or(false) && acc.map(|r| r.is_ok()).unwrap_or(false)
    } else {
        // Node already running on this port — just try the outbound connect.
        matches!(
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr)).await,
            Ok(Ok(_))
        )
    }
}

#[tauri::command]
pub async fn recheck_port_forwarding(port: u16) -> Result<bool, String> {
    Ok(probe_port_forwarding(port).await)
}

// ─── Hostname check ───────────────────────────────────────────────────────────

// Checks whether the hostname resolves to this machine's public IP.
// Only called for custom hostnames (not bare IPs).
// Returns None if the service is unavailable (caller should fall back).
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

// ─── Local firewall check ─────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn os_firewall_check(port: u16) -> Option<(bool, String)> {
    let out = crate::cmd::new("/usr/libexec/ApplicationFirewall/socketfilterfw")
        .arg("--getglobalstate")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    if text.contains("disabled") || text.contains("state = 0") {
        Some((true, format!("macOS Firewall: Port {} open", port)))
    } else if text.contains("enabled") || text.contains("state = 1") {
        Some((
            true,
            format!("macOS Firewall: Port {} open (check Docker allowed)", port),
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
        let port_udp = format!("{}/udp", port);
        if text.contains(&port_udp) {
            return Some((true, format!("ufw allows {}/udp", port)));
        }
        return Some((
            false,
            format!("ufw active \u{2014} run: sudo ufw allow {}/udp", port),
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
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("localport={}", port),
            "dir=in",
            "protocol=udp",
        ])
        .output()
        .ok()?;
    let rule_text = String::from_utf8_lossy(&rule.stdout);
    if rule_text.to_lowercase().contains("allow") {
        Some((true, format!("Windows Firewall allows {}/udp", port)))
    } else {
        Some((
            false,
            format!(
                "Windows Firewall may block {}/udp \u{2014} add inbound UDP rule",
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
    let addr: std::net::SocketAddr =
        format!("0.0.0.0:{}", port).parse().unwrap();
    match UdpSocket::bind(addr) {
        Ok(_) => (true, format!("Port {}/udp bindable locally", port)),
        Err(e) => (false, format!("Cannot bind {}/udp: {}", port, e)),
    }
}

// ─── Checklist runner ─────────────────────────────────────────────────────────

pub async fn run_checklist_core<F>(
    run_mode: &RunMode,
    on_progress: F,
) -> Vec<CheckItem>
where
    F: Fn(&[CheckItem]) + Send,
{
    let mut checks = Vec::new();

    match run_mode {
        RunMode::Docker => {
            // 1. Docker
            checks.push(CheckItem {
                id: "docker".to_string(),
                passed: check_docker(),
                label: "Docker installed & running".to_string(),
            });
            on_progress(&checks);

            // 2. Image
            checks.push(CheckItem {
                id: "image".to_string(),
                passed: check_image_present(),
                label: "Node image available".to_string(),
            });
            on_progress(&checks);
        }
        RunMode::Native => {
            // 1. Binary available
            checks.push(CheckItem {
                id: "binary".to_string(),
                passed: crate::native::is_binary_available(),
                label: "Node binary available".to_string(),
            });
            on_progress(&checks);
        }
    }

    // 3. Secret
    checks.push(CheckItem {
        id: "secret".to_string(),
        passed: check_secret_exists(),
        label: "Node secret configured".to_string(),
    });
    on_progress(&checks);

    // 4. Public IP
    let ip_opt = fetch_public_ip().await;
    let ip_passed = ip_opt.is_some();
    let ip_label = match &ip_opt {
        Some(ip) => format!("Public IP: {}", ip),
        None => "Public IP unreachable".to_string(),
    };
    checks.push(CheckItem {
        id: "ip".to_string(),
        passed: ip_passed,
        label: ip_label,
    });
    on_progress(&checks);

    // 5. Hostname accessible to internet
    let config = crate::settings::load_settings().node_config;
    let port = config.port;
    let (hostname, host_passed) = if !config.public_host.is_empty() {
        let host = &config.public_host;
        let host_only = host.split(':').next().unwrap_or(host);
        let passed = if host_only.parse::<std::net::IpAddr>().is_ok() {
            ip_passed
        } else {
            match check_hostname_dns(host_only).await {
                Some(matched) => matched,
                None => ip_passed,
            }
        };
        (host.clone(), passed)
    } else {
        let ip = ip_opt.as_deref().unwrap_or("unknown").to_string();
        (ip, ip_passed)
    };
    checks.push(CheckItem {
        id: "hostname".to_string(),
        passed: host_passed,
        label: format!("{} accessible to internet", hostname),
    });
    on_progress(&checks);

    // 6. Port forwarding
    let port_ok = probe_port_forwarding(port).await;
    checks.push(CheckItem {
        id: "port".to_string(),
        passed: port_ok,
        label: if port_ok {
            format!("Port {} forwarded", port)
        } else {
            format!("Port {} not reachable from internet", port)
        },
    });
    on_progress(&checks);

    // 7. Local firewall
    let (fw_passed, fw_label) = check_local_firewall(port);
    checks.push(CheckItem {
        id: "firewall".to_string(),
        passed: fw_passed,
        label: fw_label,
    });
    on_progress(&checks);

    checks
}

#[tauri::command]
pub async fn run_checklist(app: tauri::AppHandle) -> Result<Vec<CheckItem>, String> {
    let settings = crate::settings::load_settings();
    let run_mode = settings.run_mode;
    Ok(run_checklist_core(&run_mode, |checks| {
        let _ = app.emit("checklist-update", checks);
    })
    .await)
}
