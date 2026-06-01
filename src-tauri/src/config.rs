// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig, RunMode};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;

const DOCKER_VALIDATOR_RPC: &str = "ws://quip-validator:9944";
/// Native miner → local validator: the validator container publishes its raw
/// JSON-RPC on the host loopback (see stack_assets), so the host-side miner
/// connects directly rather than through Caddy's `/rpc` route.
pub(crate) const NATIVE_VALIDATOR_RPC: &str = "ws://127.0.0.1:9944";
const DOCKER_SIGNER_KEY: &str = "/data/keystore.json";
const DOCKER_MINER_REST_HOST: &str = "0.0.0.0";
const DOCKER_MINER_REST_PORT: u16 = 80;
const DEFAULT_NATIVE_REST_PORT: u16 = 20100;

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

#[derive(Serialize)]
struct MinerToml {
    validators: Vec<String>,
    signer_key: String,
    rest_host: String,
    rest_port: u16,
    #[serde(skip_serializing_if = "String::is_empty")]
    node_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    public_host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_port: Option<u16>,
    log_level: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    node_log: String,
}

#[derive(Serialize)]
struct CpuToml {
    num_cpus: u32,
}

#[derive(Serialize)]
struct GpuToml {
    utilization: u8,
    yielding: bool,
}

#[derive(Default, Serialize)]
struct CudaDeviceToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    utilization: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    yielding: Option<bool>,
}

#[derive(Serialize)]
struct DwaveToml {
    #[serde(skip_serializing_if = "String::is_empty")]
    token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    daily_budget: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    solver: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    dwave_region_url: String,
}

#[derive(Default, Serialize)]
struct MarkerToml {}

/// v0.2 `[metal]` section. Carries shared GPU keys (`utilization`,
/// `yielding`) plus the Metal-only adaptive-cap knobs directly, rather
/// than relying on a shared `[gpu]` block.
#[derive(Serialize)]
struct MetalToml {
    utilization: u8,
    yielding: bool,
    active_util: u8,
    idle_after_s: u32,
}

#[derive(Serialize)]
struct ConfigToml {
    miner: MinerToml,
    cpu: CpuToml,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpu: Option<GpuToml>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    cuda: BTreeMap<String, CudaDeviceToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metal: Option<MetalToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modal: Option<MarkerToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qpu: Option<MarkerToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dwave: Option<DwaveToml>,
}

impl ConfigToml {
    fn from_node_config(config: &NodeConfig, run_mode: &RunMode) -> Self {
        let is_docker = *run_mode == RunMode::Docker;
        let miner = MinerToml {
            validators: if is_docker {
                vec![DOCKER_VALIDATOR_RPC.to_string()]
            } else {
                vec![NATIVE_VALIDATOR_RPC.to_string()]
            },
            signer_key: if is_docker {
                DOCKER_SIGNER_KEY.to_string()
            } else {
                native_signer_key()
            },
            rest_host: if is_docker {
                DOCKER_MINER_REST_HOST.to_string()
            } else {
                config.rest_host.clone()
            },
            rest_port: if is_docker {
                DOCKER_MINER_REST_PORT
            } else {
                native_rest_port(config)
            },
            node_name: config.node_name.clone(),
            public_host: config.public_host.clone(),
            public_port: config.public_port,
            log_level: config.log_level.clone(),
            node_log: config.node_log.clone(),
        };

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

        let effective_backend = if is_docker && config.gpu_backend == GpuBackend::Mps {
            None
        } else {
            Some(config.gpu_backend.clone())
        };

        let (gpu_util, gpu_yield) = enabled_devices
            .first()
            .map(|d| (d.utilization, d.yielding))
            .unwrap_or((100, false));

        // [gpu] holds shared defaults for the CUDA-style backends only. Metal
        // carries its own tuning in [metal] (the protocol keeps Metal's
        // adaptive-cap keys out of [gpu] inheritance), so MPS never emits it.
        let gpu = if matches!(
            effective_backend,
            Some(GpuBackend::Local) | Some(GpuBackend::Modal)
        ) && !enabled_devices.is_empty()
        {
            Some(GpuToml {
                utilization: gpu_util,
                yielding: gpu_yield,
            })
        } else {
            None
        };

        let mut cuda = BTreeMap::new();
        let mut metal = None;
        let mut modal = None;
        match effective_backend {
            Some(GpuBackend::Local) => {
                if gpu.is_some() {
                    for dev in &enabled_devices {
                        cuda.insert(
                            dev.index.to_string(),
                            CudaDeviceToml {
                                utilization: (dev.utilization != gpu_util)
                                    .then_some(dev.utilization),
                                yielding: (dev.yielding != gpu_yield).then_some(dev.yielding),
                            },
                        );
                    }
                }
            }
            Some(GpuBackend::Mps) => {
                metal = Some(MetalToml {
                    utilization: config.metal_config.utilization,
                    yielding: config.metal_config.yielding,
                    active_util: config.metal_config.active_util,
                    idle_after_s: config.metal_config.idle_after_s,
                })
            }
            Some(GpuBackend::Modal) => modal = Some(MarkerToml::default()),
            None => {}
        }

        let (qpu, dwave) = match &config.dwave_config {
            Some(dw) => (
                Some(MarkerToml::default()),
                Some(DwaveToml {
                    token: dw.token.clone(),
                    daily_budget: dw.daily_budget.clone(),
                    solver: dw.solver.clone(),
                    dwave_region_url: dw.dwave_region_url.clone(),
                }),
            ),
            None => (None, None),
        };

        ConfigToml {
            miner,
            cpu: CpuToml {
                num_cpus: config.num_cpus,
            },
            gpu,
            cuda,
            metal,
            modal,
            qpu,
            dwave,
        }
    }
}

