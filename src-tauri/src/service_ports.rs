// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host port publishing. Docker service addresses and internal ports never change.

use crate::settings::{NodeConfig, RunMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub enabled: bool,
    pub host_port: u16,
}

impl PortBinding {
    const fn new(enabled: bool, host_port: u16) -> Self {
        Self { enabled, host_port }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServicePorts {
    pub validator_metrics: PortBinding,
    pub miner_rest: PortBinding,
    pub dashboard_http: PortBinding,
    pub postgres: PortBinding,
    pub caddy_http: PortBinding,
    pub caddy_https: PortBinding,
    pub caddy_http3: PortBinding,
    pub caddy_internal: PortBinding,
    pub caddy_admin: PortBinding,
    pub caddy_api_http3: PortBinding,
}

impl Default for ServicePorts {
    fn default() -> Self {
        Self {
            validator_metrics: PortBinding::new(false, 9615),
            miner_rest: PortBinding::new(false, 8086),
            dashboard_http: PortBinding::new(false, 3001),
            postgres: PortBinding::new(false, 5432),
            caddy_http: PortBinding::new(true, 80),
            caddy_https: PortBinding::new(true, 443),
            caddy_http3: PortBinding::new(false, 443),
            caddy_internal: PortBinding::new(false, 8088),
            caddy_admin: PortBinding::new(false, 2019),
            caddy_api_http3: PortBinding::new(false, 20049),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortId {
    ValidatorP2p,
    ValidatorRpc,
    ValidatorMetrics,
    MinerRest,
    DashboardHttp,
    Postgres,
    PublicApi,
    CaddyHttp,
    CaddyHttps,
    CaddyHttp3,
    CaddyInternal,
    CaddyAdmin,
    CaddyApiHttp3,
}

#[derive(Clone, Serialize)]
pub struct PortSpec {
    pub id: PortId,
    pub service: &'static str,
    pub label: &'static str,
    pub container_port: u16,
    pub protocols: &'static [&'static str],
    pub port_path: &'static str,
    pub enabled_path: &'static str,
}

pub const PORT_SPECS: &[PortSpec] = &[
    PortSpec {
        id: PortId::ValidatorP2p,
        service: "quip-validator",
        label: "P2P",
        container_port: 30333,
        protocols: &["tcp", "udp"],
        port_path: "validator_port",
        enabled_path: "validator_p2p_enabled",
    },
    PortSpec {
        id: PortId::ValidatorRpc,
        service: "quip-validator",
        label: "RPC",
        container_port: 9944,
        protocols: &["tcp"],
        port_path: "validator_rpc_port",
        enabled_path: "validator_rpc_enabled",
    },
    PortSpec {
        id: PortId::ValidatorMetrics,
        service: "quip-validator",
        label: "Metrics",
        container_port: 9615,
        protocols: &["tcp"],
        port_path: "service_ports.validator_metrics.host_port",
        enabled_path: "service_ports.validator_metrics.enabled",
    },
    PortSpec {
        id: PortId::MinerRest,
        service: "quip-miner",
        label: "REST",
        container_port: 8086,
        protocols: &["tcp"],
        port_path: "service_ports.miner_rest.host_port",
        enabled_path: "service_ports.miner_rest.enabled",
    },
    PortSpec {
        id: PortId::DashboardHttp,
        service: "quip-dashboard",
        label: "HTTP",
        container_port: 3001,
        protocols: &["tcp"],
        port_path: "service_ports.dashboard_http.host_port",
        enabled_path: "service_ports.dashboard_http.enabled",
    },
    PortSpec {
        id: PortId::Postgres,
        service: "quip-postgres",
        label: "PostgreSQL",
        container_port: 5432,
        protocols: &["tcp"],
        port_path: "service_ports.postgres.host_port",
        enabled_path: "service_ports.postgres.enabled",
    },
    PortSpec {
        id: PortId::PublicApi,
        service: "quip-caddy",
        label: "Public API",
        container_port: 20049,
        protocols: &["tcp"],
        port_path: "port",
        enabled_path: "public_api_enabled",
    },
    PortSpec {
        id: PortId::CaddyHttp,
        service: "quip-caddy",
        label: "HTTP",
        container_port: 80,
        protocols: &["tcp"],
        port_path: "service_ports.caddy_http.host_port",
        enabled_path: "service_ports.caddy_http.enabled",
    },
    PortSpec {
        id: PortId::CaddyHttps,
        service: "quip-caddy",
        label: "HTTPS",
        container_port: 443,
        protocols: &["tcp"],
        port_path: "service_ports.caddy_https.host_port",
        enabled_path: "service_ports.caddy_https.enabled",
    },
    PortSpec {
        id: PortId::CaddyHttp3,
        service: "quip-caddy",
        label: "HTTP/3",
        container_port: 443,
        protocols: &["udp"],
        port_path: "service_ports.caddy_http3.host_port",
        enabled_path: "service_ports.caddy_http3.enabled",
    },
    PortSpec {
        id: PortId::CaddyInternal,
        service: "quip-caddy",
        label: "Internal routing",
        container_port: 8088,
        protocols: &["tcp"],
        port_path: "service_ports.caddy_internal.host_port",
        enabled_path: "service_ports.caddy_internal.enabled",
    },
    PortSpec {
        id: PortId::CaddyAdmin,
        service: "quip-caddy",
        label: "Admin API",
        container_port: 2019,
        protocols: &["tcp"],
        port_path: "service_ports.caddy_admin.host_port",
        enabled_path: "service_ports.caddy_admin.enabled",
    },
    PortSpec {
        id: PortId::CaddyApiHttp3,
        service: "quip-caddy",
        label: "Public API HTTP/3",
        container_port: 20049,
        protocols: &["udp"],
        port_path: "service_ports.caddy_api_http3.host_port",
        enabled_path: "service_ports.caddy_api_http3.enabled",
    },
];

impl PortId {
    pub fn required(self, mode: &RunMode) -> bool {
        *mode == RunMode::Native && matches!(self, Self::ValidatorRpc | Self::MinerRest)
    }

    pub fn binding(self, config: &NodeConfig, mode: &RunMode) -> PortBinding {
        let ports = &config.service_ports;
        match self {
            Self::ValidatorP2p => {
                PortBinding::new(config.validator_p2p_enabled, config.validator_port)
            }
            Self::ValidatorRpc => {
                PortBinding::new(config.validator_rpc_enabled, config.validator_rpc_port)
            }
            Self::PublicApi => PortBinding::new(config.public_api_enabled, config.port),
            Self::MinerRest if *mode == RunMode::Native => PortBinding::new(
                ports.miner_rest.enabled,
                crate::config::native_rest_port(config),
            ),
            Self::MinerRest => ports.miner_rest,
            Self::ValidatorMetrics => ports.validator_metrics,
            Self::DashboardHttp => ports.dashboard_http,
            Self::Postgres => ports.postgres,
            Self::CaddyHttp => ports.caddy_http,
            Self::CaddyHttps => ports.caddy_https,
            Self::CaddyHttp3 => ports.caddy_http3,
            Self::CaddyInternal => ports.caddy_internal,
            Self::CaddyAdmin => ports.caddy_admin,
            Self::CaddyApiHttp3 => ports.caddy_api_http3,
        }
    }

    pub fn set_binding(self, config: &mut NodeConfig, mode: &RunMode, binding: PortBinding) {
        match self {
            Self::ValidatorP2p => {
                config.validator_p2p_enabled = binding.enabled;
                config.validator_port = binding.host_port;
            }
            Self::ValidatorRpc => {
                config.validator_rpc_enabled = binding.enabled;
                config.validator_rpc_port = binding.host_port;
            }
            Self::PublicApi => {
                config.public_api_enabled = binding.enabled;
                config.port = binding.host_port;
            }
            Self::MinerRest if *mode == RunMode::Native => {
                config.rest_insecure_port = i32::from(binding.host_port);
            }
            Self::MinerRest => config.service_ports.miner_rest = binding,
            Self::ValidatorMetrics => config.service_ports.validator_metrics = binding,
            Self::DashboardHttp => config.service_ports.dashboard_http = binding,
            Self::Postgres => config.service_ports.postgres = binding,
            Self::CaddyHttp => config.service_ports.caddy_http = binding,
            Self::CaddyHttps => config.service_ports.caddy_https = binding,
            Self::CaddyHttp3 => config.service_ports.caddy_http3 = binding,
            Self::CaddyInternal => config.service_ports.caddy_internal = binding,
            Self::CaddyAdmin => config.service_ports.caddy_admin = binding,
            Self::CaddyApiHttp3 => config.service_ports.caddy_api_http3 = binding,
        }
    }
}

pub fn validate(config: &NodeConfig, mode: &RunMode) -> Result<(), String> {
    if *mode == RunMode::Native
        && (config.rest_insecure_port == 0
            || config.rest_insecure_port > 65535
            || (config.rest_insecure_port < 0 && config.rest_port > 65535))
    {
        return Err("quip-miner REST: choose a host port from 1 to 65535".into());
    }
    let mut used = std::collections::BTreeMap::new();
    for spec in PORT_SPECS {
        let binding = spec.id.binding(config, mode);
        if binding.host_port == 0 {
            return Err(format!(
                "{} {}: choose a host port from 1 to 65535",
                spec.service, spec.label
            ));
        }
        if binding.enabled || spec.id.required(mode) {
            for protocol in spec.protocols {
                if let Some(previous) = used.insert((binding.host_port, protocol), spec) {
                    return Err(format!(
                        "{} {} and {} {} both use host port {}/{protocol}; choose different ports",
                        previous.service,
                        previous.label,
                        spec.service,
                        spec.label,
                        binding.host_port
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_saved_ports_keep_their_values_and_new_publications_default_off() {
        let config: NodeConfig = serde_json::from_value(serde_json::json!({
            "port": 21049, "validator_port": 31333, "validator_rpc_port": 29944,
        }))
        .unwrap();
        assert_eq!(
            PortId::PublicApi.binding(&config, &RunMode::Docker),
            PortBinding::new(true, 21049)
        );
        assert_eq!(
            PortId::ValidatorRpc.binding(&config, &RunMode::Docker),
            PortBinding::new(false, 29944)
        );
        assert!(!config.service_ports.postgres.enabled);
        validate(&config, &RunMode::Docker).unwrap();
    }

    #[test]
    fn duplicate_tcp_publications_are_rejected_but_tcp_and_udp_can_share_a_port() {
        let mut config = NodeConfig::default();
        config.service_ports.caddy_http3.enabled = true;
        validate(&config, &RunMode::Docker).unwrap();
        config.validator_rpc_port = 20049;
        validate(&config, &RunMode::Docker).unwrap(); // Disabled RPC does not bind.
        config.validator_rpc_enabled = true;
        assert!(validate(&config, &RunMode::Docker)
            .unwrap_err()
            .contains("20049/tcp"));
    }

    #[test]
    fn native_required_ports_participate_in_conflict_checks_even_with_switch_off() {
        let mut config = NodeConfig {
            validator_rpc_port: 20049,
            ..NodeConfig::default()
        };
        assert!(validate(&config, &RunMode::Native).is_err());
        config.validator_rpc_port = 9944;
        config.rest_insecure_port = 9944;
        assert!(validate(&config, &RunMode::Native).is_err());
        config.rest_insecure_port = 50000;
        validate(&config, &RunMode::Native).unwrap();
        assert_eq!(
            PortId::MinerRest
                .binding(&config, &RunMode::Native)
                .host_port,
            50000
        );
    }

    #[test]
    fn invalid_native_port_cannot_wrap_or_fall_back() {
        for port in [0, 65536, 70000] {
            let config = NodeConfig {
                rest_insecure_port: port,
                ..NodeConfig::default()
            };
            assert!(validate(&config, &RunMode::Native).is_err());
        }
    }
}

#[derive(Serialize)]
pub struct PortControl {
    #[serde(flatten)]
    pub spec: PortSpec,
    pub binding: PortBinding,
    pub required: bool,
    pub public_access_optional: bool,
}

#[tauri::command]
pub fn get_service_ports(config: NodeConfig, run_mode: RunMode) -> Vec<PortControl> {
    PORT_SPECS
        .iter()
        .map(|spec| {
            let mut spec = spec.clone();
            if spec.id == PortId::MinerRest && run_mode == RunMode::Native {
                spec.port_path = "rest_insecure_port";
            }
            PortControl {
                binding: spec.id.binding(&config, &run_mode),
                required: spec.id.required(&run_mode),
                public_access_optional: spec.id == PortId::ValidatorRpc
                    && run_mode == RunMode::Native,
                spec,
            }
        })
        .collect()
}
