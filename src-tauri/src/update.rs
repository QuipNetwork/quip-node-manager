// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::native::NativeProcessState;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStep {
    StopNative,
    StopStack,
    DownloadBinary,
    PullImages,
    StartStack,
    StartNative,
}

/// The ordered stop → apply → start plan for a user-initiated update restart.
/// Docker stops/starts only the compose stack; Native also stops/starts the
/// host miner and optionally refreshes its binary.
///
/// `binary_update_pending` gates the `DownloadBinary` step: pass `true` only
/// when `native::check_binary_update()` returned `Ok(Some(_))`. For Docker
/// the flag is irrelevant — the plan never includes `DownloadBinary`.
fn update_restart_steps(
    mode: &crate::settings::RunMode,
    binary_update_pending: bool,
) -> Vec<UpdateStep> {
    use UpdateStep::*;
    match mode {
        crate::settings::RunMode::Docker => vec![StopStack, PullImages, StartStack],
        crate::settings::RunMode::Native if binary_update_pending => {
            vec![
                StopNative,
                StopStack,
                DownloadBinary,
                PullImages,
                StartStack,
                StartNative,
            ]
        }
        crate::settings::RunMode::Native => {
            vec![StopNative, StopStack, PullImages, StartStack, StartNative]
        }
    }
}

async fn run_update_step(app: &tauri::AppHandle, step: UpdateStep) -> Result<(), String> {
    // Native steps fetch the managed process state from `app` (same idiom as
    // health.rs), so the command needs no State parameter.
    match step {
        UpdateStep::StopNative => {
            let state = app.state::<NativeProcessState>();
            crate::native::stop_native_node(app.clone(), state).await
        }
        UpdateStep::StopStack => crate::compose::stop_stack(app.clone()).await,
        UpdateStep::DownloadBinary => crate::native::download_native_binary(app.clone())
            .await
            .map(|_| ()),
        UpdateStep::PullImages => crate::compose::pull_compose_images(app.clone()).await,
        UpdateStep::StartStack => crate::compose::start_stack(app.clone()).await,
        UpdateStep::StartNative => {
            let state = app.state::<NativeProcessState>();
            crate::native::start_native_node(app.clone(), state)
                .await
                .map(|_| ())
        }
    }
}