fn render_config_toml(config: &NodeConfig, run_mode: &RunMode) -> String {
    let config_toml = ConfigToml::from_node_config(config, run_mode);
    toml::to_string_pretty(&config_toml).expect("config TOML serialization should not fail")
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
    use crate::settings::{DwaveConfig, GpuDeviceConfig, MetalConfig};

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

        assert!(toml.contains("validators = [\"ws://127.0.0.1:9944\"]"));
        assert!(toml.contains("signer_key = "));
        assert!(toml.contains("keystore.json"));
        assert!(toml.contains("rest_host = \"127.0.0.1\""));
        assert!(toml.contains("rest_port = 20123"));
    }

    #[test]
    fn mps_backend_writes_tuning_into_metal_section() {
        // v0.2 [metal] carries its own utilization/yielding plus the
        // Metal-only adaptive-cap keys; it does NOT rely on a shared [gpu]
        // block (which the protocol reserves for CUDA inheritance).
        let cfg = NodeConfig {
            gpu_backend: GpuBackend::Mps,
            metal_config: MetalConfig {
                utilization: 5,
                yielding: true,
                active_util: 70,
                idle_after_s: 30,
            },
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Native);
        let metal = toml.find("[metal]").expect("[metal] section missing");
        assert!(
            !toml.contains("[gpu]"),
            "Metal must not emit a shared [gpu] block"
        );
        assert!(toml[metal..].contains("utilization = 5"));
        assert!(toml[metal..].contains("yielding = true"));
        assert!(toml[metal..].contains("active_util = 70"));
        assert!(toml[metal..].contains("idle_after_s = 30"));
    }

    #[test]
    fn mps_metal_tuning_is_independent_of_cuda_device_toggles() {
        // A disabled CUDA-style device entry must not suppress Metal tuning:
        // Metal is a single implicit GPU driven by metal_config.
        let cfg = NodeConfig {
            gpu_backend: GpuBackend::Mps,
            gpu_device_configs: vec![GpuDeviceConfig {
                index: 0,
                enabled: false,
                utilization: 100,
                yielding: false,
            }],
            metal_config: MetalConfig {
                utilization: 42,
                yielding: true,
                active_util: 85,
                idle_after_s: 60,
            },
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Native);
        let metal = toml.find("[metal]").expect("[metal] section missing");
        assert!(!toml.contains("[gpu]"));
        assert!(toml[metal..].contains("utilization = 42"));
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
