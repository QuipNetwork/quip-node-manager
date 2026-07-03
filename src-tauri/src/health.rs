// SPDX-License-Identifier: AGPL-3.0-or-later
//! Node health monitor: infra + chain-liveness + participation.

use crate::settings::{RunMode, StackHealth};
use crate::validator_rpc::SystemHealth;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum DimensionState {
    Ok,
    Warn,
    Fail,
    Unknown,
}

#[derive(Serialize, Clone, Debug)]
pub struct DimensionStatus {
    pub state: DimensionState,
    pub detail: String,
}

fn status(state: DimensionState, detail: impl Into<String>) -> DimensionStatus {
    DimensionStatus { state, detail: detail.into() }
}

/// Chain is live when the substrate block advanced since the last poll and the
/// node has peers and is not syncing. The first poll has no baseline → Unknown.
pub fn check_chain(
    prev_block: Option<u64>,
    now_block: u64,
    health: &SystemHealth,
) -> DimensionStatus {
    let Some(prev) = prev_block else {
        return status(DimensionState::Unknown, format!("first sample at block {now_block}"));
    };
    if health.peers == 0 {
        return status(DimensionState::Fail, "no peers");
    }
    if now_block <= prev {
        return status(DimensionState::Fail, format!("block stalled at {now_block}"));
    }
    if health.is_syncing {
        return status(DimensionState::Warn, format!("syncing at block {now_block}"));
    }
    status(DimensionState::Ok, format!("block {now_block}, {} peers", health.peers))
}

/// Participating when our latest participation marker is within 1 of the chain's
/// current qblock (tolerating the fresh-head transition). Before the first
/// marker is seen we are warming up (Warn), not failing.
pub fn check_participation(
    qblock_count: u64,
    latest_participation: Option<u64>,
    warming_up: bool,
) -> DimensionStatus {
    match latest_participation {
        None if warming_up => status(DimensionState::Warn, "warming up: no marker yet"),
        None => status(DimensionState::Fail, "no participation marker on chain"),
        Some(p) if p + 1 >= qblock_count => {
            status(DimensionState::Ok, format!("participating in qblock {p}/{qblock_count}"))
        }
        Some(p) => status(
            DimensionState::Fail,
            format!("behind: marker qblock {p}, chain {qblock_count}"),
        ),
    }
}

/// Infrastructure is OK when the Docker stack rolls up Running AND the miner is
/// up (native process alive, or — in Docker mode — its container is part of the
/// already-Running stack, in which case `miner_up` is passed as true).
pub fn check_infra(stack: StackHealth, miner_up: bool) -> DimensionStatus {
    if !miner_up {
        return status(DimensionState::Fail, "miner not running");
    }
    match stack {
        StackHealth::Running => status(DimensionState::Ok, "all services up"),
        StackHealth::Degraded => status(DimensionState::Warn, "stack degraded"),
        StackHealth::Unhealthy => status(DimensionState::Fail, "stack unhealthy"),
        StackHealth::Stopped => status(DimensionState::Fail, "stack stopped"),
    }
}

/// Combined verdict from all three health dimensions.
#[derive(Serialize, Clone)]
pub struct HealthReport {
    pub overall: StackHealth,
    pub infra: DimensionStatus,
    pub chain: DimensionStatus,
    pub participation: DimensionStatus,
}

/// Worst-wins: any Fail → Unhealthy; else any Warn/Unknown → Degraded; else Running.
pub fn roll_up(
    infra: DimensionStatus,
    chain: DimensionStatus,
    participation: DimensionStatus,
) -> HealthReport {
    let states = [&infra.state, &chain.state, &participation.state];
    let overall = if states.iter().any(|s| **s == DimensionState::Fail) {
        StackHealth::Unhealthy
    } else if states.iter().any(|s| matches!(s, DimensionState::Warn | DimensionState::Unknown)) {
        StackHealth::Degraded
    } else {
        StackHealth::Running
    };
    HealthReport { overall, infra, chain, participation }
}

/// Suppress a single-poll Unhealthy blip as Degraded; only report Unhealthy
/// once it persists for two consecutive polls. Recovery resets the counter.
pub fn debounce(
    _prev_overall: &StackHealth,
    candidate: StackHealth,
    consecutive_fails: &mut u32,
) -> StackHealth {
    match candidate {
        StackHealth::Unhealthy => {
            *consecutive_fails += 1;
            if *consecutive_fails >= 2 {
                StackHealth::Unhealthy
            } else {
                StackHealth::Degraded
            }
        }
        other => {
            *consecutive_fails = 0;
            other
        }
    }
}

#[derive(Default)]
struct MonitorState {
    prev_block: Option<u64>,
    consecutive_fails: u32,
    prev_overall: Option<StackHealth>,
    first_poll_epoch: Option<u64>,
}

