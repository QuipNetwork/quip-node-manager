// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

#[tauri::command]
pub async fn detect_public_ip() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get("https://ifconfig.co/ip")
        .header("Accept", "text/plain")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ip = resp.text().await.map_err(|e| e.to_string())?;
    Ok(ip.trim().to_string())
}

#[tauri::command]
pub async fn check_port_forwarding(ip: String, port: u16) -> Result<bool, String> {
    let addr = format!("{}:{}", ip, port);
    match timeout(Duration::from_secs(5), TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(true),
        Ok(Err(_)) => Ok(false),
        Err(_) => Ok(false), // timeout
    }
}