/// User-initiated: stop → apply the pending update → start, mode-aware. Bails
/// on the first failing step (leaving the node stopped) rather than claiming a
/// false success — the caller keeps the update flagged and re-enables the button.
///
/// For Native mode the `DownloadBinary` step is included only when
/// `native::check_binary_update()` confirms a binary update is pending. A
/// check error (network hiccup, GitLab outage) is treated as "not pending"
/// so that a Docker-image-only update still completes successfully; the binary
/// check will be re-run on the next monitor poll.
#[tauri::command]
pub async fn restart_to_update(app: tauri::AppHandle) -> Result<(), String> {
    let mode = crate::settings::load_settings().run_mode;
    let binary_update_pending = matches!(crate::native::check_binary_update().await, Ok(Some(_)));
    for step in update_restart_steps(&mode, binary_update_pending) {
        run_update_step(&app, step).await?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
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

// ── Update channel resolution ──────────────────────────────────────────────

use crate::settings::UpdateChannel;

/// Whether `tag` belongs on `channel`. Beta accepts every tag; Release accepts
/// only stable tags (no `-rc` / prerelease suffix). Relies on `parse_semver`'s
/// prerelease slot, which is `u64::MAX` for a final release and a smaller number
/// for `-rcN` pre-releases.
/// Stable tags at or below this version are NOT offered on the Release channel:
/// `v0.2.0` predates the stable-release process, so Release stays gated (grayed
/// out, forced to Beta) until a strictly newer stable (`v0.2.1`+) ships. Beta is
/// unaffected. As a `parse_semver` tuple, a stable `v0.2.0` = `(0,2,0,u64::MAX)`.
pub const RELEASE_STABLE_FLOOR: (u64, u64, u64, u64) = (0, 2, 0, u64::MAX);

pub fn tag_matches_channel(tag: &str, channel: UpdateChannel) -> bool {
    match channel {
        UpdateChannel::Beta => true,
        // Stable (no `-rc`) AND strictly newer than the floor.
        UpdateChannel::Release => {
            let sv = parse_semver(tag);
            sv.3 == u64::MAX && sv > RELEASE_STABLE_FLOOR
        }
    }
}

/// Highest item on `channel` by semver, where `version_of` extracts each item's
/// version string. On Release, `-rc` prereleases (and anything at/below the
/// v0.2.0 floor) are filtered out; on Beta everything is eligible. Used to pick
/// the app's own self-update release so the updater tracks the same channel as
/// the stack — new rc's nag on Beta, only new stables nag on Release.
fn pick_release_for_channel<T>(
    items: Vec<T>,
    channel: UpdateChannel,
    version_of: impl Fn(&T) -> &str,
) -> Option<T> {
    items
        .into_iter()
        .filter(|it| tag_matches_channel(version_of(it), channel))
        .max_by_key(|it| parse_semver(version_of(it)))
}

/// Per-image channel resolution for the settings UI. Each stack image resolves
/// its own tag from its own registry (they advance independently), so the UI
/// can show what each will pin to and whether Release is runnable.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChannelInfo {
    /// Tag each image resolves to on the channel (`None` = registry unreachable
    /// or no canonical tag), keyed by display name (Miner/Validator/Dashboard).
    pub images: Vec<(String, Option<String>)>,
    /// Whether every image has a stable (non-rc) tag — Release is selectable
    /// only when true.
    pub stable_available: bool,
}

/// The (display name, image reference) list the UI + monitor resolve, honoring
/// the active miner flavour (cpu vs cuda).
fn stack_images(settings: &crate::settings::AppSettings) -> [(&'static str, &'static str); 3] {
    [
        ("Miner", crate::compose::image_for_tag(settings.image_tag)),
        ("Validator", crate::compose::VALIDATOR_IMAGE),
        ("Dashboard", crate::compose::DASHBOARD_IMAGE),
    ]
}

/// Fetch one repo's tags once and derive both its channel tag and whether it
/// has any stable tag — avoids a second registry round-trip for the gray-out.
async fn resolve_repo_channel(
    client: &reqwest::Client,
    image: &str,
    channel: UpdateChannel,
) -> (Option<String>, bool) {
    match crate::registry::fetch_registry_tags(client, crate::registry::repo_path(image)).await {
        Ok(tags) => (
            crate::registry::pick_channel_tag(&tags, channel),
            crate::registry::pick_channel_tag(&tags, UpdateChannel::Release).is_some(),
        ),
        Err(_) => (None, false),
    }
}

/// Resolve what a channel points at for every image right now, for the settings
/// UI. Best-effort: an unreachable registry yields `None` for that image (and
/// no stable), so the UI degrades gracefully instead of erroring.
#[tauri::command]
pub async fn resolve_channel_info(channel: UpdateChannel) -> ChannelInfo {
    let settings = crate::settings::load_settings();
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return ChannelInfo {
            images: Vec::new(),
            stable_available: false,
        };
    };

    let images = stack_images(&settings);
    let (a, b, c) = tokio::join!(
        resolve_repo_channel(&client, images[0].1, channel),
        resolve_repo_channel(&client, images[1].1, channel),
        resolve_repo_channel(&client, images[2].1, channel),
    );
    let results = [a, b, c];
    ChannelInfo {
        images: images
            .iter()
            .zip(&results)
            .map(|((name, _), (tag, _))| (name.to_string(), tag.clone()))
            .collect(),
        stable_available: results.iter().all(|(_, has_stable)| *has_stable),
    }
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
    let channel = crate::settings::load_settings().update_channel;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let url = "https://gitlab.com/api/v4/projects/quip.network%2Fquip-node-manager/releases";
    let resp = client
        .get(url)
        .header("User-Agent", "quip-node-manager")
        .send()
        .await
        .map_err(|e| format!("releases API: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("releases API HTTP {}", resp.status()));
    }
    let releases: Vec<GitLabRelease> = resp
        .json()
        .await
        .map_err(|e| format!("releases API body: {e}"))?;

    // Track the channel: Release only offers new stable app releases, Beta also
    // offers newer rc's. Picks the highest matching release, not just the newest
    // listed, so a stray older entry can't shadow the real latest.
    let Some(latest) =
        pick_release_for_channel(releases, channel, |r| r.name.trim_start_matches('v'))
    else {
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

// ── Shared update-check path (periodic monitor + manual button) ────────────

/// High-level classification of one full update sweep. Found updates win over
/// partial check failures so a user still sees what is available when only
/// one source is unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheckStatus {
    UpdatesAvailable,
    UpToDate,
    CheckFailed,
}

/// One stack image with a newer channel tag than the pin in `.env`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUpdateInfo {
    pub image: String,
    pub version: String,
}

/// Result of one app + images + (optional) native-binary update sweep.
/// Returned by the manual "Check for updates" command so the UI can show
/// "you are up to date" and surface registry/API failures distinctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckOutcome {
    pub status: UpdateCheckStatus,
    pub app_update: Option<UpdateInfo>,
    pub image_updates: Vec<ImageUpdateInfo>,
    pub binary_update: Option<UpdateInfo>,
    /// Per-source failure messages (registry / releases API unreachable).
    pub errors: Vec<String>,
    /// Single line suitable for the log drawer.
    pub message: String,
}

