// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig, RunMode};
use std::fs;

const DOCKER_VALIDATOR_RPC: &str = "ws://quip-validator:9944";
const DOCKER_SIGNER_KEY: &str = "/data/keystore.json";
const DOCKER_MINER_REST_HOST: &str = "0.0.0.0";
const DOCKER_MINER_REST_PORT: u16 = 80;
const DEFAULT_NATIVE_REST_PORT: u16 = 20100;

fn toml_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{}\"", escaped)
}

fn push_toml_str(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(&toml_string(value));
    out.push('\n');
}

fn push_toml_string_array(out: &mut String, key: &str, values: &[String]) {
    out.push_str(key);
    out.push_str(" = [");
    for (idx, value) in values.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&toml_string(value));
    }
    out.push_str("]\n");
}

fn native_rest_port(config: &NodeConfig) -> u16 {
    if config.rest_insecure_port > 0 {
        config.rest_insecure_port as u16
    } else if config.rest_port > 0 {
        config.rest_port as u16
    } else {
        DEFAULT_NATIVE_REST_PORT
    }
}

fn native_signer_key() -> String {
    data_dir()
        .join("keystore.json")
        .to_string_lossy()
        .to_string()
}

fn render_config_toml(config: &NodeConfig, run_mode: &RunMode) -> String {
    let mut out = String::new();
    let is_docker = *run_mode == RunMode::Docker;

    // ── [miner] ─────────────────────────────────────────────────────────
    out.push_str("[miner]\n");
    let validators = if is_docker {
        vec![DOCKER_VALIDATOR_RPC.to_string()]
    } else {
        vec![format!("ws://127.0.0.1:{}/rpc", config.port)]
    };
    push_toml_string_array(&mut out, "validators", &validators);
    let signer_key = if is_docker {
        DOCKER_SIGNER_KEY.to_string()
    } else {
        native_signer_key()
    };
    push_toml_str(&mut out, "signer_key", &signer_key);
    if is_docker {
        push_toml_str(&mut out, "rest_host", DOCKER_MINER_REST_HOST);
        out.push_str(&format!("rest_port = {}\n", DOCKER_MINER_REST_PORT));
    } else {
        push_toml_str(&mut out, "rest_host", &config.rest_host);
        out.push_str(&format!("rest_port = {}\n", native_rest_port(config)));
    }

    if !config.node_name.is_empty() {
        push_toml_str(&mut out, "node_name", &config.node_name);
    }
    if !config.public_host.is_empty() {
        push_toml_str(&mut out, "public_host", &config.public_host);
    }
    if let Some(pp) = config.public_port {
        out.push_str(&format!("public_port = {}\n", pp));
    }
    push_toml_str(&mut out, "log_level", &config.log_level);
    if !config.node_log.is_empty() {
        push_toml_str(&mut out, "node_log", &config.node_log);
    }
    out.push('\n');

    // ── [cpu] ───────────────────────────────────────────────────────────
    out.push_str("[cpu]\n");
    out.push_str(&format!("num_cpus = {}\n", config.num_cpus));
    out.push('\n');

    // ── GPU sections ────────────────────────────────────────────────────
    // [gpu] holds global defaults inherited by every backend section
    // ([cuda.N], [metal], [modal]). See quip-protocol/quip-miner.example.toml.
    //
    // Metal is unavailable in Linux containers regardless of what the Mac
    // host reports. In Docker mode we suppress Mps.
    let enabled_devices: Vec<&crate::settings::GpuDeviceConfig> = config
        .gpu_device_configs
        .iter()
        .filter(|d| d.enabled)
        .collect();

    // Effective backend: Mps is clamped to "no backend" in Docker mode
    // because the container is Linux and has no Metal access.
    let effective_backend = if is_docker && config.gpu_backend == GpuBackend::Mps {
        None
    } else {
        Some(config.gpu_backend.clone())
    };

    let (gpu_util, gpu_yield) = enabled_devices
        .first()
        .map(|d| (d.utilization, d.yielding))
        .unwrap_or((100, false));

    let emit_gpu_globals = effective_backend.is_some() && !enabled_devices.is_empty();

    if emit_gpu_globals {
        out.push_str("[gpu]\n");
        out.push_str(&format!("utilization = {}\n", gpu_util));
        out.push_str(&format!("yielding = {}\n", gpu_yield));
        out.push('\n');
    }

    match effective_backend {
        Some(GpuBackend::Local) => {
            if emit_gpu_globals {
                for dev in &enabled_devices {
                    out.push_str(&format!("[cuda.{}]\n", dev.index));
                    if dev.utilization != gpu_util {
                        out.push_str(&format!("utilization = {}\n", dev.utilization));
                    }
                    if dev.yielding != gpu_yield {
                        out.push_str(&format!("yielding = {}\n", dev.yielding));
                    }
                    out.push('\n');
                }
            }
        }
        Some(GpuBackend::Mps) => {
            out.push_str("[metal]\n");
            out.push('\n');
        }
        Some(GpuBackend::Modal) => {
            out.push_str("[modal]\n");
            out.push('\n');
        }
        None => {}
    }

    // ── [qpu] / [dwave] ─────────────────────────────────────────────────
    if let Some(dw) = &config.dwave_config {
        out.push_str("[qpu]\n\n");
        out.push_str("[dwave]\n");
        if !dw.token.is_empty() {
            push_toml_str(&mut out, "token", &dw.token);
        }
        if !dw.daily_budget.is_empty() {
            push_toml_str(&mut out, "daily_budget", &dw.daily_budget);
        }
        if !dw.solver.is_empty() {
            push_toml_str(&mut out, "solver", &dw.solver);
        }
        if !dw.dwave_region_url.is_empty() {
            push_toml_str(&mut out, "dwave_region_url", &dw.dwave_region_url);
        }
        out.push('\n');
    }

    out
}

