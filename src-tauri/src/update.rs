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

/// Parse a version like `v0.2.0`, `0.2.0`, or `0.2.0-rc1` into a comparable
/// tuple `(major, minor, patch, prerelease)`.
///
/// A final release sorts ABOVE any pre-release of the same `major.minor.patch`
/// (`0.2.0` > `0.2.0-rc2`), and pre-releases sort among themselves by their
/// trailing number (`0.2.0-rc1` < `0.2.0-rc2`). Without pre-release ordering
/// the in-app updater never offers an `rc` → `rc` bump, because the entire
/// `-rcN` suffix parses to nothing and both versions collapse to the same
/// `(major, minor, patch)`. Unparseable components default to 0.
pub fn parse_semver(v: &str) -> (u64, u64, u64, u64) {
    let v = v.trim_start_matches('v');
    let (core, prerelease) = match v.split_once('-') {
        // A release has no pre-release suffix, so it outranks every `-rcN`.
        None => (v, u64::MAX),
        Some((core, pre)) => (core, parse_prerelease(pre)),
    };
    let mut parts = core.split('.');
    let major = next_number(&mut parts);
    let minor = next_number(&mut parts);
    let patch = next_number(&mut parts);
    (major, minor, patch, prerelease)
}

fn next_number<'a>(parts: &mut impl Iterator<Item = &'a str>) -> u64 {
    parts.next().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Extract the trailing integer from a pre-release identifier (`rc1` → 1,
/// `rc.2` → 2). Used only to order pre-releases against each other; the
/// identifier text itself is not significant.
fn parse_prerelease(pre: &str) -> u64 {
    let digits: String = pre.chars().filter(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
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
            crate::settings::RunMode::Native => crate::native::installed_binary_version(),
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
/// Postgres and Caddy are deliberately absent: they use Docker Hub version
/// tags (`postgres:16`, `caddy:2-alpine`), so point releases come in via
/// routine `docker compose pull` rather than GitLab registry checks.
#[derive(Clone, Copy, Debug)]
pub enum ImageRef {
    Miner(crate::settings::ImageTag),
    Validator,
    Dashboard,
}

impl ImageRef {
    fn repository(&self) -> &'static str {
        match self {
            ImageRef::Miner(image_tag) => crate::compose::image_for_tag(*image_tag),
            ImageRef::Validator => crate::compose::VALIDATOR_IMAGE,
            ImageRef::Dashboard => crate::compose::DASHBOARD_IMAGE,
        }
    }

    fn gitlab_path(&self) -> &'static str {
        self.repository()
            .strip_prefix("registry.gitlab.com/")
            .unwrap_or_else(|| self.repository())
    }

    fn tag(&self) -> &'static str {
        match self {
            ImageRef::Miner(_) | ImageRef::Validator | ImageRef::Dashboard => {
                crate::compose::COMPOSE_IMAGE_TAG
            }
        }
    }

    fn local_ref(&self) -> String {
        format!("{}:{}", self.repository(), self.tag())
    }

    /// Human label used by the UI for update toasts.
    pub fn display_name(&self) -> &'static str {
        match self {
            ImageRef::Miner(crate::settings::ImageTag::Cuda) => "Miner (CUDA)",
            ImageRef::Miner(crate::settings::ImageTag::Cpu) => "Miner (CPU)",
            ImageRef::Validator => "Validator",
            ImageRef::Dashboard => "Dashboard",
        }
    }
}

/// The images whose configured-tag digests are worth polling for the given
/// settings + run_mode. Native mode drops Docker miner/validator images (the
/// miner binary runs on the host). Dashboard-disabled mode is unsupported in
/// v0.2, so the dashboard image is always relevant.
fn relevant_images(settings: &crate::settings::AppSettings) -> Vec<ImageRef> {
    let mut v = Vec::new();
    if settings.run_mode == crate::settings::RunMode::Docker {
        v.push(ImageRef::Miner(settings.image_tag));
        v.push(ImageRef::Validator);
    }
    v.push(ImageRef::Dashboard);
    v
}

/// Core GitLab registry digest probe — HEAD the configured tag's manifest,
/// diff against the local `docker image inspect` digest. Gracefully degrades
/// to `Ok(None)` when the registry requires auth or the image isn't present
/// locally.
async fn check_gitlab_image_update(image: ImageRef) -> Result<Option<ImageUpdateInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let manifest_url = format!(
        "https://registry.gitlab.com/v2/{}/manifests/{}",
        image.gitlab_path(),
        image.tag()
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
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    let update_available = !current_digest.is_empty() && !current_digest.contains(&digest);

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
    check_gitlab_image_update(ImageRef::Miner(image_tag)).await
}

#[tauri::command]
pub async fn check_dashboard_image_update() -> Result<Option<ImageUpdateInfo>, String> {
    check_gitlab_image_update(ImageRef::Dashboard).await
}

pub async fn check_docker_core_image_update(
    image_tag: crate::settings::ImageTag,
) -> Result<Option<(ImageRef, ImageUpdateInfo)>, String> {
    for image in [ImageRef::Miner(image_tag), ImageRef::Validator] {
        if let Some(info) = check_gitlab_image_update(image).await? {
            if info.update_available {
                return Ok(Some((image, info)));
            }
        }
    }
    Ok(None)
}

