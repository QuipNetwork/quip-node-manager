// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig};
use std::fs;

fn render_config_toml(config: &NodeConfig) -> String {
    let mut out = String::new();

    // [global] — matches quip-node.example.toml format
    out.push_str("[global]\n");
    out.push_str(&format!("secret = \"{}\"\n", config.secret));
    out.push_str(&format!("listen = \"{}\"\n", config.listen));
    out.push_str(&format!("port = {}\n", config.port));
    if !config.node_name.is_empty() {
        out.push_str(&format!("node_name = \"{}\"\n", config.node_name));
    }
    if !config.public_host.is_empty() {
        out.push_str(&format!("public_host = \"{}\"\n", config.public_host));
    }
    out.push_str(&format!("auto_mine = {}\n", config.auto_mine));
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
    out.push_str(&format!("verify_ssl = {}\n", config.verify_ssl));
    if config.log_level != "info" {
        out.push_str(&format!("log_level = \"{}\"\n", config.log_level));
    }
    if !config.peers.is_empty() {
        let peers: Vec<String> =
            config.peers.iter().map(|p| format!("\"{}\"", p)).collect();
        out.push_str(&format!("peer = [{}]\n", peers.join(", ")));
    }
    out.push('\n');

    // [tofu]
    out.push_str("[tofu]\n");
    out.push_str("enabled = true\n");
    out.push_str("trust_db = \"/data/trust.db\"\n");
    out.push('\n');

    // [cpu]
    out.push_str("[cpu]\n");
    out.push_str(&format!("num_cpus = {}\n", config.num_cpus));
    out.push('\n');

    // [gpu] — local CUDA or Apple Silicon MPS
    if config.gpu_backend == GpuBackend::Local
        || config.gpu_backend == GpuBackend::Mps
    {
        let enabled: Vec<String> = config
            .gpu_device_configs
            .iter()
            .filter(|d| d.enabled)
            .map(|d| format!("\"{}\"", d.index))
            .collect();
        if !enabled.is_empty() || config.gpu_backend == GpuBackend::Mps {
            out.push_str("[gpu]\n");
            let backend_str = if config.gpu_backend == GpuBackend::Mps {
                "mps"
            } else {
                "local"
            };
            out.push_str(&format!("backend = \"{}\"\n", backend_str));
            out.push_str(&format!("devices = [{}]\n", enabled.join(", ")));
            if let Some(util) = config
                .gpu_device_configs
                .iter()
                .filter(|d| d.enabled)
                .map(|d| d.utilization)
                .max()
            {
                if util < 100 {
                    out.push_str(&format!("gpu_utilization = {}\n", util));
                }
            }
            out.push('\n');
        }
    }

    // [qpu]
    if let Some(qpu) = &config.qpu_config {
        if !qpu.api_key.is_empty() {
            out.push_str("[qpu]\n");
            out.push_str(&format!("dwave_api_key = \"{}\"\n", qpu.api_key));
            if !qpu.solver.is_empty() {
                out.push_str(&format!(
                    "dwave_api_solver = \"{}\"\n",
                    qpu.solver
                ));
            }
            if !qpu.region_url.is_empty() {
                out.push_str(&format!(
                    "dwave_region_url = \"{}\"\n",
                    qpu.region_url
                ));
            }
            if !qpu.daily_budget.is_empty() {
                out.push_str(&format!(
                    "qpu_daily_budget = \"{}\"\n",
                    qpu.daily_budget
                ));
            }
            out.push('\n');
        }
    }

    out
}

pub fn write_config_toml(config: &NodeConfig) -> Result<(), String> {
    crate::settings::ensure_data_dir()?;
    let content = render_config_toml(config);
    fs::write(data_dir().join("config.toml"), content)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_config_toml(
    config: NodeConfig,
) -> Result<String, String> {
    Ok(render_config_toml(&config))
}
