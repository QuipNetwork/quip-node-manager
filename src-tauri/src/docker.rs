// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{ContainerStatus, GpuBackend, NodeConfig};
use serde::Serialize;
use std::process::Command;

const CPU_IMAGE: &str =
    "registry.gitlab.com/piqued/quip-protocol/quip-network-node-cpu";
const CUDA_IMAGE: &str =
    "registry.gitlab.com/piqued/quip-protocol/quip-network-node-cuda";

pub fn image_for_tag(image_tag: &str) -> &'static str {
    if image_tag == "cuda" {
        CUDA_IMAGE
    } else {
        CPU_IMAGE
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
}

#[tauri::command]
pub async fn list_gpu_devices() -> Result<Vec<GpuDevice>, String> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=index,name", "--format=csv,noheader"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            Ok(text
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ',');
                    let index = parts.next()?.trim().parse().ok()?;
                    let name = parts.next()?.trim().to_string();
                    Some(GpuDevice { index, name })
                })
                .collect())
        }
        _ => Ok(vec![]),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_apple_silicon() -> bool {
    true
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn is_apple_silicon() -> bool {
    false
}

#[tauri::command]
pub async fn detect_gpu_backend() -> Result<String, String> {
    let has_nvidia = Command::new("nvidia-smi")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_nvidia {
        return Ok("local".to_string());
    }
    if is_apple_silicon() {
        return Ok("mps".to_string());
    }
    Ok("none".to_string())
}

#[tauri::command]
pub async fn check_docker_installed() -> Result<bool, String> {
    let status = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(status.status.success())
}

#[tauri::command]
pub async fn check_docker_hello_world() -> Result<bool, String> {
    let status = Command::new("docker")
        .args(["run", "--rm", "hello-world"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(status.status.success())
}

#[tauri::command]
pub async fn pull_node_image(
    image_tag: String,
) -> Result<String, String> {
    let image = format!("{}:latest", image_for_tag(&image_tag));
    let output = Command::new("docker")
        .args(["pull", &image])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "docker pull failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

#[tauri::command]
pub async fn start_node_container(
    config: NodeConfig,
    image_tag: String,
) -> Result<String, String> {
    // Write config.toml before starting — entrypoint mounts /data/config.toml
    crate::config::write_config_toml(&config)?;

    let home =
        dirs::home_dir().ok_or("cannot determine home directory")?;
    let data_dir = home.join("quip-data");
    let data_mount = format!("{}:/data", data_dir.display());
    let image = format!("{}:latest", image_for_tag(&image_tag));

    let enabled_devices: Vec<u32> = config
        .gpu_device_configs
        .iter()
        .filter(|d| d.enabled)
        .map(|d| d.index)
        .collect();

    // MPS (Metal) is not accessible from Docker — runs in a Linux VM on macOS.
    // The MPS native sidecar path is handled separately. For Docker, treat as CPU.
    let use_gpu = config.gpu_backend == GpuBackend::Local
        && !enabled_devices.is_empty()
        && image_tag == "cuda";

    // QUIP_MODE is read by entrypoint.sh to select the subcommand (cpu/gpu)
    let quip_mode = if use_gpu { "gpu" } else { "cpu" };

    // Peers: pass as comma-separated QUIP_PEERS; entrypoint converts to --peer flags
    let quip_peers = if config.peers.is_empty() {
        // Let entrypoint use its built-in defaults
        String::new()
    } else {
        config.peers.join(",")
    };

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        "quip-node".to_string(),
        "-p".to_string(),
        format!("{}:{}/udp", config.port, config.port),
        "-v".to_string(),
        data_mount,
        // Environment variables consumed by entrypoint.sh
        "-e".to_string(),
        format!("QUIP_MODE={}", quip_mode),
        "-e".to_string(),
        format!("QUIP_PORT={}", config.port),
        "-e".to_string(),
        format!("QUIP_LISTEN={}", config.listen),
        "-e".to_string(),
        format!("QUIP_AUTO_MINE={}", config.auto_mine),
    ];

    if !quip_peers.is_empty() {
        args.push("-e".to_string());
        args.push(format!("QUIP_PEERS={}", quip_peers));
    }
    if !config.public_host.is_empty() {
        args.push("-e".to_string());
        args.push(format!("QUIP_PUBLIC_HOST={}", config.public_host));
    }
    if !config.node_name.is_empty() {
        args.push("-e".to_string());
        args.push(format!("QUIP_NODE_NAME={}", config.node_name));
    }

    // For CUDA: expose all selected devices
    if use_gpu {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    args.push(image);
    // No CMD args — entrypoint.sh manages the quip-network-node invocation

    let output = Command::new("docker")
        .args(&args)
        .output()
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

#[tauri::command]
pub async fn stop_node_container() -> Result<(), String> {
    let _ =
        Command::new("docker").args(["stop", "quip-node"]).output();
    Command::new("docker")
        .args(["rm", "-f", "quip-node"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_container_status() -> Result<ContainerStatus, String> {
    let output = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.Id}}\t{{.State.Running}}\t{{.Config.Image}}\t{{.State.Status}}",
            "quip-node",
        ])
        .output()
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
    let output = Command::new("docker")
        .args(["inspect", "quip-node"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}