/// Pure classifier: any found update → `UpdatesAvailable`; else any check
/// error → `CheckFailed`; else `UpToDate`. Found updates take priority so a
/// partial outage does not hide a real update.
pub fn classify_update_check(found_updates: bool, check_errors: bool) -> UpdateCheckStatus {
    if found_updates {
        UpdateCheckStatus::UpdatesAvailable
    } else if check_errors {
        UpdateCheckStatus::CheckFailed
    } else {
        UpdateCheckStatus::UpToDate
    }
}

/// Build the user-facing summary line for a completed update sweep.
pub fn summarize_update_check(
    status: UpdateCheckStatus,
    app_update: &Option<UpdateInfo>,
    image_updates: &[ImageUpdateInfo],
    binary_update: &Option<UpdateInfo>,
    errors: &[String],
) -> String {
    match status {
        UpdateCheckStatus::UpToDate => "You are up to date.".to_string(),
        UpdateCheckStatus::CheckFailed => {
            if errors.is_empty() {
                "Update check failed.".to_string()
            } else {
                format!("Update check failed: {}", errors.join("; "))
            }
        }
        UpdateCheckStatus::UpdatesAvailable => {
            let mut parts = Vec::new();
            if let Some(info) = app_update {
                parts.push(format!("Node Manager v{}", info.version));
            }
            for img in image_updates {
                parts.push(format!("{} image {}", img.image, img.version));
            }
            if let Some(info) = binary_update {
                parts.push(format!("native miner v{}", info.version));
            }
            let mut msg = if parts.is_empty() {
                "Updates available.".to_string()
            } else {
                format!("Updates available: {}.", parts.join(", "))
            };
            if !errors.is_empty() {
                msg.push_str(&format!(" (partial check failures: {})", errors.join("; ")));
            }
            msg
        }
    }
}

/// Resolve whether `image` has a newer channel tag than `current`. Returns
/// `Ok(Some(target))` when an update exists, `Ok(None)` when current is
/// latest (or the channel has no tag), and `Err` when the registry is
/// unreachable so callers can distinguish failure from "up to date".
async fn resolve_image_update(
    image: &str,
    current: &str,
    channel: UpdateChannel,
) -> Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let tags = crate::registry::fetch_registry_tags(&client, crate::registry::repo_path(image))
        .await
        .map_err(|e| e.to_string())?;
    let Some(target) = crate::registry::pick_channel_tag(&tags, channel) else {
        return Ok(None);
    };
    if parse_semver(&target) > parse_semver(current) {
        Ok(Some(target))
    } else {
        Ok(None)
    }
}

