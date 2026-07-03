// SPDX-License-Identifier: AGPL-3.0-or-later
//! Node health monitor: infra + chain-liveness + participation.

use crate::validator_rpc::SystemHealth;
use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn health(peers: u64, syncing: bool) -> SystemHealth {
        SystemHealth { peers, is_syncing: syncing }
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
}
