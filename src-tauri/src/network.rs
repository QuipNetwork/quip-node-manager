// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

#[tauri::command]
pub async fn detect_public_ip() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get("https://api.ipify.org")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ip = resp.text().await.map_err(|e| e.to_string())?;
    Ok(ip.trim().to_string())
}