/// Dedup ids for OS-notification suppression (image + binary only; app
/// updates are out of scope for the desktop notification set).
fn update_ids_from_outcome(outcome: &UpdateCheckOutcome) -> HashSet<String> {
    let mut ids = HashSet::new();
    for img in &outcome.image_updates {
        ids.insert(format!("image:{}:{}", img.image, img.version));
    }
    if let Some(info) = &outcome.binary_update {
        ids.insert(format!("binary:{}", info.version));
    }
    ids
}

/// Single implementation of the update sweep used by both the background
/// monitor and the manual "Check for updates" command. Emits the same
/// events the monitor has always emitted when updates are found.
///
/// Check failures (unreachable registry / releases API) land in
/// `outcome.errors` and never masquerade as "up to date".
pub async fn run_update_checks(
    app: &tauri::AppHandle,
    settings: &crate::settings::AppSettings,
) -> UpdateCheckOutcome {
    let mut errors: Vec<String> = Vec::new();
    let mut app_update: Option<UpdateInfo> = None;
    let mut image_updates: Vec<ImageUpdateInfo> = Vec::new();
    let mut binary_update: Option<UpdateInfo> = None;

    // App release (node-manager itself).
    match check_app_update().await {
        Ok(Some(info)) => {
            let _ = app.emit("app-update-available", &info);
            crate::set_tray_update(
                app,
                true,
                &format!("Quip Node Manager — v{} available", info.version),
            );
            app_update = Some(info);
        }
        Ok(None) => {}
        Err(e) => errors.push(format!("app release: {e}")),
    }

    // Compose images — each against its own registry + pinned `.env` tag.
    // Applies in both modes (dashboard + support images run under Native too).
    // No pin ⇒ stack never started; skip silently (not a check failure).
    let env_keys = ["QUIP_MINER_TAG", "QUIP_VALIDATOR_TAG", "QUIP_DASHBOARD_TAG"];
    for ((name, image), key) in stack_images(settings).iter().zip(env_keys) {
        let Some(current) = crate::compose::current_pinned_tag(key) else {
            continue;
        };
        match resolve_image_update(image, &current, settings.update_channel).await {
            Ok(Some(target)) => {
                let _ = app.emit(
                    "image-update-available",
                    serde_json::json!({ "image": name, "version": target }),
                );
                image_updates.push(ImageUpdateInfo {
                    image: name.to_string(),
                    version: target,
                });
            }
            Ok(None) => {}
            Err(e) => errors.push(format!("{name} image: {e}")),
        }
    }

    // Native binary lives on GitLab Releases, not the container registry.
    if settings.run_mode == crate::settings::RunMode::Native {
        match crate::native::check_binary_update().await {
            Ok(Some(info)) => {
                let _ = app.emit("binary-update-available", &info);
                binary_update = Some(info);
            }
            Ok(None) => {}
            Err(e) => errors.push(format!("native binary: {e}")),
        }
    }

    let found = app_update.is_some() || !image_updates.is_empty() || binary_update.is_some();
    let status = classify_update_check(found, !errors.is_empty());
    let message =
        summarize_update_check(status, &app_update, &image_updates, &binary_update, &errors);

    UpdateCheckOutcome {
        status,
        app_update,
        image_updates,
        binary_update,
        errors,
        message,
    }
}

/// True when `current` contains an update id not present in `last` — i.e. a
/// genuinely new update appeared since the last notification. Shrinking or
/// clearing the set never notifies (an already-notified or applied update must
/// not re-nag).
fn has_new_update(current: &HashSet<String>, last: &HashSet<String>) -> bool {
    current.difference(last).next().is_some()
}

fn notify_update_available(app: &tauri::AppHandle) {
    let _ = app
        .notification()
        .builder()
        .title("Quip node update available")
        .body("Restart to Update to apply the latest node update.")
        .show();
}

