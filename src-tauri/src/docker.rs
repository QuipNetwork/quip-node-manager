// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{ContainerStatus, GpuBackend, RunMode};
use tauri::Emitter;

fn log_cmd(app: &tauri::AppHandle, cmd: &str) {
    let entry = serde_json::json!({
        "timestamp": "",
        "level": "INFO",
        "message": format!("$ {}", cmd),
    });
    let _ = app.emit("node-log", entry);
}

fn log_output(app: &tauri::AppHandle, text: &str) {
    for line in text.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

fn log_err(app: &tauri::AppHandle, text: &str) {
    for line in text.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "ERROR",
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

const CPU_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu";
const CUDA_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cuda";

pub fn image_for_tag(image_tag: &str) -> &'static str {
    if image_tag == "cuda" {
        CUDA_IMAGE
    } else {
        CPU_IMAGE
    }
}

#[tauri::command]
pub async fn check_docker_installed() -> Result<bool, String> {
    let status = crate::cmd::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(status.status.success())
}

#[tauri::command]
pub async fn check_docker_hello_world() -> Result<bool, String> {
    let status = crate::cmd::new("docker")
        .args(["run", "--rm", "hello-world"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(status.status.success())
}

#[tauri::command]
pub async fn pull_node_image(
    app: tauri::AppHandle,
    image_tag: String,
) -> Result<String, String> {
    let image = format!("{}:latest", image_for_tag(&image_tag));
    log_cmd(&app, &format!("docker pull {}", image));
    let output = crate::cmd::new("docker")
        .args(["pull", &image])
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stdout.trim().is_empty() {
        log_output(&app, stdout.trim());
    }
    if output.status.success() {
        Ok(stdout)
    } else {
        log_err(&app, stderr.trim());
        Err(format!("docker pull failed: {}", stderr))
    }
}

/// Host uid/gid for the PUID/PGID env vars passed to the node container.
///
/// The node image's entrypoint (quip-protocol v0.1.7+) uses these to chown
/// `/data` so bind-mounted files are owned by the host user, not root.
/// On Windows, Docker Desktop's VM has no meaningful mapping to Windows
/// users — 1000:1000 matches the compose recipe's default.
fn host_uid_gid() -> (u32, u32) {
    #[cfg(unix)]
    {
        // SAFETY: getuid/getgid take no arguments and cannot fail per POSIX.
        unsafe { (libc::getuid(), libc::getgid()) }
    }
    #[cfg(not(unix))]
    {
        (1000, 1000)
    }
}

#[tauri::command]
pub async fn start_node_container(
    app: tauri::AppHandle,
) -> Result<String, String> {
    let settings = crate::settings::load_settings();
    let mut config = settings.node_config;
    let image_tag = settings.image_tag;

    // Auto-detect public IP when no public_host is configured
    if config.public_host.is_empty() {
        if let Ok(ip) = crate::network::detect_public_ip().await {
            log_cmd(&app, &format!("Auto-detected public IP: {}", ip));
            config.public_host = ip;
        }
    }

    // Write config.toml before starting
    log_cmd(&app, "Writing config.toml");
    crate::config::write_config_toml(&config, &RunMode::Docker)?;

    // Remove any stale container first
    log_cmd(&app, "docker rm -f quip-node");
    let rm_out = crate::cmd::new("docker")
        .args(["rm", "-f", "quip-node"])
        .output();
    if let Ok(o) = &rm_out {
        let stderr = String::from_utf8_lossy(&o.stderr);
        if !stderr.trim().is_empty()
            && !stderr.contains("No such container")
        {
            log_output(&app, stderr.trim());
        }
    }

    // Always pull latest image before starting (cache-bust :latest)
    pull_node_image(app.clone(), image_tag.clone()).await?;

    // Honor the user's custom storage directory (bootstrap.json).
    // Ensure it exists before Docker tries to bind-mount it, otherwise
    // the daemon silently creates a root-owned directory on the host.
    crate::settings::ensure_data_dir()?;
    let data_dir = crate::settings::data_dir();
    let data_mount = format!("{}:/data", data_dir.display());
    let image = format!("{}:latest", image_for_tag(&image_tag));

    let enabled_devices: Vec<u32> = config
        .gpu_device_configs
        .iter()
        .filter(|d| d.enabled)
        .map(|d| d.index)
        .collect();

    // MPS (Metal) is not accessible from Docker — runs in a Linux VM.
    let use_gpu = config.gpu_backend == GpuBackend::Local
        && !enabled_devices.is_empty()
        && image_tag == "cuda";

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        "quip-node".to_string(),
        "-p".to_string(),
        format!("{}:{}/udp", config.port, config.port),
        "-p".to_string(),
        format!("{}:{}/tcp", config.port, config.port),
        "-v".to_string(),
        data_mount,
    ];

    // Pass host uid/gid so the entrypoint chowns /data to the host user.
    // Matches the compose recipe's PUID/PGID env scheme (nodes.quip.network).
    let (puid, pgid) = host_uid_gid();
    args.push("-e".to_string());
    args.push(format!("PUID={}", puid));
    args.push("-e".to_string());
    args.push(format!("PGID={}", pgid));

    if !config.public_host.is_empty() {
        args.push("-e".to_string());
        args.push(format!(
            "QUIP_PUBLIC_HOST={}",
            config.public_host
        ));
    }
    if !config.node_name.is_empty() {
        args.push("-e".to_string());
        args.push(format!("QUIP_NODE_NAME={}", config.node_name));
    }

    if use_gpu {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    args.push(image);

    log_cmd(&app, &format!("docker {}", args.join(" ")));

    let output = crate::cmd::new("docker")
        .args(&args)
        .output()
        .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        let cid = stdout.trim();
        log_output(
            &app,
            &format!("Container started: {}", &cid[..12.min(cid.len())]),
        );
        Ok(cid.to_string())
    } else {
        log_err(&app, stderr.trim());
        Err(stderr.trim().to_string())
    }
}

#[tauri::command]
pub async fn stop_node_container(
    app: tauri::AppHandle,
) -> Result<(), String> {
    log_cmd(&app, "docker stop quip-node");
    let stop = crate::cmd::new("docker")
        .args(["stop", "quip-node"])
        .output();
    if let Ok(o) = &stop {
        if !o.status.success() {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.trim().is_empty() {
                log_err(&app, stderr.trim());
            }
        }
    }

    log_cmd(&app, "docker rm -f quip-node");
    crate::cmd::new("docker")
        .args(["rm", "-f", "quip-node"])
        .output()
        .map_err(|e| e.to_string())?;
    log_output(&app, "Container removed.");
    Ok(())
}

#[tauri::command]
pub async fn get_container_status() -> Result<ContainerStatus, String> {
    let output = tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args([
                "inspect",
                "--format",
                "{{.Id}}\t{{.State.Running}}\t{{.Config.Image}}\t{{.State.Status}}",
                "quip-node",
            ])
            .output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Ok(ContainerStatus {
            running: false,
            container_id: None,
            image: String::new(),
            status_text: "not found".to_string(),
        });
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = line.trim().split('\t').collect();
    if parts.len() >= 4 {
        Ok(ContainerStatus {
            running: parts[1] == "true",
            container_id: Some(
                parts[0][..12.min(parts[0].len())].to_string(),
            ),
            image: parts[2].to_string(),
            status_text: parts[3].to_string(),
        })
    } else {
        Ok(ContainerStatus {
            running: false,
            container_id: None,
            image: String::new(),
            status_text: "unknown".to_string(),
        })
    }
}

#[tauri::command]
pub async fn get_container_config() -> Result<String, String> {
    let output = crate::cmd::new("docker")
        .args(["inspect", "quip-node"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}
