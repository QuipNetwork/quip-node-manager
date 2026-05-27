// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig, RunMode};
use std::fs;

const DOCKER_VALIDATOR: &str = "ws://quip-validator:9944";
const NATIVE_VALIDATOR: &str = "ws://127.0.0.1:9944";
const DOCKER_SIGNER_KEY: &str = "/data/keystore.json";
const NATIVE_SIGNER_KEY_FILE: &str = "keystore.json";
const DOCKER_NODE_LOG: &str = "/data/logs/quip-node.log";
const NATIVE_NODE_LOG_FILE: &str = "logs/quip-node.log";
const REST_HOST: &str = "0.0.0.0";
const DOCKER_REST_PORT: i16 = 80;
const NATIVE_REST_PORT: i16 = 20080;

fn render_config_toml(config: &NodeConfig, run_mode: &RunMode) -> String {
    render_v02_config_toml(config, run_mode)
}

fn render_v02_config_toml(config: &NodeConfig, run_mode: &RunMode) -> String {
    let mut out = String::new();

    out.push_str("[miner]\n");
    if !config.node_name.is_empty() {
        out.push_str(&format!("node_name = \"{}\"\n", config.node_name));
    }
    if !config.public_host.is_empty() {
        out.push_str(&format!("public_host = \"{}\"\n", config.public_host));
    }
    if let Some(pp) = config.public_port {
        out.push_str(&format!("public_port = {}\n", pp));
    }

    let validators = validators_for_config(config, run_mode);
    let validator_strs: Vec<String> = validators.iter().map(|p| format!("\"{}\"", p)).collect();
    out.push_str(&format!("validators = [{}]\n", validator_strs.join(", ")));
    out.push_str(&format!(
        "signer_key = \"{}\"\n",
        signer_key_for_run_mode(run_mode)
    ));
    out.push_str(&format!("rest_host = \"{}\"\n", REST_HOST));
    out.push_str(&format!(
        "rest_port = {}\n",
        rest_port_for_run_mode(run_mode)
    ));
    out.push_str(&format!("log_level = \"{}\"\n", config.log_level));
    out.push_str(&format!(
        "node_log = \"{}\"\n",
        node_log_for_config(config, run_mode)
    ));
    out.push('\n');

    render_backend_sections(&mut out, config, true);

    out
}

fn validators_for_config(config: &NodeConfig, run_mode: &RunMode) -> Vec<String> {
    let validators: Vec<String> = config
        .peers
        .iter()
        .filter(|p| p.starts_with("ws://") || p.starts_with("wss://"))
        .cloned()
        .collect();
    if !validators.is_empty() {
        return validators;
    }

    match run_mode {
        RunMode::Docker => vec![DOCKER_VALIDATOR.to_string()],
        RunMode::Native => vec![NATIVE_VALIDATOR.to_string()],
    }
}

fn signer_key_for_run_mode(run_mode: &RunMode) -> String {
    match run_mode {
        RunMode::Docker => DOCKER_SIGNER_KEY.to_string(),
        RunMode::Native => data_dir()
            .join(NATIVE_SIGNER_KEY_FILE)
            .display()
            .to_string(),
    }
}

fn rest_port_for_run_mode(run_mode: &RunMode) -> i16 {
    match run_mode {
        RunMode::Docker => DOCKER_REST_PORT,
        RunMode::Native => NATIVE_REST_PORT,
    }
}

fn node_log_for_config(config: &NodeConfig, run_mode: &RunMode) -> String {
    if !config.node_log.is_empty() {
        return config.node_log.clone();
    }

    match run_mode {
        RunMode::Docker => DOCKER_NODE_LOG.to_string(),
        RunMode::Native => data_dir().join(NATIVE_NODE_LOG_FILE).display().to_string(),
    }
}

fn render_backend_sections(out: &mut String, config: &NodeConfig, include_qpu_marker: bool) {
    // [cpu]
    out.push_str("[cpu]\n");
    out.push_str(&format!("num_cpus = {}\n", config.num_cpus));
    out.push('\n');

    // GPU sections
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
                out.push_str(&format!("utilization = {}\n", first.utilization));
                out.push_str(&format!("yielding = {}\n", first.yielding));
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

    // [dwave]
    if let Some(dw) = &config.dwave_config {
        if !dw.token.is_empty() {
            if include_qpu_marker {
                out.push_str("[qpu]\n");
                out.push('\n');
            }
            out.push_str("[dwave]\n");
            out.push_str(&format!("token = \"{}\"\n", dw.token));
            if !dw.daily_budget.is_empty() {
                out.push_str(&format!("daily_budget = \"{}\"\n", dw.daily_budget));
            }
            if !dw.solver.is_empty() {
                out.push_str(&format!("solver = \"{}\"\n", dw.solver));
            }
            if !dw.dwave_region_url.is_empty() {
                out.push_str(&format!("dwave_region_url = \"{}\"\n", dw.dwave_region_url));
            }
            out.push('\n');
        }
    }
}