/// One measurement of all three dimensions, rolled up and debounced.
async fn sample(app: &AppHandle, st: &Mutex<MonitorState>) -> HealthReport {
    let settings = crate::settings::load_settings();
    let run_mode = settings.run_mode.clone();
    let cfg = &settings.node_config;

    // Dimension A: infra. (get_stack_status / load_settings / data_dir take no app arg.)
    let stack = crate::compose::get_stack_status()
        .await
        .map(|s| s.overall)
        .unwrap_or(StackHealth::Unhealthy);
    let miner_up = match run_mode {
        RunMode::Native => {
            // get_native_node_status is an async #[tauri::command] over managed
            // NativeProcessState; fetch that state and call it directly.
            let native_state = app.state::<crate::native::NativeProcessState>();
            crate::native::get_native_node_status(native_state)
                .await
                .map(|s| s.running)
                .unwrap_or(false)
        }
        RunMode::Docker => !matches!(stack, StackHealth::Stopped),
    };
    let infra = check_infra(stack, miner_up);

    // Dimensions B & C via validator RPC (native_miner_validator_url is pub(crate)).
    let validator_url = crate::native::native_miner_validator_url(cfg);
    let rpc = crate::validator_rpc::ValidatorRpc::new(&validator_url);
    let (chain, participation) = probe_chain_and_participation(&rpc, st).await;

    let candidate = roll_up(infra, chain, participation);
    let mut guard = st.lock().unwrap();
    let prev = guard.prev_overall.unwrap_or(StackHealth::Stopped);
    let debounced = debounce(&prev, candidate.overall, &mut guard.consecutive_fails);
    guard.prev_overall = Some(debounced);
    HealthReport { overall: debounced, ..candidate }
}

async fn probe_chain_and_participation(
    rpc: &crate::validator_rpc::ValidatorRpc,
    st: &Mutex<MonitorState>,
) -> (DimensionStatus, DimensionStatus) {
    let now_block = match rpc.current_block().await {
        Ok(b) => b,
        Err(e) => {
            return (
                status(DimensionState::Unknown, e),
                status(DimensionState::Unknown, "rpc unreachable"),
            );
        }
    };
    let health = match rpc.system_health().await {
        Ok(h) => h,
        Err(e) => {
            return (
                status(DimensionState::Unknown, e),
                status(DimensionState::Unknown, "rpc unreachable"),
            );
        }
    };
    let prev_block = { st.lock().unwrap().prev_block };
    let chain = check_chain(prev_block, now_block, &health);
    {
        st.lock().unwrap().prev_block = Some(now_block);
    }

    let participation = probe_participation(rpc, st).await;
    (chain, participation)
}

async fn probe_participation(
    rpc: &crate::validator_rpc::ValidatorRpc,
    st: &Mutex<MonitorState>,
) -> DimensionStatus {
    let keystore = crate::settings::data_dir().join("keystore.json");
    let account = match crate::validator_rpc::read_account_id(&keystore) {
        Ok(a) => a,
        Err(e) => return status(DimensionState::Unknown, e),
    };
    let qc_key = crate::validator_rpc::storage_value_key("QuantumPow", "QBlockCount");
    let lp_key =
        crate::validator_rpc::storage_map_key("MinerRegistry", "LatestParticipation", &account);
    let qblock_count = match rpc.storage_u64(&qc_key).await {
        Ok(Some(v)) => v,
        Ok(None) => return status(DimensionState::Unknown, "no qblock count on chain"),
        Err(e) => return status(DimensionState::Unknown, e),
    };
    let latest = rpc.storage_u64(&lp_key).await.ok().flatten();
    // Warming up = still within the startup grace window (~2 min = one head interval).
    let warming_up = within_startup_grace(st);
    check_participation(qblock_count, latest, warming_up)
}

/// True until STARTUP_GRACE_SECS have elapsed since the first poll.
fn within_startup_grace(st: &Mutex<MonitorState>) -> bool {
    const STARTUP_GRACE_SECS: u64 = 120;
    let mut guard = st.lock().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let start = *guard.first_poll_epoch.get_or_insert(now);
    now.saturating_sub(start) < STARTUP_GRACE_SECS
}

#[tauri::command]
pub async fn get_health(app: AppHandle) -> HealthReport {
    let st = app.state::<Mutex<MonitorState>>();
    let inner = st.inner();
    sample(&app, inner).await
}

/// Spawn the 15 s poll loop; emit `health-changed` and notify on transitions.
pub fn spawn_health_monitor(app: AppHandle) {
    app.manage(Mutex::new(MonitorState::default()));
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
        let mut last_overall: Option<StackHealth> = None;
        loop {
            ticker.tick().await;
            let report = {
                let st = app.state::<Mutex<MonitorState>>();
                let inner = st.inner();
                sample(&app, inner).await
            };
            let flipped_to_unhealthy = matches!(report.overall, StackHealth::Unhealthy)
                && !matches!(last_overall, Some(StackHealth::Unhealthy));
            if flipped_to_unhealthy {
                notify_unhealthy(&app, &report);
            }
            last_overall = Some(report.overall);
            let _ = app.emit("health-changed", &report);
        }
    });
}

