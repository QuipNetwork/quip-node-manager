// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, ContainerStatus, GpuBackend, RunMode};
use std::process::Output;
use std::time::Duration;
use tauri::Emitter;

const NETWORK_NAME: &str = "quip-node-manager";
const CPU_CONTAINER: &str = "quip-cpu";
const CUDA_CONTAINER: &str = "quip-cuda";
const MINER_ALIAS: &str = "quip-miner";
const VALIDATOR_CONTAINER: &str = "quip-validator";
const BOOTSTRAP_CONTAINER: &str = "quip-bootstrap";
const CADDY_CONTAINER: &str = "quip-caddy";
const LEGACY_CONTAINER: &str = "quip-node";

const MINER_TAG: &str = "v0.2-preview";
const VALIDATOR_TAG: &str = "v0.2-preview";
const CADDY_TAG: &str = "2-alpine";
const FAUCET_URL: &str = "https://faucet.testnet.quip.network";

const CPU_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cpu";
const CUDA_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda";
const VALIDATOR_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol-rs/quip-network-node";
const CADDY_IMAGE: &str = "caddy";

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

pub fn image_for_tag(image_tag: &str) -> &'static str {
    if image_tag == "cuda" {
        CUDA_IMAGE
    } else {
        CPU_IMAGE
    }
}

pub fn image_ref_for_tag(image_tag: &str) -> String {
    format!("{}:{}", image_for_tag(image_tag), MINER_TAG)
}

pub fn bootstrap_image_ref() -> String {
    // Upstream packages the bootstrap CLI in the CPU miner image. In native
    // mode this is only a short-lived sidecar; mining still runs via Metal.
    image_ref_for_tag("cpu")
}

pub fn validator_image_ref() -> String {
    format!("{}:{}", VALIDATOR_IMAGE, VALIDATOR_TAG)
}

pub fn caddy_image_ref() -> String {
    format!("{}:{}", CADDY_IMAGE, CADDY_TAG)
}

pub fn miner_container_for_tag(image_tag: &str) -> &'static str {
    if image_tag == "cuda" {
        CUDA_CONTAINER
    } else {
        CPU_CONTAINER
    }
}

fn managed_containers() -> [&'static str; 5] {
    [
        CPU_CONTAINER,
        CUDA_CONTAINER,
        CADDY_CONTAINER,
        BOOTSTRAP_CONTAINER,
        VALIDATOR_CONTAINER,
    ]
}

fn command_line(args: &[String]) -> String {
    format!("docker {}", args.join(" "))
}

fn docker_output(app: &tauri::AppHandle, args: Vec<String>) -> Result<Output, String> {
    log_cmd(app, &command_line(&args));
    crate::cmd::new("docker")
        .args(&args)
        .output()
        .map_err(|e| e.to_string())
}

fn docker_output_no_log(args: &[&str]) -> Result<Output, String> {
    crate::cmd::new("docker")
        .args(args)
        .output()
        .map_err(|e| e.to_string())
}

fn remove_container(app: &tauri::AppHandle, name: &str) {
    let args = vec![
        "rm".to_string(),
        "-f".to_string(),
        name.to_string(),
    ];
    let output = docker_output(app, args);
    if let Ok(o) = output {
        let stderr = String::from_utf8_lossy(&o.stderr);
        if !stderr.trim().is_empty()
            && !stderr.contains("No such container")
        {
            log_output(app, stderr.trim());
        }
    }
}

fn remove_container_no_log(name: &str) {
    let _ = docker_output_no_log(&["rm", "-f", name]);
}

