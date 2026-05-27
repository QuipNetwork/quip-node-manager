// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::Emitter;

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

pub fn parse_semver(v: &str) -> (u64, u64, u64) {
    let v = v.trim_start_matches('v');
    let parts: Vec<&str> = v.split('.').collect();
    let major = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub async fn get_node_version() -> Option<String> {
    // Native mode: the binary is on disk, `--version` is cheap.
    // Docker mode: we used to do `docker run --rm <image> --version`, but
    // if the image entrypoint doesn't treat --version as a no-op it
    // starts a full node in an anonymous container (observed as a random
    // "confident_lehmann" container that sits alongside the compose stack
    // and can't be reaped by `docker compose down`). Not worth the risk
    // for a title-bar label — skip and let the dashboard show the
    // running node's own version instead.
    tokio::task::spawn_blocking(|| {
        let settings = crate::settings::load_settings();
        match settings.run_mode {
            crate::settings::RunMode::Native => {
                crate::native::installed_binary_version()
            }
            crate::settings::RunMode::Docker => None,
        }
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
pub async fn check_app_update() -> Result<Option<UpdateInfo>, String> {
    let current = env!("CARGO_PKG_VERSION");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let url = "https://gitlab.com/api/v4/projects/quip.network%2Fquip-node-manager/releases";
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

/// Which compose image a digest check is running against. Used by the
/// background monitor to iterate over the whole stack, and by each
/// Tauri-facing check wrapper to keep serialisation shapes unchanged.
///
/// Postgres and Caddy are deliberately absent: they use pinned version
/// tags (`postgres:16`, `caddy:2-alpine`), not `:latest`, so point
/// releases come in via routine `docker compose pull` rather than
/// silent digest drift.
#[derive(Clone, Copy, Debug)]
pub enum ImageRef {
    Node(crate::settings::ImageTag),
    Dashboard,
}

impl ImageRef {
    fn gitlab_path(&self) -> &'static str {
        match self {
            ImageRef::Node(crate::settings::ImageTag::Cuda) => {
                "quip.network/quip-protocol/quip-network-node-cuda"
            }
            ImageRef::Node(crate::settings::ImageTag::Cpu) => {
                "quip.network/quip-protocol/quip-network-node-cpu"
            }
            ImageRef::Dashboard => "quip.network/dashboard.quip.network",
        }
    }

    fn local_ref(&self) -> String {
        format!("registry.gitlab.com/{}:latest", self.gitlab_path())
    }

    /// Human label used by the UI for update toasts.
    pub fn display_name(&self) -> &'static str {
        match self {
            ImageRef::Node(crate::settings::ImageTag::Cuda) => "Node (CUDA)",
            ImageRef::Node(crate::settings::ImageTag::Cpu) => "Node (CPU)",
            ImageRef::Dashboard => "Dashboard",
        }
    }
}

/// The images whose latest-tag digests are worth polling for the given
/// settings + run_mode. Native mode drops the node image (it runs on the
/// host); `dashboard_enabled == false` drops the dashboard image.
fn relevant_images(settings: &crate::settings::AppSettings) -> Vec<ImageRef> {
    let mut v = Vec::new();
    if settings.run_mode == crate::settings::RunMode::Docker {
        v.push(ImageRef::Node(settings.image_tag));
    }
    if settings.dashboard_enabled {
        v.push(ImageRef::Dashboard);
    }
    v
}

/// Core GitLab registry digest probe — HEAD the manifest, diff against the
/// local `docker image inspect` digest. Gracefully degrades to `Ok(None)`
/// when the registry requires auth or the image isn't present locally.
async fn check_gitlab_image_update(
    image: ImageRef,
) -> Result<Option<ImageUpdateInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let manifest_url = format!(
        "https://registry.gitlab.com/v2/{}/manifests/latest",
        image.gitlab_path()
    );

    let resp = match client
        .head(&manifest_url)
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json",
        )
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

    let inspect_image = image.local_ref();
    let current_digest = tokio::task::spawn_blocking(move || {
        crate::cmd::new("docker")
            .args([
                "image",
                "inspect",
                "--format",
                "{{index .RepoDigests 0}}",
                &inspect_image,
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            })
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    let update_available =
        !current_digest.is_empty() && !current_digest.contains(&digest);

    Ok(Some(ImageUpdateInfo {
        current_digest,
        latest_digest: digest,
        update_available,
    }))
}

#[tauri::command]
pub async fn check_image_update(
    image_tag: crate::settings::ImageTag,
) -> Result<Option<ImageUpdateInfo>, String> {
    check_gitlab_image_update(ImageRef::Node(image_tag)).await
}

#[tauri::command]
pub async fn check_dashboard_image_update(
) -> Result<Option<ImageUpdateInfo>, String> {
    check_gitlab_image_update(ImageRef::Dashboard).await
}

/// Background task that checks for updates every 30 minutes.
/// - Docker mode: checks for new image digest
/// - Native mode: checks for new binary release
/// - Always: checks for new node-manager app release
pub async fn background_update_monitor(app: tauri::AppHandle) {
    let mut interval =
        tokio::time::interval(Duration::from_secs(30 * 60));
    // Skip the first immediate tick
    interval.tick().await;

    loop {
        interval.tick().await;

        let settings = crate::settings::load_settings();

        // Check for node-manager app updates
        if let Ok(Some(info)) = check_app_update().await {
            let _ = app.emit("app-update-available", &info);
            crate::set_tray_update(
                &app,
                true,
                &format!("Quip Node Manager — v{} available", info.version),
            );
        }

        // Compose-image checks — applies in Docker mode, and in Native mode
        // whenever the dashboard service is running (dashboard + postgres
        // + maybe caddy). `relevant_images` filters the set correctly.
        let mut any_compose_update = false;
        for image in relevant_images(&settings) {
            if let Ok(Some(info)) = check_gitlab_image_update(image).await {
                if info.update_available {
                    let _ = app.emit(
                        "image-update-available",
                        serde_json::json!({
                            "image": image.display_name(),
                            "info": info,
                        }),
                    );
                    any_compose_update = true;
                }
            }
        }

        if any_compose_update && settings.auto_update_enabled {
            emit_log(
                &app,
                "[Auto-Update] New stack image detected, restarting...",
            );
            let _ = crate::compose::stop_stack(app.clone()).await;
            let _ = crate::compose::pull_compose_images(app.clone()).await;
            let _ = crate::compose::start_stack(app.clone()).await;
            emit_log(&app, "[Auto-Update] Restart complete.");
        }

        // Native binary: separate channel because the binary is not a
        // container image and lives on GitLab Releases, not the registry.
        if settings.run_mode == crate::settings::RunMode::Native {
            if let Ok(Some(info)) =
                crate::native::check_binary_update().await
            {
                let _ = app.emit("binary-update-available", &info);

                if settings.auto_update_enabled {
                    emit_log(
                        &app,
                        &format!(
                            "[Auto-Update] New binary v{} available, downloading...",
                            info.version
                        ),
                    );
                    let _ = crate::native::download_native_binary(
                        app.clone(),
                    )
                    .await;
                    emit_log(&app, "[Auto-Update] Binary updated.");
                }
            }
        }
    }
}

fn emit_log(app: &tauri::AppHandle, msg: &str) {
    let _ = app.emit(
        "node-log",
        serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": msg,
        }),
    );
}
