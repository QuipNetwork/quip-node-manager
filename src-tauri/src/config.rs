// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig, RunMode};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;

// Shared v0.2 Docker-mode miner contract. Both this module (fresh config
// rendering) and `migration_v2` (v0.1 → v0.2 upgrade) must agree on these, so
// they live in one place.
pub(crate) const DOCKER_VALIDATOR_RPC: &str = "ws://quip-validator:9944";
pub(crate) const DOCKER_SIGNER_KEY: &str = "/data/keystore.json";
// Faucet bot for wallet auto-topup at startup. The miner has NO built-in
// default: absent `faucet_url` → underfunded wallet fails fast with
// `wallet-underfunded`. The manager writes its own config.toml, bypassing the
// upstream seed template, so it must render this key itself. Both run modes
// target the public testnet.
pub(crate) const FAUCET_URL: &str = "https://faucet.testnet.quip.network";
pub(crate) const DOCKER_MINER_REST_HOST: &str = "0.0.0.0";
// Container-internal miner REST port. Must match the upstream Caddyfile's
// `reverse_proxy quip-miner:8086` and the seeded `quip-miner.{cpu,cuda}.toml`
// `rest_port` (see stack_assets); the miner publishes no host port.
pub(crate) const DOCKER_MINER_REST_PORT: u16 = 8086;
pub(crate) const DEFAULT_NATIVE_REST_PORT: u16 = 20100;

/// Native miner → local validator: the validator container publishes its raw
/// JSON-RPC on the host loopback (see stack_assets), so the host-side miner
/// connects directly rather than through Caddy's `/rpc` route. The host port
/// is configurable (`validator_rpc_port`, default 9944).
pub(crate) fn native_validator_rpc_url(config: &NodeConfig) -> String {
    format!("ws://127.0.0.1:{}", config.validator_rpc_port)
}

/// Host REST port for the native miner. This is the single source of truth for
/// the port the miner binds (rendered into config.toml here) AND the port the
/// dashboard's Caddyfile routes to (via `stack_assets`); both must agree.
pub(crate) fn native_rest_port(config: &NodeConfig) -> u16 {
    if config.rest_insecure_port > 0 {
        config.rest_insecure_port as u16
    } else if config.rest_port > 0 {
        config.rest_port as u16
    } else {
        DEFAULT_NATIVE_REST_PORT
    }
}

fn native_signer_key() -> String {
    crate::native::native_signer_key_path()
        .to_string_lossy()
        .to_string()
}

