// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig, RunMode};
use std::fs;

fn render_config_toml(
    config: &NodeConfig,
    run_mode: &RunMode,
) -> String {
    let mut out = String::new();
    let is_docker = *run_mode == RunMode::Docker;

    // ── [global] ────────────────────────────────────────────────────────
    out.push_str("[global]\n");
    if !config.node_name.is_empty() {
        out.push_str(&format!(
            "node_name = \"{}\"\n",
            config.node_name
        ));
    }
    out.push_str(&format!("listen = \"{}\"\n", config.listen));
    out.push_str(&format!("port = {}\n", config.port));
    if !config.public_host.is_empty() {
        out.push_str(&format!(
            "public_host = \"{}\"\n",
            config.public_host
        ));
    }
    if let Some(pp) = config.public_port {
        out.push_str(&format!("public_port = {}\n", pp));
    }
    if !config.secret.is_empty() {
        out.push_str(&format!("secret = \"{}\"\n", config.secret));
    }
    out.push_str(&format!("auto_mine = {}\n", config.auto_mine));
    out.push_str(&format!(
        "genesis_config = \"{}\"\n",
        config.genesis_config
    ));
    if !config.peers.is_empty() {
        let peers: Vec<String> =
            config.peers.iter().map(|p| format!("\"{}\"", p)).collect();
        out.push_str(&format!("peer = [{}]\n", peers.join(", ")));
    }
    out.push_str(&format!("timeout = {}\n", config.timeout));
    out.push_str(&format!(
        "heartbeat_interval = {}\n",
        config.heartbeat_interval
    ));
    out.push_str(&format!(
        "heartbeat_timeout = {}\n",
        config.heartbeat_timeout
    ));
    if let Some(fanout) = config.fanout {
        out.push_str(&format!("fanout = {}\n", fanout));
    }

    // TLS
    out.push_str(&format!("verify_tls = {}\n", config.verify_tls));
    if !config.tls_cert_file.is_empty() {
        out.push_str(&format!(
            "tls_cert_file = \"{}\"\n",
            config.tls_cert_file
        ));
        out.push_str(&format!(
            "tls_key_file = \"{}\"\n",
            config.tls_key_file
        ));
    }

    // TOFU
    out.push_str(&format!("tofu = {}\n", config.tofu));
    let trust_db = if is_docker {
        "/data/trust.db".to_string()
    } else {
        config.trust_db.clone()
    };
    out.push_str(&format!("trust_db = \"{}\"\n", trust_db));

    // REST API (only emit when explicitly enabled)
    if config.rest_port > 0 || config.rest_insecure_port > 0 {
        out.push_str(&format!(
            "rest_host = \"{}\"\n",
            config.rest_host
        ));
        out.push_str(&format!("rest_port = {}\n", config.rest_port));
        out.push_str(&format!(
            "rest_insecure_port = {}\n",
            config.rest_insecure_port
        ));
    }

    // Logging
    out.push_str(&format!(
        "log_level = \"{}\"\n",
        config.log_level
    ));
    if !config.node_log.is_empty() {
        out.push_str(&format!(
            "node_log = \"{}\"\n",
            config.node_log
        ));
    }
    if !config.http_log.is_empty() {
        out.push_str(&format!(
            "http_log = \"{}\"\n",
            config.http_log
        ));
    }

    // Telemetry
    out.push_str(&format!(
        "telemetry_enabled = {}\n",
        config.telemetry_enabled
    ));
    if config.telemetry_enabled {
        out.push_str(&format!(
            "telemetry_dir = \"{}\"\n",
            config.telemetry_dir
        ));
    }
    out.push('\n');

    // ── [cpu] ───────────────────────────────────────────────────────────
    out.push_str("[cpu]\n");
    out.push_str(&format!("num_cpus = {}\n", config.num_cpus));
    out.push('\n');

    // ── GPU sections ────────────────────────────────────────────────────
    let enabled_devices: Vec<&crate::settings::GpuDeviceConfig> = config
        .gpu_device_configs
        .iter()
        .filter(|d| d.enabled)
        .collect();

    match config.gpu_backend {
        GpuBackend::Local if !enabled_devices.is_empty() => {
            // [gpu] global defaults
            if let Some(first) = enabled_devices.first() {
                out.push_str("[gpu]\n");
                out.push_str(&format!(
                    "utilization = {}\n",
                    first.utilization
                ));
                out.push_str(&format!(
                    "yielding = {}\n",
                    first.yielding
                ));
                out.push('\n');
            }
            // [cuda.N] per-device sections
            for dev in &enabled_devices {
                out.push_str(&format!("[cuda.{}]\n", dev.index));
                out.push('\n');
            }
        }
        GpuBackend::Mps => {
            out.push_str("[metal]\n");
            out.push('\n');
        }
        GpuBackend::Modal => {
            out.push_str("[modal]\n");
            out.push('\n');
        }
        _ => {}
    }

    // ── [dwave] ─────────────────────────────────────────────────────────
    if let Some(dw) = &config.dwave_config {
        if !dw.token.is_empty() {
            out.push_str("[dwave]\n");
            out.push_str(&format!("token = \"{}\"\n", dw.token));
            if !dw.daily_budget.is_empty() {
                out.push_str(&format!(
                    "daily_budget = \"{}\"\n",
                    dw.daily_budget
                ));
            }
            if !dw.solver.is_empty() {
                out.push_str(&format!(
                    "solver = \"{}\"\n",
                    dw.solver
                ));
            }
            if !dw.dwave_region_url.is_empty() {
                out.push_str(&format!(
                    "dwave_region_url = \"{}\"\n",
                    dw.dwave_region_url
                ));
            }
            out.push('\n');
        }
    }

    out
}

pub fn write_config_toml(
    config: &NodeConfig,
    run_mode: &RunMode,
) -> Result<(), String> {
    crate::settings::ensure_data_dir()?;
    let content = render_config_toml(config, run_mode);
    fs::write(data_dir().join("config.toml"), content)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_config_toml(
    config: NodeConfig,
    run_mode: RunMode,
) -> Result<String, String> {
    Ok(render_config_toml(&config, &run_mode))
}
