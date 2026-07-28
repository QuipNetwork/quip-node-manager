// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-repository image-tag resolution against the GitLab container registry.
//!
//! Each stack image (miner, validator, dashboard) lives in its own GitLab
//! project and advances on its own cadence — the miner may be at `v0.2.1-rc49`
//! while the validator is at `v0.2.1-rc13`. So every image resolves its channel
//! tag **individually** from its own registry, not from one shared release feed.
//!
//! The registry is queried with the Docker Registry v2 API (`/tags/list`) using
//! an anonymous pull token from GitLab's JWT endpoint — the same read path
//! `docker pull` uses for a public image, so no login is required.

use crate::settings::UpdateChannel;
use crate::update::{parse_semver, tag_matches_channel};
use std::time::Duration;

/// Whether `tag` is a canonical release tag: `vMAJOR.MINOR.PATCH` optionally
/// followed by `-rcN`. This deliberately rejects the noise a real registry
/// carries — git-sha tags, arch-suffixed tags (`…-amd64`/`-arm64`), rolling
/// minor tags (`v0.2`), `latest`, `-preview`, and the legacy non-hyphenated
/// `v0.2.1rc3` form — so semver resolution only ever considers real releases.
pub fn is_canonical_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let core_ok = {
        let parts: Vec<&str> = core.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    if !core_ok {
        return false;
    }
    match pre {
        None => true,
        Some(p) => p
            .strip_prefix("rc")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
    }
}

/// Highest canonical tag on `channel` from a list of raw registry tag names.
/// Release keeps only stable (`vX.Y.Z`) tags; Beta additionally allows `-rc`.
pub fn pick_channel_tag(tags: &[String], channel: UpdateChannel) -> Option<String> {
    tags.iter()
        .filter(|t| is_canonical_release_tag(t) && tag_matches_channel(t, channel))
        .max_by_key(|t| parse_semver(t))
        .cloned()
}

/// The registry path a repository is addressed by in the v2 API — the image
/// reference with the `registry.gitlab.com/` host stripped (e.g.
/// `quip.network/quip-miner/quip-miner-cpu`).
pub fn repo_path(image: &str) -> &str {
    image
        .strip_prefix("registry.gitlab.com/")
        .unwrap_or(image)
}

/// Fetch every tag name for a public registry repository via the Docker
/// Registry v2 API, authenticating with an anonymous pull token. Returns the
/// raw tag list (unfiltered) or an error describing which step failed.
pub async fn fetch_registry_tags(
    client: &reqwest::Client,
    path: &str,
) -> Result<Vec<String>, String> {
    let token_url = format!(
        "https://gitlab.com/jwt/auth?service=container_registry&scope=repository:{path}:pull"
    );
    let token = client
        .get(&token_url)
        .send()
        .await
        .map_err(|e| format!("registry auth for {path} failed: {e}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("registry auth for {path} returned bad JSON: {e}"))?
        .get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("registry auth for {path} returned no token"))?;

    let list_url = format!("https://registry.gitlab.com/v2/{path}/tags/list?n=10000");
    let body = client
        .get(&list_url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("GET tags for {path} failed: {e}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("tags list for {path} returned bad JSON: {e}"))?;

    Ok(body
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// Resolve the channel tag for a single image, or `None` when the registry is
/// unreachable or carries no canonical tag on that channel. `image` is the full
/// image reference (with or without the `registry.gitlab.com/` host).
pub async fn resolve_image_channel_tag(image: &str, channel: UpdateChannel) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let tags = fetch_registry_tags(&client, repo_path(image)).await.ok()?;
    pick_channel_tag(&tags, channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_filter_accepts_real_releases_only() {
        for good in ["v0.2.0", "v0.2.1-rc49", "v1.10.3", "v0.0.1"] {
            assert!(is_canonical_release_tag(good), "should accept {good}");
        }
        for bad in [
            "v0.2",              // rolling minor
            "latest",            // moving
            "v0.2.1-rc10-amd64", // arch suffix
            "v0.2.1rc3",         // legacy non-hyphenated
            "v0.2.0-preview",    // preview
            "sha-0dc1e809",      // git sha
            "0.2.0",             // missing v
            "v0.2.1-rc",         // rc without number
        ] {
            assert!(!is_canonical_release_tag(bad), "should reject {bad}");
        }
    }

    #[test]
    fn picks_highest_per_channel_ignoring_noise() {
        // A realistic messy tag set from one repo, incl. a stable above the
        // v0.2.0 Release floor.
        let tags: Vec<String> = [
            "latest",
            "v0.2",
            "sha-deadbeef",
            "v0.2.0",
            "v0.2.1",
            "v0.2.1-rc7",
            "v0.2.1-rc13",
            "v0.2.1-rc13-amd64",
            "v0.2.1rc4",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        assert_eq!(
            pick_channel_tag(&tags, UpdateChannel::Beta).as_deref(),
            Some("v0.2.1"),
            "Beta takes the highest tag overall (stable v0.2.1 > its rc's)"
        );
        assert_eq!(
            pick_channel_tag(&tags, UpdateChannel::Release).as_deref(),
            Some("v0.2.1"),
            "Release takes the highest stable above the floor"
        );
    }

    #[test]
    fn release_excludes_v0_2_0_floor() {
        // Only v0.2.0 stable + rc's — Release is gated (floor), Beta resolves.
        let tags: Vec<String> = ["v0.2.0", "v0.2.1-rc13"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pick_channel_tag(&tags, UpdateChannel::Release), None);
        assert_eq!(
            pick_channel_tag(&tags, UpdateChannel::Beta).as_deref(),
            Some("v0.2.1-rc13")
        );
    }

    #[test]
    fn no_canonical_tag_yields_none() {
        let tags: Vec<String> = ["latest", "v0.2", "sha-abc123"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pick_channel_tag(&tags, UpdateChannel::Beta), None);
        assert_eq!(pick_channel_tag(&tags, UpdateChannel::Release), None);
    }

    #[test]
    fn repo_path_strips_registry_host() {
        assert_eq!(
            repo_path("registry.gitlab.com/quip.network/quip-miner/quip-miner-cpu"),
            "quip.network/quip-miner/quip-miner-cpu"
        );
        assert_eq!(repo_path("quip.network/x"), "quip.network/x");
    }
}