#[derive(Serialize)]
struct MinerToml {
    validators: Vec<String>,
    signer_key: String,
    faucet_url: String,
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

/// Absolute path to a bundled miner in Native mode, empty in Docker mode.
///
/// The coordinator resolves a bare `binary` name through PATH. In the image the
/// miners are installed to `/usr/local/bin`, so the default bare name is right
/// there and a host path would not exist. On the host nothing puts the bundle's
/// bin dir on PATH, so the section must name the miner outright.
fn native_binary(run_mode: &RunMode, name: &str) -> String {
    match run_mode {
        RunMode::Docker => String::new(),
        RunMode::Native => crate::native::miner_binary_path(name)
            .to_string_lossy()
            .to_string(),
    }
}

#[derive(Serialize)]
struct CpuToml {
    #[serde(skip_serializing_if = "String::is_empty")]
    binary: String,
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
    binary: String,
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
    #[serde(skip_serializing_if = "String::is_empty")]
    binary: String,
    utilization: u8,
    yielding: bool,
    active_util: u8,
    idle_after_s: u32,
}

#[derive(Serialize)]
struct ConfigToml {
    miner: MinerToml,
    // None when CPU mining is disabled — the miner treats [cpu] presence as
    // the backend switch, so the section must vanish entirely, not zero out.
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu: Option<CpuToml>,
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
                vec![native_validator_rpc_url(config)]
            },
            signer_key: if is_docker {
                DOCKER_SIGNER_KEY.to_string()
            } else {
                native_signer_key()
            },
            faucet_url: FAUCET_URL.to_string(),
            // Native miner REST is loopback-only by design — the dashboard
            // container reaches it via host.docker.internal. Forcing it here
            // (rather than at each start path) keeps the invariant in one
            // place so a promoted or user-set rest_host can't expose it.
            rest_host: if is_docker {
                DOCKER_MINER_REST_HOST.to_string()
            } else {
                "127.0.0.1".to_string()
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
                    binary: native_binary(run_mode, "quip-metal-sa"),
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
                    binary: native_binary(run_mode, "quip-dwave-qa"),
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
            cpu: config.cpu_enabled.then_some(CpuToml {
                binary: native_binary(run_mode, "quip-cpu-sa"),
                num_cpus: config.num_cpus,
            }),
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
    fn native_miner_binaries_are_absolute_paths() {
        // The coordinator spawns each miner with `Command::new(binary)`. A bare
        // name there is resolved through PATH, not through the coordinator's
        // working directory, and the bundle's bin dir is on neither. So Native
        // mode must name each miner by absolute path.
        let cfg = NodeConfig {
            gpu_backend: GpuBackend::Mps,
            dwave_config: Some(DwaveConfig {
                token: "tok".to_string(),
                ..DwaveConfig::default()
            }),
            ..NodeConfig::default()
        };
        let rendered = render_config_toml(&cfg, &RunMode::Native);
        let parsed: toml::Value = toml::from_str(&rendered).expect("valid toml");

        for (section, binary) in [
            ("cpu", "quip-cpu-sa"),
            ("metal", "quip-metal-sa"),
            ("dwave", "quip-dwave-qa"),
        ] {
            let got = parsed[section]["binary"]
                .as_str()
                .unwrap_or_else(|| panic!("[{section}] has no binary key"));
            let path = std::path::Path::new(got);
            assert!(path.is_absolute(), "[{section}] binary {got} is not absolute");
            assert!(path.ends_with(binary), "[{section}] binary is {got}");
        }
    }

    #[test]
    fn docker_miner_binaries_stay_bare_names() {
        // Inside the image the miners live in /usr/local/bin, which is on PATH.
        // A host path would not exist in the container.
        let cfg = NodeConfig {
            dwave_config: Some(DwaveConfig {
                token: "tok".to_string(),
                ..DwaveConfig::default()
            }),
            ..NodeConfig::default()
        };
        let rendered = render_config_toml(&cfg, &RunMode::Docker);
        let parsed: toml::Value = toml::from_str(&rendered).expect("valid toml");

        assert!(parsed["cpu"].get("binary").is_none());
        assert!(parsed["dwave"].get("binary").is_none());
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
        assert!(toml.contains("rest_port = 8086"));
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
            // A non-loopback rest_host must be overridden to 127.0.0.1 by the
            // renderer (native REST is loopback-only).
            rest_host: "0.0.0.0".to_string(),
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
    fn cpu_enabled_renders_configured_core_count() {
        let cfg = NodeConfig {
            num_cpus: 8,
            ..NodeConfig::default()
        };
        let toml = render_config_toml(&cfg, &RunMode::Docker);
        assert!(toml.contains("[cpu]\n"));
        assert!(toml.contains("num_cpus = 8"));
    }

    #[test]
    fn cpu_disabled_omits_cpu_section_in_both_modes() {
        let cfg = NodeConfig {
            cpu_enabled: false,
            num_cpus: 8,
            ..NodeConfig::default()
        };
        for mode in [RunMode::Docker, RunMode::Native] {
            let toml = render_config_toml(&cfg, &mode);
            assert!(
                !toml.contains("[cpu]"),
                "{mode:?}: [cpu] rendered while disabled"
            );
            assert!(
                !toml.contains("num_cpus"),
                "{mode:?}: num_cpus rendered while disabled"
            );
        }
    }

    #[test]
    fn faucet_url_renders_in_both_modes() {
        // The miner has no built-in faucet default: without faucet_url a fresh
        // wallet fails fast with `wallet-underfunded`. Because the manager
        // writes its own config.toml (bypassing the upstream seed template),
        // both run modes must render the public testnet faucet.
        let cfg = NodeConfig::default();
        for mode in [RunMode::Docker, RunMode::Native] {
            let toml = render_config_toml(&cfg, &mode);
            assert!(
                toml.contains("faucet_url = \"https://faucet.testnet.quip.network\""),
                "{mode:?}: faucet_url missing from [miner]"
            );
        }
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

