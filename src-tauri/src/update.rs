// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImageUpdateInfo {
    pub current_digest: String,
    pub latest_digest: String,
    pub update_available: bool,
}

#[derive(Deserialize)]
struct GitLabRelease {
    name: String,
    description: Option<String>,
    #[serde(rename = "_links")]
    links: ReleaseLinks,
}

#[derive(Deserialize)]
struct ReleaseLinks {
    #[serde(rename = "self")]
    self_url: String,
}

fn parse_semver(v: &str) -> (u64, u64, u64) {
    let v = v.trim_start_matches('v');
    let parts: Vec<&str> = v.split('.').collect();
    let major = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

#[tauri::command]
pub async fn check_app_update() -> Result<Option<UpdateInfo>, String> {
    let current = env!("CARGO_PKG_VERSION");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let url = "https://gitlab.com/api/v4/projects/piqued%2Fquip-node-manager/releases";
    let releases: Vec<GitLabRelease> = match client
        .get(url)
        .header("User-Agent", "quip-node-manager")
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => return Ok(None),
    };

    let Some(latest) = releases.into_iter().next() else {
        return Ok(None);
    };

    let latest_version = latest.name.trim_start_matches('v');
    if parse_semver(latest_version) > parse_semver(current) {
        Ok(Some(UpdateInfo {
            version: latest_version.to_string(),
            url: latest.links.self_url,
            notes: latest.description.unwrap_or_default(),
        }))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn check_image_update(image_tag: String) -> Result<Option<ImageUpdateInfo>, String> {
    // For now: attempt HEAD request to registry to get digest
    // GitLab registry requires auth for manifests, so gracefully degrade
    let image_name = if image_tag == "cuda" {
        "quip-network-node-cuda"
    } else {
        "quip-network-node-cpu"
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let manifest_url = format!(
        "https://registry.gitlab.com/v2/piqued/quip-protocol/{}/manifests/latest",
        image_name
    );

    let resp = match client
        .head(&manifest_url)
        .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let digest = resp
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if digest.is_empty() {
        return Ok(None);
    }

    // Get current local digest
    let local_output = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{index .RepoDigests 0}}",
            &format!("registry.gitlab.com/piqued/quip-protocol/{}:latest", image_name)])
        .output()
        .ok();

    let current_digest = local_output
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let update_available = !current_digest.is_empty() && !current_digest.contains(&digest);

    Ok(Some(ImageUpdateInfo {
        current_digest,
        latest_digest: digest,
        update_available,
    }))
}
