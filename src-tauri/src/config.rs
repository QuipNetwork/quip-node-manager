// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, GpuBackend, NodeConfig, RunMode};
use std::fs;

const DEFAULT_PEERS: &[&str] = &[
    "qpu-1.nodes.quip.network:20049",
    "cpu-1.quip.carback.us:20049",
    "gpu-1.quip.carback.us:20049",
    "gpu-2.quip.carback.us:20050",
    "nodes.quip.network:20049",
];

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
    // Docker mode: the node always binds the container-internal port (20049)
    // and compose remaps the host side. Native mode: the node binds whatever
    // the user configured, since there's no container in between.
    let bind_port = if is_docker { 20049 } else { config.port };
    out.push_str(&format!("port = {}\n", bind_port));
    if !config.public_host.is_empty() {
        out.push_str(&format!(
            "public_host = \"{}\"\n",
            config.public_host
        ));
    }
    // public_port tells peers which port to dial back on. Explicit user
    // value wins; otherwise, in Docker mode, announce the user-facing port
    // whenever it differs from the internal bind (i.e. the host side of the
    // publish has been remapped).
    let effective_public_port = config.public_port.or_else(|| {
        if bind_port != config.port {
            Some(config.port)
        } else {
            None
        }
    });
    if let Some(pp) = effective_public_port {
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
    if config.peers.is_empty() {
        let peer_strs: Vec<String> =
            DEFAULT_PEERS.iter().map(|p| format!("\"{}\"", p)).collect();
        out.push_str(&format!("peer = [{}]\n", peer_strs.join(", ")));
    } else {
        let peer_strs: Vec<String> =
            config.peers.iter().map(|p| format!("\"{}\"", p)).collect();
        out.push_str(&format!("peer = [{}]\n", peer_strs.join(", ")));
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
        // In Docker the node process runs with cwd=/app as a non-root user,
        // so a relative path like "telemetry" resolves to /app/telemetry
        // which isn't writable. Force an absolute path under /data.
        let telemetry_dir = if is_docker {
            "/data/telemetry".to_string()
        } else {
            config.telemetry_dir.clone()
        };
        out.push_str(&format!(
            "telemetry_dir = \"{}\"\n",
            telemetry_dir
        ));
    }
    out.push('\n');

    // ── [cpu] ───────────────────────────────────────────────────────────
    out.push_str("[cpu]\n");
    out.push_str(&format!("num_cpus = {}\n", config.num_cpus));
    out.push('\n');

    // ── GPU sections ────────────────────────────────────────────────────
    // [gpu] holds global defaults inherited by every backend section
    // ([cuda.N], [metal], [modal]). See quip-protocol/quip-node.example.toml.
    //
    // Emission rules:
    //   - A backend section is only written if the user has at least one
    //     GPU device enabled. With zero devices enabled, emitting an
    //     empty [metal] / [modal] still activates that backend in the
    //     node — which then fails to build its (zero) miners.
    //   - Metal is unavailable in Linux containers regardless of what
    //     the Mac host reports. In Docker mode we suppress Mps.
    let enabled_devices: Vec<&crate::settings::GpuDeviceConfig> = config
        .gpu_device_configs
        .iter()
        .filter(|d| d.enabled)
        .collect();

    // Effective backend: Mps is clamped to "no backend" in Docker mode
    // because the container is Linux and has no Metal access.
    let effective_backend =
        if is_docker && config.gpu_backend == GpuBackend::Mps {
            None
        } else {
            Some(config.gpu_backend.clone())
        };

    let (gpu_util, gpu_yield) = enabled_devices
        .first()
        .map(|d| (d.utilization, d.yielding))
        .unwrap_or((100, false));

    let emit_backend =
        effective_backend.is_some() && !enabled_devices.is_empty();

    if emit_backend {
        out.push_str("[gpu]\n");
        out.push_str(&format!("utilization = {}\n", gpu_util));
        out.push_str(&format!("yielding = {}\n", gpu_yield));
        out.push('\n');
    }

    if emit_backend {
        match effective_backend {
            Some(GpuBackend::Local) => {
                for dev in &enabled_devices {
                    out.push_str(&format!("[cuda.{}]\n", dev.index));
                    if dev.utilization != gpu_util {
                        out.push_str(&format!(
                            "utilization = {}\n",
                            dev.utilization
                        ));
                    }
                    if dev.yielding != gpu_yield {
                        out.push_str(&format!(
                            "yielding = {}\n",
                            dev.yielding
                        ));
                    }
                    out.push('\n');
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
    // Docker mode: compose bind-mounts `./data:/data` (relative to the
    // project-directory), so the container sees `/data/config.toml` as
    // `<data_dir>/data/config.toml` on the host. Writing to the bare
    // `<data_dir>/config.toml` (native's location) would land it outside
    // the mount and the node would never read it — falling back to
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
pub async fn generate_config_toml(
    config: NodeConfig,
    run_mode: RunMode,
) -> Result<String, String> {
    Ok(render_config_toml(&config, &run_mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::GpuDeviceConfig;

    fn cfg_with_gpu(
        backend: GpuBackend,
        devices: Vec<GpuDeviceConfig>,
    ) -> NodeConfig {
        NodeConfig {
            gpu_backend: backend,
            gpu_device_configs: devices,
            ..NodeConfig::default()
        }
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
}