fn pull_image(app: &tauri::AppHandle, image: &str) -> Result<String, String> {
    let output = docker_output(
        app,
        vec!["pull".to_string(), image.to_string()],
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stdout.trim().is_empty() {
        log_output(app, stdout.trim());
    }
    if output.status.success() {
        Ok(stdout)
    } else {
        log_err(app, stderr.trim());
        Err(format!("docker pull failed: {}", stderr.trim()))
    }
}

fn ensure_network(app: &tauri::AppHandle) -> Result<(), String> {
    let inspect = crate::cmd::new("docker")
        .args(["network", "inspect", NETWORK_NAME])
        .output()
        .map_err(|e| e.to_string())?;
    if inspect.status.success() {
        return Ok(());
    }

    let output = docker_output(
        app,
        vec![
            "network".to_string(),
            "create".to_string(),
            NETWORK_NAME.to_string(),
        ],
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn ensure_runtime_dirs() -> Result<(), String> {
    crate::settings::ensure_data_dir()?;
    let root = data_dir();
    for rel in [
        "logs",
        "validator-data",
        "caddy",
        "caddy-data",
        "caddy-config",
    ] {
        std::fs::create_dir_all(root.join(rel)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn data_mount() -> String {
    format!("{}:/data", data_dir().display())
}

fn caddyfile_path() -> std::path::PathBuf {
    data_dir().join("caddy").join("Caddyfile")
}

fn caddy_upstream(run_mode: &RunMode) -> &'static str {
    match run_mode {
        RunMode::Docker => "quip-miner:80",
        RunMode::Native => "host.docker.internal:20080",
    }
}

fn write_caddyfile(run_mode: &RunMode) -> Result<(), String> {
    let content = format!(
        concat!(
            "{{\n",
            "    auto_https off\n",
            "}}\n\n",
            ":20049 {{\n",
            "    handle /rpc {{\n",
            "        rewrite * /\n",
            "        reverse_proxy quip-validator:9944\n",
            "    }}\n",
            "    handle_path /rpc/* {{\n",
            "        reverse_proxy quip-validator:9944\n",
            "    }}\n",
            "    handle /api/v1/* {{\n",
            "        reverse_proxy {}\n",
            "    }}\n",
            "    respond \"Quip Node Manager v0.2 runtime\" 200\n",
            "}}\n",
        ),
        caddy_upstream(run_mode)
    );
    std::fs::write(caddyfile_path(), content).map_err(|e| e.to_string())
}

fn cpuset_for_cpus(num_cpus: u32) -> String {
    match num_cpus {
        0 | 1 => "0".to_string(),
        n => format!("0-{}", n - 1),
    }
}

fn start_validator(app: &tauri::AppHandle) -> Result<String, String> {
    remove_container(app, VALIDATOR_CONTAINER);

    let image = validator_image_ref();
    let validator_data = format!(
        "{}:/data",
        data_dir().join("validator-data").display()
    );
    let args = vec![
        "run",
        "-d",
        "--name",
        VALIDATOR_CONTAINER,
        "--network",
        NETWORK_NAME,
        "--network-alias",
        VALIDATOR_CONTAINER,
        "-p",
        "30333:30333/tcp",
        "-p",
        "30333:30333/udp",
        "-p",
        "127.0.0.1:9944:9944",
        "-v",
        &validator_data,
        &image,
        "--chain=quip-testnet",
        "--base-path=/data",
        "--name=quip-validator",
        "--validator",
        "--state-pruning=archive",
        "--blocks-pruning=archive",
        "--rpc-port=9944",
        "--unsafe-rpc-external",
        "--rpc-cors=*",
        "--rpc-methods=safe",
        "--prometheus-port=9615",
        "--prometheus-external",
        "--no-mdns",
        "--unsafe-force-node-key-generation",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    run_detached(app, args, "Validator started")
}

fn start_caddy(app: &tauri::AppHandle, run_mode: &RunMode) -> Result<String, String> {
    remove_container(app, CADDY_CONTAINER);
    write_caddyfile(run_mode)?;

    let image = caddy_image_ref();
    let caddyfile = format!("{}:/etc/caddy/Caddyfile:ro", caddyfile_path().display());
    let caddy_data = format!("{}:/data", data_dir().join("caddy-data").display());
    let caddy_config = format!(
        "{}:/config",
        data_dir().join("caddy-config").display()
    );
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        CADDY_CONTAINER.to_string(),
        "--network".to_string(),
        NETWORK_NAME.to_string(),
        "-p".to_string(),
        "20049:20049".to_string(),
        "-v".to_string(),
        caddyfile,
        "-v".to_string(),
        caddy_data,
        "-v".to_string(),
        caddy_config,
    ];
    if cfg!(target_os = "linux") && *run_mode == RunMode::Native {
        args.push("--add-host".to_string());
        args.push("host.docker.internal:host-gateway".to_string());
    }
    args.push(image);

    run_detached(app, args, "Caddy started")
}

async fn run_bootstrap(app: &tauri::AppHandle) -> Result<(), String> {
    remove_container(app, BOOTSTRAP_CONTAINER);
    let image = bootstrap_image_ref();
    for attempt in 1..=60 {
        log_output(
            app,
            &format!("[bootstrap] attempt {}/60", attempt),
        );
        let args = vec![
            "run",
            "--rm",
            "--name",
            BOOTSTRAP_CONTAINER,
            "--network",
            NETWORK_NAME,
            "-e",
            &format!("QUIP_FAUCET_URL={}", FAUCET_URL),
            "-v",
            &data_mount(),
            "--entrypoint",
            "quip-miner",
            &image,
            "bootstrap",
            "--validator",
            "ws://quip-validator:9944",
            "--signer-key",
            "/data/keystore.json",
            "--faucet-url",
            FAUCET_URL,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let output = docker_output(app, args)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.trim().is_empty() {
            log_output(app, stdout.trim());
        }
        if output.status.success() {
            log_output(app, "[bootstrap] success");
            return Ok(());
        }
        if !stderr.trim().is_empty() {
            log_err(app, stderr.trim());
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }

    Err("bootstrap failed after 60 attempts".to_string())
}

fn start_miner(
    app: &tauri::AppHandle,
    image_tag: &str,
    config: &crate::settings::NodeConfig,
) -> Result<String, String> {
    let container = miner_container_for_tag(image_tag);
    remove_container(app, container);

    let image = image_ref_for_tag(image_tag);
    let cpuset = cpuset_for_cpus(config.num_cpus);
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container.to_string(),
        "--network".to_string(),
        NETWORK_NAME.to_string(),
        "--network-alias".to_string(),
        MINER_ALIAS.to_string(),
        "-e".to_string(),
        "PUID=1000".to_string(),
        "-e".to_string(),
        "PGID=1000".to_string(),
        "-e".to_string(),
        "QUIP_VALIDATORS=ws://quip-validator:9944".to_string(),
        "-e".to_string(),
        format!("QUIP_FAUCET_URL={}", FAUCET_URL),
        "-e".to_string(),
        "QUIP_REST_HOST=0.0.0.0".to_string(),
        "-e".to_string(),
        "QUIP_REST_PORT=80".to_string(),
        "-e".to_string(),
        "QUIP_SIGNER_KEY=/data/keystore.json".to_string(),
        "-v".to_string(),
        data_mount(),
    ];

    if image_tag == "cpu" {
        args.push("--cpuset-cpus".to_string());
        args.push(cpuset);
    }
    if image_tag == "cuda" {
        args.push("-e".to_string());
        args.push("QUIP_MODE=gpu".to_string());
    }

    let enabled_devices = config
        .gpu_device_configs
        .iter()
        .any(|d| d.enabled);
    let use_gpu = config.gpu_backend == GpuBackend::Local
        && enabled_devices
        && image_tag == "cuda";
    if use_gpu {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    args.push(image);

    run_detached(app, args, "Miner started")
}

fn run_detached(
    app: &tauri::AppHandle,
    args: Vec<String>,
    success_label: &str,
) -> Result<String, String> {
    let output = docker_output(app, args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        let cid = stdout.trim();
        log_output(
            app,
            &format!("{}: {}", success_label, &cid[..12.min(cid.len())]),
        );
        Ok(cid.to_string())
    } else {
        log_err(app, stderr.trim());
        Err(stderr.trim().to_string())
    }
}

pub async fn start_support_containers(
    app: tauri::AppHandle,
    run_mode: RunMode,
) -> Result<(), String> {
    ensure_runtime_dirs()?;
    ensure_network(&app)?;

    pull_image(&app, &validator_image_ref())?;
    pull_image(&app, &caddy_image_ref())?;
    pull_image(&app, &bootstrap_image_ref())?;

    start_validator(&app)?;
    start_caddy(&app, &run_mode)?;
    run_bootstrap(&app).await?;
    Ok(())
}

pub fn stop_support_containers_no_log() {
    remove_container_no_log(CADDY_CONTAINER);
    remove_container_no_log(BOOTSTRAP_CONTAINER);
    remove_container_no_log(VALIDATOR_CONTAINER);
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
    let mut output = String::new();
    let mut images = vec![
        image_ref_for_tag(&image_tag),
        validator_image_ref(),
        caddy_image_ref(),
    ];
    if image_tag != "cpu" {
        images.push(bootstrap_image_ref());
    }
    for image in images {
        output.push_str(&pull_image(&app, &image)?);
    }
    Ok(output)
}

#[tauri::command]
pub async fn start_node_container(app: tauri::AppHandle) -> Result<String, String> {
    let settings = crate::settings::load_settings();
    let mut config = settings.node_config;
    let image_tag = settings.image_tag;

    if config.public_host.is_empty() {
        if let Ok(ip) = crate::network::detect_public_ip().await {
            log_cmd(&app, &format!("Auto-detected public IP: {}", ip));
            config.public_host = ip;
        }
    }

    log_cmd(&app, "Writing config.toml");
    crate::config::write_config_toml(&config, &RunMode::Docker)?;
    ensure_runtime_dirs()?;

    for name in managed_containers() {
        remove_container(&app, name);
    }
    remove_container(&app, LEGACY_CONTAINER);

    pull_node_image(app.clone(), image_tag.clone()).await?;
    ensure_network(&app)?;
    start_validator(&app)?;
    start_caddy(&app, &RunMode::Docker)?;
    run_bootstrap(&app).await?;
    start_miner(&app, &image_tag, &config)
}

#[tauri::command]
pub async fn stop_node_container(app: tauri::AppHandle) -> Result<(), String> {
    for name in [
        CPU_CONTAINER,
        CUDA_CONTAINER,
        CADDY_CONTAINER,
        BOOTSTRAP_CONTAINER,
        VALIDATOR_CONTAINER,
        LEGACY_CONTAINER,
    ] {
        remove_container(&app, name);
    }
    log_output(&app, "Managed containers removed.");
    Ok(())
}

#[tauri::command]
pub async fn get_container_status() -> Result<ContainerStatus, String> {
    let settings = crate::settings::load_settings();
    let miner = miner_container_for_tag(&settings.image_tag).to_string();
    inspect_container(&miner)
}

fn inspect_container(name: &str) -> Result<ContainerStatus, String> {
    let output = tokio::task::block_in_place(|| {
        crate::cmd::new("docker")
            .args([
                "inspect",
                "--format",
                "{{.Id}}\t{{.State.Running}}\t{{.Config.Image}}\t{{.State.Status}}",
                name,
            ])
            .output()
    })
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
    let settings = crate::settings::load_settings();
    let miner = miner_container_for_tag(&settings.image_tag);
    let output = crate::cmd::new("docker")
        .args(["inspect", miner])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}