fn notify_unhealthy(app: &AppHandle, report: &HealthReport) {
    let reason = [&report.infra, &report.chain, &report.participation]
        .iter()
        .find(|d| d.state == DimensionState::Fail)
        .map(|d| d.detail.clone())
        .unwrap_or_else(|| "node unhealthy".to_string());
    let _ = app
        .notification()
        .builder()
        .title("Quip node unhealthy")
        .body(&reason)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(peers: u64, syncing: bool) -> SystemHealth {
        SystemHealth { peers, is_syncing: syncing }
    }

    use crate::settings::StackHealth;

    fn ok() -> DimensionStatus { status(DimensionState::Ok, "") }
    fn fail() -> DimensionStatus { status(DimensionState::Fail, "") }
    fn warn() -> DimensionStatus { status(DimensionState::Warn, "") }

    #[test]
    fn rollup_all_ok_is_running() {
        assert!(matches!(roll_up(ok(), ok(), ok()).overall, StackHealth::Running));
    }

    #[test]
    fn rollup_infra_fail_is_unhealthy() {
        assert!(matches!(roll_up(fail(), ok(), ok()).overall, StackHealth::Unhealthy));
    }

    #[test]
    fn rollup_chain_fail_is_unhealthy() {
        assert!(matches!(roll_up(ok(), fail(), ok()).overall, StackHealth::Unhealthy));
    }

    #[test]
    fn rollup_warn_is_degraded_not_unhealthy() {
        assert!(matches!(roll_up(ok(), warn(), ok()).overall, StackHealth::Degraded));
    }

    #[test]
    fn debounce_holds_first_fail_at_degraded() {
        let mut n = 0;
        let out = debounce(&StackHealth::Running, StackHealth::Unhealthy, &mut n);
        assert!(matches!(out, StackHealth::Degraded));
        assert_eq!(n, 1);
    }

    #[test]
    fn debounce_reports_unhealthy_on_second_consecutive_fail() {
        let mut n = 1;
        let out = debounce(&StackHealth::Degraded, StackHealth::Unhealthy, &mut n);
        assert!(matches!(out, StackHealth::Unhealthy));
        assert_eq!(n, 2);
    }

    #[test]
    fn debounce_resets_on_recovery() {
        let mut n = 2;
        let out = debounce(&StackHealth::Unhealthy, StackHealth::Running, &mut n);
        assert!(matches!(out, StackHealth::Running));
        assert_eq!(n, 0);
    }

    #[test]
    fn chain_ok_when_block_advances_and_synced() {
        let s = check_chain(Some(100), 103, &health(8, false));
        assert_eq!(s.state, DimensionState::Ok);
    }

    #[test]
    fn chain_fails_when_block_stalls() {
        let s = check_chain(Some(100), 100, &health(8, false));
        assert_eq!(s.state, DimensionState::Fail);
    }

    #[test]
    fn chain_warns_while_syncing() {
        let s = check_chain(Some(100), 103, &health(8, true));
        assert_eq!(s.state, DimensionState::Warn);
    }

    #[test]
    fn chain_fails_with_no_peers() {
        let s = check_chain(Some(100), 103, &health(0, false));
        assert_eq!(s.state, DimensionState::Fail);
    }

    #[test]
    fn chain_unknown_on_first_sample() {
        let s = check_chain(None, 100, &health(8, false));
        assert_eq!(s.state, DimensionState::Unknown);
    }

    #[test]
    fn participation_ok_when_current() {
        assert_eq!(check_participation(3865, Some(3866), false).state, DimensionState::Ok);
        assert_eq!(check_participation(3865, Some(3865), false).state, DimensionState::Ok);
        assert_eq!(check_participation(3865, Some(3864), false).state, DimensionState::Ok);
    }

    #[test]
    fn participation_fails_when_two_behind() {
        assert_eq!(check_participation(3865, Some(3863), false).state, DimensionState::Fail);
    }

    #[test]
    fn participation_warns_while_warming_up() {
        assert_eq!(check_participation(3865, None, true).state, DimensionState::Warn);
    }

    #[test]
    fn participation_fails_when_never_participated_after_warmup() {
        assert_eq!(check_participation(3865, None, false).state, DimensionState::Fail);
    }

    #[test]
    fn infra_ok_when_stack_running_and_miner_up() {
        assert_eq!(check_infra(StackHealth::Running, true).state, DimensionState::Ok);
    }

    #[test]
    fn infra_fails_when_stack_unhealthy() {
        assert_eq!(check_infra(StackHealth::Unhealthy, true).state, DimensionState::Fail);
    }

    #[test]
    fn infra_fails_when_miner_down() {
        assert_eq!(check_infra(StackHealth::Running, false).state, DimensionState::Fail);
    }
}
