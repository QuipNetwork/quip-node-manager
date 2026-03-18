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

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct QpuConfig {
    pub url: String,
    pub api_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeConfig {
    pub num_cpus: u32,
    pub gpu_backend: GpuBackend,
    pub gpu_devices: Vec<u32>,
    pub gpu_utilization: u8,
    pub qpu_configs: Vec<QpuConfig>,
    pub peers: Vec<String>,
    pub port: u16,
    pub secret: String,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            num_cpus: 2,
            gpu_backend: GpuBackend::Local,
            gpu_devices: vec![],
            gpu_utilization: 80,
            qpu_configs: vec![],
            peers: vec![],
            port: 20049,
            secret: String::new(),
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
