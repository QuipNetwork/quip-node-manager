// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{ContainerStatus, NodeConfig};
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
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!("docker pull failed: {}", stderr))
    }
}

#[tauri::command]
pub async fn start_node_container(
    config: NodeConfig,
    image_tag: String,
) -> Result<String, String> {
    crate::config::write_config_toml(&config)?;

    let home =
        dirs::home_dir().ok_or("cannot determine home directory")?;
    let data_dir = home.join("quip-data");
    let data_mount = format!("{}:/data", data_dir.display());
    let config_mount = format!(
        "{}/config.toml:/config/config.toml:ro",
        data_dir.display()
    );
    let image = format!("{}:latest", image_for_tag(&image_tag));

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        "quip-node".to_string(),
        "-p".to_string(),
        "20049:20049/udp".to_string(),
        "-v".to_string(),
        data_mount,
        "-v".to_string(),
        config_mount,
    ];

    if image_tag == "cuda" {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    args.push(image);
    args.push("network-node".to_string());
    args.push("--config".to_string());
    args.push("/config/config.toml".to_string());
    args.push("cpu".to_string());
    args.push("--num-cpus".to_string());
    args.push(config.num_cpus.to_string());

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
    let output = Command::new("docker")
        .args(["rm", "-f", "quip-node"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Ok(())
    }
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
        let running = parts[1] == "true";
        Ok(ContainerStatus {
            running,
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