pub fn write_config_toml(config: &NodeConfig, run_mode: &RunMode) -> Result<(), String> {
    crate::settings::ensure_data_dir()?;
    let content = render_config_toml(config, run_mode);
    fs::write(data_dir().join("config.toml"), content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_config_toml(config: NodeConfig, run_mode: RunMode) -> Result<String, String> {
    Ok(render_config_toml(&config, &run_mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{DwaveConfig, GpuDeviceConfig};

    #[test]
    fn docker_config_uses_v02_miner_schema() {
        let config = NodeConfig::default();
        let toml = render_config_toml(&config, &RunMode::Docker);

        assert!(toml.starts_with("[miner]\n"));
        assert!(toml.contains("validators = [\"ws://quip-validator:9944\"]"));
        assert!(toml.contains("signer_key = \"/data/keystore.json\""));
        assert!(toml.contains("rest_host = \"0.0.0.0\""));
        assert!(toml.contains("rest_port = 80"));
        assert!(toml.contains("node_log = \"/data/logs/quip-node.log\""));
        assert!(toml.contains("[cpu]\n"));
    }

    #[test]
    fn docker_config_does_not_emit_legacy_p2p_fields() {
        let mut config = NodeConfig::default();
        config.listen = "::".to_string();
        config.port = 20049;
        config.secret = "abc123".to_string();
        config.trust_db = "/data/trust.db".to_string();
        config.peers = vec!["qpu-1.nodes.quip.network:20049".to_string()];

        let toml = render_config_toml(&config, &RunMode::Docker);

        assert!(!toml.contains("[global]"));
        assert!(!toml.contains("peer ="));
        assert!(!toml.contains("secret ="));
        assert!(!toml.contains("genesis_config"));
        assert!(!toml.contains("\nlisten ="));
        assert!(!toml.contains("\nport ="));
        assert!(!toml.contains("trust_db"));
        assert!(!toml.contains("tofu ="));
        assert!(!toml.contains("verify_tls"));
        assert!(!toml.contains("heartbeat_"));
    }

    #[test]
    fn docker_config_treats_ws_peers_as_validators() {
        let mut config = NodeConfig::default();
        config.peers = vec![
            "qpu-1.nodes.quip.network:20049".to_string(),
            "wss://validator.example.com/rpc".to_string(),
        ];

        let toml = render_config_toml(&config, &RunMode::Docker);

        assert!(toml.contains("validators = [\"wss://validator.example.com/rpc\"]"));
        assert!(!toml.contains("qpu-1.nodes.quip.network:20049"));
    }

    #[test]
    fn docker_config_preserves_backend_sections() {
        let mut config = NodeConfig::default();
        config.num_cpus = 4;
        config.gpu_backend = GpuBackend::Local;
        config.gpu_device_configs = vec![GpuDeviceConfig {
            index: 0,
            enabled: true,
            utilization: 55,
            yielding: true,
        }];
        config.dwave_config = Some(DwaveConfig {
            token: "test-token".to_string(),
            solver: "Advantage2_System1.13".to_string(),
            dwave_region_url: "https://na-west-1.cloud.dwavesys.com/sapi/v2/".to_string(),
            daily_budget: "16m".to_string(),
            qpu_min_blocks_for_estimation: None,
            qpu_ema_alpha: None,
        });

        let toml = render_config_toml(&config, &RunMode::Docker);

        assert!(toml.contains("[cpu]\nnum_cpus = 4"));
        assert!(toml.contains("[gpu]\n"));
        assert!(toml.contains("utilization = 55"));
        assert!(toml.contains("yielding = true"));
        assert!(toml.contains("[cuda.0]\n"));
        assert!(toml.contains("[qpu]\n"));
        assert!(toml.contains("[dwave]\n"));
        assert!(toml.contains("token = \"test-token\""));
    }

    #[test]
    fn native_config_uses_v02_schema_with_host_defaults() {
        let config = NodeConfig::default();
        let toml = render_config_toml(&config, &RunMode::Native);
        let signer_key = data_dir()
            .join(NATIVE_SIGNER_KEY_FILE)
            .display()
            .to_string();
        let node_log = data_dir().join(NATIVE_NODE_LOG_FILE).display().to_string();

        assert!(toml.starts_with("[miner]\n"));
        assert!(toml.contains("validators = [\"ws://127.0.0.1:9944\"]"));
        assert!(toml.contains(&format!("signer_key = \"{}\"", signer_key)));
        assert!(toml.contains("rest_host = \"0.0.0.0\""));
        assert!(toml.contains("rest_port = 20080"));
        assert!(toml.contains(&format!("node_log = \"{}\"", node_log)));
        assert!(!toml.contains("[global]\n"));
        assert!(!toml.contains("peer = ["));
        assert!(!toml.contains("trust_db ="));
    }
}