pub fn write_config_toml(config: &NodeConfig, run_mode: &RunMode) -> Result<(), String> {
    crate::settings::ensure_data_dir()?;
    let content = render_config_toml(config, run_mode);
    // Docker mode: compose bind-mounts `./data:/data` (relative to the
    // project-directory), so the container sees `/data/config.toml` as
    // `<data_dir>/data/config.toml` on the host. Writing to the bare
    // `<data_dir>/config.toml` (native's location) would land it outside
    // the mount and the miner would never read it — falling back to
    // auto-detected defaults like `num_cpus = os.cpu_count()`.
    let path = match run_mode {
        RunMode::Docker => data_dir().join("data").join("config.toml"),
        RunMode::Native => data_dir().join("config.toml"),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_config_toml(config: NodeConfig, run_mode: RunMode) -> Result<String, String> {
    Ok(render_config_toml(&config, &run_mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{DwaveConfig, GpuDeviceConfig};

    fn cfg_with_gpu(backend: GpuBackend, devices: Vec<GpuDeviceConfig>) -> NodeConfig {
        NodeConfig {
            gpu_backend: backend,
            gpu_device_configs: devices,
            ..NodeConfig::default()
        }
    }

    #[test]
    fn docker_config_uses_v02_miner_schema() {
        let cfg = NodeConfig {
            listen: "0.0.0.0".to_string(),
            peers: vec!["legacy-peer:20049".to_string()],
            auto_mine: true,
            secret: "legacy-secret".to_string(),
            genesis_config: "legacy-genesis.json".to_string(),
            timeout: 9,
            heartbeat_interval: 10,
            heartbeat_timeout: 11,
            fanout: Some(3),
            verify_tls: true,
            tls_cert_file: "cert.pem".to_string(),
            tls_key_file: "key.pem".to_string(),
            tofu: false,
            trust_db: "/tmp/trust.db".to_string(),
            http_log: "http.log".to_string(),
            telemetry_enabled: false,
            telemetry_dir: "telemetry-old".to_string(),
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Docker);

        assert!(toml.contains("[miner]\n"));
        assert!(!toml.contains("[global]"));
        assert!(toml.contains("validators = [\"ws://quip-validator:9944\"]"));
        assert!(toml.contains("signer_key = \"/data/keystore.json\""));
        assert!(toml.contains("rest_host = \"0.0.0.0\""));
        assert!(toml.contains("rest_port = 80"));
        assert!(toml.contains("[cpu]\n"));

        for legacy_key in [
            "listen =",
            "port = 20049",
            "peer =",
            "auto_mine",
            "secret =",
            "genesis_config",
            "timeout =",
            "heartbeat_interval",
            "heartbeat_timeout",
            "fanout",
            "verify_tls",
            "tls_cert_file",
            "tls_key_file",
            "tofu",
            "trust_db",
            "http_log",
            "telemetry_enabled",
            "telemetry_dir",
        ] {
            assert!(
                !toml.contains(legacy_key),
                "rendered legacy key: {legacy_key}"
            );
        }
    }

    #[test]
    fn docker_config_preserves_promoted_miner_fields() {
        let cfg = NodeConfig {
            node_name: "validator-home".to_string(),
            public_host: "node.example.com".to_string(),
            public_port: Some(24444),
            log_level: "debug".to_string(),
            node_log: "/data/logs/miner.log".to_string(),
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Docker);

        assert!(toml.contains("node_name = \"validator-home\""));
        assert!(toml.contains("public_host = \"node.example.com\""));
        assert!(toml.contains("public_port = 24444"));
        assert!(toml.contains("log_level = \"debug\""));
        assert!(toml.contains("node_log = \"/data/logs/miner.log\""));
    }

    #[test]
    fn native_config_renders_host_local_miner_paths() {
        let cfg = NodeConfig {
            port: 21049,
            rest_host: "127.0.0.1".to_string(),
            rest_insecure_port: 20123,
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Native);

        assert!(toml.contains("validators = [\"ws://127.0.0.1:21049/rpc\"]"));
        assert!(toml.contains("signer_key = "));
        assert!(toml.contains("keystore.json"));
        assert!(toml.contains("rest_host = \"127.0.0.1\""));
        assert!(toml.contains("rest_port = 20123"));
    }

    #[test]
    fn mps_backend_emits_gpu_globals_before_metal() {
        let cfg = cfg_with_gpu(
            GpuBackend::Mps,
            vec![GpuDeviceConfig {
                index: 0,
                enabled: true,
                utilization: 5,
                yielding: true,
            }],
        );
        let toml = render_config_toml(&cfg, &RunMode::Native);
        let gpu = toml.find("[gpu]").expect("[gpu] section missing");
        let metal = toml.find("[metal]").expect("[metal] section missing");
        assert!(gpu < metal, "[gpu] must precede [metal]");
        assert!(toml[gpu..metal].contains("utilization = 5"));
        assert!(toml[gpu..metal].contains("yielding = true"));
    }

    #[test]
    fn modal_backend_emits_gpu_globals_before_modal() {
        let cfg = cfg_with_gpu(
            GpuBackend::Modal,
            vec![GpuDeviceConfig {
                index: 0,
                enabled: true,
                utilization: 80,
                yielding: false,
            }],
        );
        let toml = render_config_toml(&cfg, &RunMode::Docker);
        assert!(toml.contains("[gpu]\nutilization = 80\nyielding = false"));
        assert!(toml.contains("[modal]"));
    }

    #[test]
    fn cuda_per_device_emits_only_deltas() {
        let cfg = cfg_with_gpu(
            GpuBackend::Local,
            vec![
                GpuDeviceConfig {
                    index: 0,
                    enabled: true,
                    utilization: 80,
                    yielding: false,
                },
                GpuDeviceConfig {
                    index: 1,
                    enabled: true,
                    utilization: 50,
                    yielding: true,
                },
            ],
        );
        let toml = render_config_toml(&cfg, &RunMode::Docker);
        let cuda0 = toml.find("[cuda.0]").unwrap();
        let cuda1 = toml.find("[cuda.1]").unwrap();
        // [cuda.0] matches globals → no overrides
        assert!(!toml[cuda0..cuda1].contains("utilization"));
        assert!(!toml[cuda0..cuda1].contains("yielding"));
        // [cuda.1] differs → both fields emitted
        assert!(toml[cuda1..].contains("utilization = 50"));
        assert!(toml[cuda1..].contains("yielding = true"));
    }

    #[test]
    fn mps_without_devices_skips_gpu_section() {
        let cfg = cfg_with_gpu(GpuBackend::Mps, vec![]);
        let toml = render_config_toml(&cfg, &RunMode::Native);
        assert!(!toml.contains("[gpu]"));
        assert!(toml.contains("[metal]"));
    }

    #[test]
    fn dwave_config_activates_qpu_backend() {
        let cfg = NodeConfig {
            dwave_config: Some(DwaveConfig {
                token: "DWAVE-TOKEN".to_string(),
                daily_budget: "60s".to_string(),
                ..DwaveConfig::default()
            }),
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Docker);

        assert!(toml.contains("[qpu]\n"));
        assert!(toml.contains("[dwave]\n"));
        assert!(toml.contains("token = \"DWAVE-TOKEN\""));
        assert!(toml.contains("daily_budget = \"60s\""));
        assert!(toml.contains("solver = \"Advantage2_System1.13\""));
    }
}
