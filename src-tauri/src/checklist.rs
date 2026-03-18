// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::Serialize;
use std::process::Command;
use std::time::Duration;
use tauri::Emitter;

use crate::settings::data_dir;

#[derive(Serialize, Clone, Debug)]
pub struct CheckItem {
    pub id: String,
    pub passed: bool,
    pub label: String,
}

fn check_docker() -> bool {
    Command::new("docker")
        .args(["info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_image_present() -> bool {
    let cpu_image = "registry.gitlab.com/piqued/quip-protocol/quip-network-node-cpu:latest";
    Command::new("docker")
        .args(["image", "inspect", cpu_image])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_secret_exists() -> bool {
    data_dir().join("node-secret.json").exists()
}

fn check_config_exists() -> bool {
    data_dir().join("config.toml").exists()
}

async fn check_public_ip() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get("https://ifconfig.co/ip")
        .header("Accept", "text/plain")
        .send()
        .await
        .is_ok()
}

async fn check_port_open() -> bool {
    // Get public IP first, then try to connect to port 20049
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let ip = match client
        .get("https://ifconfig.co/ip")
        .header("Accept", "text/plain")
        .send()
        .await
    {
        Ok(r) => match r.text().await {
            Ok(t) => t.trim().to_string(),
            Err(_) => return false,
        },
        Err(_) => return false,
    };

    let addr = format!("{}:20049", ip);
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

#[tauri::command]
pub async fn run_checklist(app: tauri::AppHandle) -> Result<Vec<CheckItem>, String> {
    let mut checks = Vec::new();

    macro_rules! emit_check {
        ($checks:expr, $app:expr) => {
            let _ = $app.emit("checklist-update", &$checks);
        };
    }

    // 1. Docker
    checks.push(CheckItem {
        id: "docker".to_string(),
        passed: check_docker(),
        label: "Docker installed & running".to_string(),
    });
    emit_check!(checks, app);

    // 2. Image
    checks.push(CheckItem {
        id: "image".to_string(),
        passed: check_image_present(),
        label: "Node image available".to_string(),
    });
    emit_check!(checks, app);

    // 3. Secret
    checks.push(CheckItem {
        id: "secret".to_string(),
        passed: check_secret_exists(),
        label: "Node secret configured".to_string(),
    });
    emit_check!(checks, app);

    // 4. Config
    checks.push(CheckItem {
        id: "config".to_string(),
        passed: check_config_exists(),
        label: "Config file generated".to_string(),
    });
    emit_check!(checks, app);

    // 5. IP
    checks.push(CheckItem {
        id: "ip".to_string(),
        passed: check_public_ip().await,
        label: "Public IP reachable".to_string(),
    });
    emit_check!(checks, app);

    // 6. Port
    checks.push(CheckItem {
        id: "port".to_string(),
        passed: check_port_open().await,
        label: "Port 20049 forwarded".to_string(),
    });
    emit_check!(checks, app);

    Ok(checks)
}