/// Background task that checks for updates every 30 minutes.
/// - Docker mode: checks for new image digest
/// - Native mode: checks for new binary release
/// - Always: checks for new node-manager app release
pub async fn background_update_monitor(app: tauri::AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_secs(30 * 60));
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
            // Bail on the first failing step and report it — never claim
            // "Restart complete." after a swallowed error, which would tell
            // the user their node is up when stop succeeded but start didn't.
            match auto_update_restart_stack(&app).await {
                Ok(()) => emit_log(&app, "[Auto-Update] Restart complete."),
                Err(e) => emit_error(
                    &app,
                    &format!(
                        "[Auto-Update] Restart failed: {e}. Your node may be \
                         stopped — open the app and start it manually."
                    ),
                ),
            }
        }

        // Native binary: separate channel because the binary is not a
        // container image and lives on GitLab Releases, not the registry.
        if settings.run_mode == crate::settings::RunMode::Native {
            if let Ok(Some(info)) = crate::native::check_binary_update().await {
                let _ = app.emit("binary-update-available", &info);

                if settings.auto_update_enabled {
                    emit_log(
                        &app,
                        &format!(
                            "[Auto-Update] New binary v{} available, downloading...",
                            info.version
                        ),
                    );
                    match crate::native::download_native_binary(app.clone()).await {
                        Ok(_) => emit_log(&app, "[Auto-Update] Binary updated."),
                        Err(e) => {
                            emit_error(&app, &format!("[Auto-Update] Binary download failed: {e}"))
                        }
                    }
                }
            }
        }
    }
}

/// Stop → pull → start the compose stack, short-circuiting on the first error
/// so a failed pull doesn't leave the stack torn down with a misleading
/// success message.
async fn auto_update_restart_stack(app: &tauri::AppHandle) -> Result<(), String> {
    crate::compose::stop_stack(app.clone()).await?;
    crate::compose::pull_compose_images(app.clone()).await?;
    crate::compose::start_stack(app.clone()).await?;
    Ok(())
}

fn emit_log(app: &tauri::AppHandle, msg: &str) {
    emit_level(app, "INFO", msg);
}

fn emit_error(app: &tauri::AppHandle, msg: &str) {
    emit_level(app, "ERROR", msg);
}

fn emit_level(app: &tauri::AppHandle, level: &str, msg: &str) {
    let _ = app.emit(
        "node-log",
        serde_json::json!({
            "timestamp": "",
            "level": level,
            "message": msg,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AppSettings, ImageTag, RunMode};

    #[test]
    fn image_refs_use_v02_repositories() {
        let refs = [
            (
                ImageRef::Miner(ImageTag::Cpu),
                "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cpu:v0.2",
                "quip.network/quip-protocol/quip-miner-cpu",
                "Miner (CPU)",
            ),
            (
                ImageRef::Miner(ImageTag::Cuda),
                "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda:v0.2",
                "quip.network/quip-protocol/quip-miner-cuda",
                "Miner (CUDA)",
            ),
            (
                ImageRef::Validator,
                "registry.gitlab.com/quip.network/quip-protocol-rs/quip-network-node:v0.2",
                "quip.network/quip-protocol-rs/quip-network-node",
                "Validator",
            ),
            (
                ImageRef::Dashboard,
                "registry.gitlab.com/quip.network/dashboard.quip.network:v0.2",
                "quip.network/dashboard.quip.network",
                "Dashboard",
            ),
        ];

        for (image, local_ref, gitlab_path, display_name) in refs {
            assert_eq!(image.local_ref(), local_ref);
            assert_eq!(image.gitlab_path(), gitlab_path);
            assert_eq!(image.tag(), "v0.2");
            assert_eq!(image.display_name(), display_name);
        }
    }

    #[test]
    fn relevant_images_include_validator_only_for_docker_mode() {
        let mut settings = AppSettings {
            run_mode: RunMode::Docker,
            image_tag: ImageTag::Cuda,
            ..AppSettings::default()
        };

        let docker_images: Vec<String> = relevant_images(&settings)
            .into_iter()
            .map(|image| image.local_ref())
            .collect();
        assert_eq!(
            docker_images,
            vec![
                "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda:v0.2",
                "registry.gitlab.com/quip.network/quip-protocol-rs/quip-network-node:v0.2",
                "registry.gitlab.com/quip.network/dashboard.quip.network:v0.2",
            ]
        );

        settings.run_mode = RunMode::Native;
        let native_images: Vec<String> = relevant_images(&settings)
            .into_iter()
            .map(|image| image.local_ref())
            .collect();
        assert_eq!(
            native_images,
            vec!["registry.gitlab.com/quip.network/dashboard.quip.network:v0.2"]
        );
    }

    #[test]
    fn parse_semver_orders_release_candidates() {
        // Pre-releases order by their trailing number...
        assert!(parse_semver("0.2.0-rc1") < parse_semver("0.2.0-rc2"));
        // ...and a final release outranks any of its pre-releases.
        assert!(parse_semver("0.2.0-rc2") < parse_semver("0.2.0"));
        // The `v` prefix is ignored; normal precedence still holds.
        assert!(parse_semver("v0.2.0") > parse_semver("v0.1.5"));
        assert!(parse_semver("0.2.0-rc1") > parse_semver("0.1.9"));
        // Identical versions compare equal — no spurious update offered.
        assert_eq!(parse_semver("0.2.0-rc1"), parse_semver("v0.2.0-rc1"));
        // A higher patch outranks a lower final release even as a pre-release,
        // so the binary updater never downgrades 0.2.1-rc2 to 0.2.0.
        assert!(parse_semver("0.2.1-rc2") > parse_semver("0.2.0"));
    }
}
