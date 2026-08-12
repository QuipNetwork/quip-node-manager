// SPDX-License-Identifier: AGPL-3.0-or-later
/// Public IP of this host, used to fill in an unset `public_host`.
///
/// Delegates to the checklist's fetcher (check.quip.network first, ipify as a
/// fallback) rather than querying ipify directly. The `ip` checklist row shows
/// the user the same value, and a node that advertises an address the
/// reachability probes never tested is worse than no address at all.
#[tauri::command]
pub async fn detect_public_ip() -> Result<String, String> {
    crate::checklist::fetch_public_ip()
        .await
        .ok_or_else(|| "no IP detection service reachable".to_string())
}
