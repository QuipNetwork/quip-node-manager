// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::data_dir;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize)]
struct NodeSecret {
    secret: String,
}

fn secret_path() -> std::path::PathBuf {
    data_dir().join("node-secret.json")
}

fn generate_hex_secret() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
    hex::encode(bytes)
}

#[tauri::command]
pub async fn get_node_secret() -> Result<String, String> {
    let path = secret_path();
    if path.exists() {
        let content =
            fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let ns: NodeSecret =
            serde_json::from_str(&content).map_err(|e| e.to_string())?;
        Ok(ns.secret)
    } else {
        generate_node_secret().await
    }
}

#[tauri::command]
pub async fn generate_node_secret() -> Result<String, String> {
    crate::settings::ensure_data_dir()?;
    let secret = generate_hex_secret();
    let ns = NodeSecret {
        secret: secret.clone(),
    };
    let content =
        serde_json::to_string_pretty(&ns).map_err(|e| e.to_string())?;
    fs::write(secret_path(), content).map_err(|e| e.to_string())?;
    Ok(secret)
}
