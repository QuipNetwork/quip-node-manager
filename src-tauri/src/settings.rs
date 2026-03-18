// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GpuBackend {
    Local,
    Modal,
    Mps,
}

impl Default for GpuBackend {
    fn default() -> Self {
        GpuBackend::Local
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GpuDeviceConfig {
    pub index: u32,
    pub enabled: bool,
    pub utilization: u8,
    pub yielding: bool,
}

impl Default for GpuDeviceConfig {
    fn default() -> Self {
        GpuDeviceConfig {
            index: 0,
            enabled: true,
            utilization: 80,
            yielding: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct QpuConfig {
    /// D-Wave API key (DWAVE_API_KEY)
    pub api_key: String,
    /// D-Wave solver name (e.g. "Advantage_system6.4")
    pub solver: String,
    /// D-Wave region URL (e.g. "https://na-west-1.cloud.dwavesys.com/sapi/v2/")
    pub region_url: String,
    /// Daily QPU time budget (e.g. "60s", "5m")
    pub daily_budget: String,
}

fn default_port() -> u16 { 20049 }
fn default_listen() -> String { "::".to_string() }
fn default_num_cpus() -> u32 { 1 }
fn default_timeout() -> u32 { 3 }
fn default_heartbeat_interval() -> u32 { 15 }
fn default_heartbeat_timeout() -> u32 { 300 }
fn default_verify_ssl() -> bool { true }
fn default_log_level() -> String { "info".to_string() }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeConfig {
    // Network
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub public_host: String,
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default)]
    pub auto_mine: bool,
    // Identity
    #[serde(default)]
    pub secret: String,
    // CPU mining
    #[serde(default = "default_num_cpus")]
    pub num_cpus: u32,
    // GPU mining
    #[serde(default)]
    pub gpu_backend: GpuBackend,
    #[serde(default)]
    pub gpu_device_configs: Vec<GpuDeviceConfig>,
    // QPU
    #[serde(default)]
    pub qpu_config: Option<QpuConfig>,
    // Advanced
    #[serde(default = "default_timeout")]
    pub timeout: u32,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u32,
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: u32,
    #[serde(default)]
    pub fanout: Option<u32>,
    #[serde(default = "default_verify_ssl")]
    pub verify_ssl: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            port: 20049,
            listen: "::".to_string(),
            public_host: String::new(),
            node_name: String::new(),
            peers: vec![],
            auto_mine: false,
            secret: String::new(),
            num_cpus: 1,
            gpu_backend: GpuBackend::Local,
            gpu_device_configs: vec![],
            qpu_config: None,
            timeout: 3,
            heartbeat_interval: 15,
            heartbeat_timeout: 300,
            fanout: None,
            verify_ssl: true,
            log_level: "info".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub node_config: NodeConfig,
    pub active_tab: String,
    pub window_maximized: bool,
    pub image_tag: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            node_config: NodeConfig::default(),
            active_tab: "status".to_string(),
            window_maximized: false,
            image_tag: "cpu".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContainerStatus {
    pub running: bool,
    pub container_id: Option<String>,
    pub image: String,
    pub status_text: String,
}

pub fn data_dir() -> PathBuf {
    let home = dirs::home_dir().expect("cannot determine home directory");
    home.join("quip-data")
}

pub fn ensure_data_dir() -> Result<(), String> {
    fs::create_dir_all(data_dir()).map_err(|e| e.to_string())
}

fn settings_path() -> PathBuf {
    data_dir().join("app-settings.json")
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    if let Ok(content) = fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        AppSettings::default()
    }
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    ensure_data_dir()?;
    let content =
        serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(settings_path(), content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_settings() -> Result<AppSettings, String> {
    Ok(load_settings())
}

#[tauri::command]
pub async fn update_settings(settings: AppSettings) -> Result<(), String> {
    save_settings(&settings)
}