/// User-initiated full update sweep. Always reports what it found (including
/// "you are up to date" and check failures). Does not touch the background
/// monitor's `last_notified` dedup set — that lives only in the monitor task.
#[tauri::command]
pub async fn check_for_updates_now(app: tauri::AppHandle) -> UpdateCheckOutcome {
    let settings = crate::settings::load_settings();
    run_update_checks(&app, &settings).await
}

/// Background task: first check ~30s after launch, then every 30 minutes.
/// - Compose images (both modes) + native binary (Native mode)
/// - Always: node-manager app release
///
/// OS notifications are deduped via `last_notified` so the same update does
/// not re-nag every period. Manual checks never write this set.
pub async fn background_update_monitor(app: tauri::AppHandle) {
    // Stay clear of startup work; do not fire at t=0.
    tokio::time::sleep(Duration::from_secs(30)).await;

    let mut last_notified: HashSet<String> = HashSet::new();

    loop {
        let settings = crate::settings::load_settings();
        let outcome = run_update_checks(&app, &settings).await;
        let current = update_ids_from_outcome(&outcome);

        if has_new_update(&current, &last_notified) {
            notify_update_available(&app);
        }
        last_notified = current;

        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::RunMode;

    #[test]
    fn app_update_selection_tracks_channel() {
        let pick = |rels: Vec<&'static str>, ch| {
            pick_release_for_channel(rels, ch, |v| v.trim_start_matches('v'))
        };

        // Only rc's newer than the running build: Beta nags (newest rc),
        // Release stays quiet (no stable to offer).
        let rc_only = vec!["v0.2.1-rc11", "v0.2.1-rc12"];
        assert_eq!(
            pick(rc_only.clone(), UpdateChannel::Beta),
            Some("v0.2.1-rc12")
        );
        assert_eq!(pick(rc_only, UpdateChannel::Release), None);

        // A stable release above the floor exists: both channels see it, and
        // Release ignores the newer rc line.
        let mixed = vec!["v0.2.2-rc1", "v0.2.1", "v0.2.0"];
        assert_eq!(pick(mixed.clone(), UpdateChannel::Beta), Some("v0.2.2-rc1"));
        assert_eq!(pick(mixed, UpdateChannel::Release), Some("v0.2.1"));
    }

    #[test]
    fn tag_matches_channel_gates_rc_and_floor_on_release() {
        // Beta accepts anything.
        assert!(tag_matches_channel("v0.2.0-rc1", UpdateChannel::Beta));
        assert!(tag_matches_channel("v0.2.0", UpdateChannel::Beta));
        // Release rejects rc.
        assert!(!tag_matches_channel("v0.2.1-rc1", UpdateChannel::Release));
        // Release rejects stable at/below the v0.2.0 floor...
        assert!(!tag_matches_channel("v0.2.0", UpdateChannel::Release));
        assert!(!tag_matches_channel("v0.1.9", UpdateChannel::Release));
        // ...but accepts a stable strictly newer than the floor.
        assert!(tag_matches_channel("v0.2.1", UpdateChannel::Release));
        assert!(tag_matches_channel("v0.3.0", UpdateChannel::Release));
    }

    #[test]
    fn notifies_only_on_a_newly_appeared_update() {
        use std::collections::HashSet;
        let none: HashSet<String> = HashSet::new();
        let a: HashSet<String> = ["miner:sha_a".into()].into();
        let ab: HashSet<String> = ["miner:sha_a".into(), "binary:0.2.1".into()].into();

        assert!(has_new_update(&a, &none), "first detection notifies");
        assert!(!has_new_update(&a, &a), "same set: no re-nag");
        assert!(has_new_update(&ab, &a), "a newly added id notifies");
        assert!(!has_new_update(&a, &ab), "shrinking set does not notify");
        assert!(!has_new_update(&none, &a), "cleared set does not notify");
    }

    #[test]
    fn classify_update_check_priority() {
        // Found updates always win — including when some sources also failed.
        assert_eq!(
            classify_update_check(true, false),
            UpdateCheckStatus::UpdatesAvailable
        );
        assert_eq!(
            classify_update_check(true, true),
            UpdateCheckStatus::UpdatesAvailable
        );
        // No updates + any check error → failed, never "up to date".
        assert_eq!(
            classify_update_check(false, true),
            UpdateCheckStatus::CheckFailed
        );
        // Clean sweep, nothing newer.
        assert_eq!(
            classify_update_check(false, false),
            UpdateCheckStatus::UpToDate
        );
    }

    #[test]
    fn summarize_update_check_messages() {
        let none: Option<UpdateInfo> = None;
        let empty_imgs: Vec<ImageUpdateInfo> = vec![];
        let no_errs: Vec<String> = vec![];

        assert_eq!(
            summarize_update_check(
                UpdateCheckStatus::UpToDate,
                &none,
                &empty_imgs,
                &none,
                &no_errs
            ),
            "You are up to date."
        );

        let errs = vec!["app release: timeout".into()];
        let failed = summarize_update_check(
            UpdateCheckStatus::CheckFailed,
            &none,
            &empty_imgs,
            &none,
            &errs,
        );
        assert!(failed.contains("Update check failed"));
        assert!(failed.contains("app release: timeout"));

        let app = Some(UpdateInfo {
            version: "0.2.4".into(),
            url: "https://example.com".into(),
            notes: String::new(),
        });
        let images = vec![ImageUpdateInfo {
            image: "Miner".into(),
            version: "v0.3.0".into(),
        }];
        let msg = summarize_update_check(
            UpdateCheckStatus::UpdatesAvailable,
            &app,
            &images,
            &none,
            &no_errs,
        );
        assert!(msg.contains("Node Manager v0.2.4"));
        assert!(msg.contains("Miner image v0.3.0"));
        assert!(!msg.contains("partial check failures"));

        // Partial failure noted alongside found updates.
        let partial = summarize_update_check(
            UpdateCheckStatus::UpdatesAvailable,
            &app,
            &empty_imgs,
            &none,
            &errs,
        );
        assert!(partial.contains("Node Manager v0.2.4"));
        assert!(partial.contains("partial check failures"));
    }

    #[test]
    fn update_ids_from_outcome_covers_image_and_binary_only() {
        let outcome = UpdateCheckOutcome {
            status: UpdateCheckStatus::UpdatesAvailable,
            app_update: Some(UpdateInfo {
                version: "9.9.9".into(),
                url: "u".into(),
                notes: String::new(),
            }),
            image_updates: vec![ImageUpdateInfo {
                image: "Miner".into(),
                version: "v1".into(),
            }],
            binary_update: Some(UpdateInfo {
                version: "0.3.0".into(),
                url: "u".into(),
                notes: String::new(),
            }),
            errors: vec![],
            message: String::new(),
        };
        let ids = update_ids_from_outcome(&outcome);
        assert!(ids.contains("image:Miner:v1"));
        assert!(ids.contains("binary:0.3.0"));
        // App updates stay out of the OS-notification dedup set.
        assert!(!ids.iter().any(|id| id.contains("9.9.9")));
    }

    #[test]
    fn docker_restart_plan_is_stop_pull_start() {
        use UpdateStep::*;
        // Docker ignores the binary_update_pending flag entirely.
        assert_eq!(
            update_restart_steps(&RunMode::Docker, false),
            vec![StopStack, PullImages, StartStack]
        );
        assert_eq!(
            update_restart_steps(&RunMode::Docker, true),
            vec![StopStack, PullImages, StartStack]
        );
    }

    #[test]
    fn native_restart_plan_with_binary_update_includes_download() {
        use UpdateStep::*;
        assert_eq!(
            update_restart_steps(&RunMode::Native, true),
            vec![
                StopNative,
                StopStack,
                DownloadBinary,
                PullImages,
                StartStack,
                StartNative
            ]
        );
    }

    #[test]
    fn native_restart_plan_without_binary_update_skips_download() {
        use UpdateStep::*;
        assert_eq!(
            update_restart_steps(&RunMode::Native, false),
            vec![StopNative, StopStack, PullImages, StartStack, StartNative]
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
