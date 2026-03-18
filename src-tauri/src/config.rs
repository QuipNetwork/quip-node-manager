// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, NodeConfig};
use std::fs;

fn render_config_toml(config: &NodeConfig) -> String {
    let mut out = String::new();

    out.push_str("[node]\n");
    out.push_str(&format!("port = {}\n", config.port));
    out.push_str(&format!("secret = \"{}\"\n", config.secret));
    out.push('\n');

    out.push_str("[network]\n");
    if config.peers.is_empty() {
        out.push_str("peers = []\n");
    } else {
        let peers: Vec<String> =
            config.peers.iter().map(|p| format!("\"{}\"", p)).collect();
        out.push_str(&format!("peers = [{}]\n", peers.join(", ")));
    }
    out.push('\n');

    for qpu in &config.qpu_configs {
        out.push_str("[[qpu]]\n");
        out.push_str(&format!("url = \"{}\"\n", qpu.url));
        out.push_str(&format!("api_key = \"{}\"\n", qpu.api_key));
        out.push('\n');
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
