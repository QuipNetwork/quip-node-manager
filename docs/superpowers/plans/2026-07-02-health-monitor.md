# Health Monitor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a continuous health monitor that reports the node healthy only when the expected Docker services (and, in Native mode, the host miner) are up, the substrate chain is advancing, and our miner has a participation marker against the current qblockid — surfaced in the UI and via a desktop/tray notification on the flip to unhealthy.

**Architecture:** A host-side `HealthMonitor` polls three independent dimensions every 15 s. Infrastructure reuses the existing `compose::get_stack_status` roll-up + native PID check. Chain-liveness and participation come from **read-only substrate JSON-RPC** over the existing `reqwest` client: `chain_getHeader` / `system_health`, plus two pallet storage reads (`QuantumPow.QBlockCount` and `MinerRegistry.LatestParticipation[account]`), decoded by hand (both are plain `u64` little-endian). The log stream stays as the diagnostic layer only.

**Tech Stack:** Rust (Tauri v2 backend), `reqwest` (already present), new crates `twox-hash` + `blake2` for storage-key hashing (the two u64 values decode with stdlib `from_le_bytes`); vanilla JS/HTML frontend.

## Global Constraints

- License header on every new file: `// SPDX-License-Identifier: AGPL-3.0-or-later` (first line, matching existing `src-tauri/src/*.rs`).
- ≤100 lines/function, cyclomatic complexity ≤8, ≤5 positional params, 100-char lines, absolute imports (`crate::…`), no relative module paths.
- Zero-warnings: `cargo clippy` must pass clean. Run from `src-tauri/`.
- No new `:latest` image tags, no config schema changes to `config.toml`.
- Dependency versions (looked up 2026-07-02): `twox-hash = "2.1"`, `blake2 = "0.10"`. (No `parity-scale-codec`: both storage values are plain `u64` LE, decoded with stdlib `from_le_bytes`.)
- Health verdict enum reuses the existing `crate::settings::StackHealth` variants: `Running`, `Degraded`, `Unhealthy`, `Stopped`.
- Validator RPC is reached from the host at `http://127.0.0.1:<validator_rpc_port>` (default 9944) in **both** run modes (Task 8 makes Docker publish it).

### Resolved on-chain facts (verified against the live testnet node, 2026-07-02)

Known-answer test vectors and fixtures used throughout the plan:

| Item | Value |
|------|-------|
| `twox128("System")` (KAT) | `26aa394eea5630e07c48ae0c9558cef7` |
| `twox128("Account")` (KAT) | `b99d880ec681799c0cf30e8886371da9` |
| `twox128("QuantumPow")` | `9b2c4dbe49d7a1aed7ce99e4b8c072e8` |
| `twox128("QBlockCount")` | `a917cf9ea4fd296f6933d75be837c3e1` |
| **`QuantumPow.QBlockCount` full key** | `0x9b2c4dbe49d7a1aed7ce99e4b8c072e8a917cf9ea4fd296f6933d75be837c3e1` |
| `twox128("MinerRegistry")` | `491850926eb92ce9861fdcc5504d045e` |
| `twox128("LatestParticipation")` | `3b989607de2d2e007e711506edd2b037` |
| account id (test fixture) | `b4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a` |
| `blake2_128(account)` | `c3d972b93e95d78499f6ca768214647b` |
| **`MinerRegistry.LatestParticipation[account]` full key** | `0x491850926eb92ce9861fdcc5504d045e3b989607de2d2e007e711506edd2b037c3d972b93e95d78499f6ca768214647bb4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a` |
| `QBlockCount` raw value → decoded | `0x190f000000000000` → `3865` (u64 LE) |
| `LatestParticipation` raw value → decoded | `0x1a0f00000000000001007e1c0800` → first u64 = `3866` |

- Storage-map hasher for both maps is `blake2_128_concat` (`blake2_128(key) ++ key`).
- The 32-byte account id is read directly from `keystore.json` field `account_id_hex` (no SS58/base58 decode needed).
- Chain liveness = substrate block number (`chain_getHeader.number`) advancing (~6.7 s/block observed); `QBlockCount` advances only every few minutes and must NOT be used for liveness.
- Participation healthy rule: `latest_participation_qblock >= qblock_count - 1` (within-1 tolerates the fresh-head transition).

## File Structure

- **Create `src-tauri/src/validator_rpc.rs`** — read-only substrate JSON-RPC client + storage-key hashing + `u64` decode. One responsibility: talk to the validator.
- **Create `src-tauri/src/health.rs`** — dimension types, pure dimension checks, roll-up + debounce, the poll loop, the `get_health` command, notify. One responsibility: decide and publish health.
- **Modify `src-tauri/Cargo.toml`** — add the three crates.
- **Modify `src-tauri/src/stack_assets.rs:155-169`** — publish validator RPC to host loopback in both modes.
- **Modify `src-tauri/src/lib.rs`** — declare the two modules, register `get_health`, spawn the monitor.
- **Modify `src/index.html`** — health panel markup.
- **Modify `src/app.js`** — consume `health-changed` event + `get_health`, fold into the status pill, render the panel.

---

### Task 1: Storage-key hashing (`validator_rpc.rs`)

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/validator_rpc.rs`

**Interfaces:**
- Produces: `twox_128(data: &[u8]) -> [u8; 16]`, `storage_value_key(pallet: &str, item: &str) -> Vec<u8>`, `storage_map_key(pallet: &str, item: &str, key: &[u8]) -> Vec<u8>` (uses `blake2_128_concat`).

- [ ] **Step 1: Add dependencies**

In `src-tauri/Cargo.toml` under `[dependencies]`, after `hex = "0.4"`:

```toml
# Read-only substrate storage-key hashing for the health monitor
# (twox128 / blake2_128_concat). The two u64 values decode with stdlib
# from_le_bytes, so no SCALE-codec crate is needed. No signing.
twox-hash = "2.1"
blake2 = "0.10"
```

- [ ] **Step 2: Write the failing test**

Create `src-tauri/src/validator_rpc.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only substrate JSON-RPC client for the health monitor.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use twox_hash::XxHash64;

/// substrate `twox_128`: two xxhash64 (seeds 0 and 1), each 8 bytes LE.
fn twox_128(data: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&XxHash64::oneshot(0, data).to_le_bytes());
    out[8..].copy_from_slice(&XxHash64::oneshot(1, data).to_le_bytes());
    out
}

/// substrate `blake2_128` (16-byte Blake2b).
fn blake2_128(data: &[u8]) -> [u8; 16] {
    let mut h = Blake2bVar::new(16).expect("16 is a valid blake2b length");
    h.update(data);
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).expect("output buffer is 16 bytes");
    out
}

/// Key for a plain `StorageValue`: twox128(pallet) ++ twox128(item).
pub fn storage_value_key(pallet: &str, item: &str) -> Vec<u8> {
    let mut k = twox_128(pallet.as_bytes()).to_vec();
    k.extend_from_slice(&twox_128(item.as_bytes()));
    k
}

/// Key for a `blake2_128_concat` map entry: value-prefix ++ blake2_128(key) ++ key.
pub fn storage_map_key(pallet: &str, item: &str, key: &[u8]) -> Vec<u8> {
    let mut k = storage_value_key(pallet, item);
    k.extend_from_slice(&blake2_128(key));
    k.extend_from_slice(key);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twox_128_matches_known_substrate_vectors() {
        assert_eq!(hex::encode(twox_128(b"System")), "26aa394eea5630e07c48ae0c9558cef7");
        assert_eq!(hex::encode(twox_128(b"Account")), "b99d880ec681799c0cf30e8886371da9");
    }
}
```

- [ ] **Step 3: Register the module and run the failing test**

Add to `src-tauri/src/lib.rs` with the other `mod` declarations (near the top module list):

```rust
mod validator_rpc;
```

Run: `cd src-tauri && cargo test validator_rpc::tests::twox_128_matches_known_substrate_vectors`
Expected: compiles; test PASSES (this verifies the crate API is used correctly). If it FAILS on the vectors, the `XxHash64::oneshot` call is wrong for the installed `twox-hash` — adjust to the version's oneshot/seeded API until the known vectors match. Do not change the expected hex.

- [ ] **Step 4: Add the storage-key tests**

Append to the `tests` module in `validator_rpc.rs`:

```rust
    #[test]
    fn qblock_count_storage_key_is_correct() {
        assert_eq!(
            hex::encode(storage_value_key("QuantumPow", "QBlockCount")),
            "9b2c4dbe49d7a1aed7ce99e4b8c072e8a917cf9ea4fd296f6933d75be837c3e1"
        );
    }

    #[test]
    fn latest_participation_map_key_is_correct() {
        let account =
            hex::decode("b4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a").unwrap();
        assert_eq!(
            hex::encode(storage_map_key("MinerRegistry", "LatestParticipation", &account)),
            "491850926eb92ce9861fdcc5504d045e3b989607de2d2e007e711506edd2b037\
c3d972b93e95d78499f6ca768214647b\
b4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a"
        );
    }
```

Run: `cd src-tauri && cargo test validator_rpc::tests`
Expected: all three PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/validator_rpc.rs src-tauri/src/lib.rs
git commit -m "feat(health): substrate storage-key hashing (twox128/blake2_128_concat)"
```

---

### Task 2: Decode the two storage values (`validator_rpc.rs`)

**Files:**
- Modify: `src-tauri/src/validator_rpc.rs`

**Interfaces:**
- Produces: `decode_u64_le(bytes: &[u8]) -> Option<u64>` — decodes the leading SCALE `u64` (little-endian) from a storage value; `None` if fewer than 8 bytes.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `validator_rpc.rs`:

```rust
    #[test]
    fn decodes_qblock_count_value() {
        let raw = hex::decode("190f000000000000").unwrap();
        assert_eq!(decode_u64_le(&raw), Some(3865));
    }

    #[test]
    fn decodes_leading_u64_of_latest_participation() {
        // Value carries qblock:u64 followed by trailing fields we ignore.
        let raw = hex::decode("1a0f00000000000001007e1c0800").unwrap();
        assert_eq!(decode_u64_le(&raw), Some(3866));
    }

    #[test]
    fn decode_u64_le_rejects_short_input() {
        assert_eq!(decode_u64_le(&[0u8; 4]), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test validator_rpc::tests::decodes_qblock_count_value`
Expected: FAIL — `cannot find function decode_u64_le`.

- [ ] **Step 3: Write minimal implementation**

Add to `validator_rpc.rs` (above the `tests` module). SCALE-encoded `u64` is exactly 8-byte little-endian, so we read the leading 8 bytes:

```rust
/// Decode the leading SCALE `u64` (little-endian) from a storage value.
pub fn decode_u64_le(bytes: &[u8]) -> Option<u64> {
    let head: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
    Some(u64::from_le_bytes(head))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test validator_rpc::tests`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/validator_rpc.rs
git commit -m "feat(health): decode leading u64 from substrate storage values"
```

---

### Task 3: JSON-RPC transport (`validator_rpc.rs`)

**Files:**
- Modify: `src-tauri/src/validator_rpc.rs`

**Interfaces:**
- Consumes: `crate::native::validator_rpc_http_probe_url` pattern (ws→http). Reuse by making it `pub(crate)` if not already, OR replicate the conversion in `rpc_base_url` below.
- Produces:
  - `pub struct ValidatorRpc { base: String }` with `ValidatorRpc::new(ws_or_http_url: &str) -> Self`.
  - `async fn current_block(&self) -> Result<u64, String>` (from `chain_getHeader.number`, hex).
  - `async fn system_health(&self) -> Result<SystemHealth, String>` where `pub struct SystemHealth { pub peers: u64, pub is_syncing: bool }`.
  - `async fn storage_u64(&self, key: &[u8]) -> Result<Option<u64>, String>` (calls `state_getStorage`, hex-decodes, `decode_u64_le`).
- Pure helpers (unit-testable without a node): `parse_block_number(hex: &str) -> Result<u64,String>`, `parse_system_health(json: &serde_json::Value) -> Result<SystemHealth,String>`.

- [ ] **Step 1: Write the failing test for the pure parsers**

Append to the `tests` module:

```rust
    #[test]
    fn parses_hex_block_number() {
        assert_eq!(parse_block_number("0x81b5b").unwrap(), 531291);
    }

    #[test]
    fn parses_system_health() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"peers":8,"isSyncing":false,"shouldHavePeers":true}"#).unwrap();
        let h = parse_system_health(&v).unwrap();
        assert_eq!(h.peers, 8);
        assert!(!h.is_syncing);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test validator_rpc::tests::parses_hex_block_number`
Expected: FAIL — items not found.

- [ ] **Step 3: Write the implementation**

Add to `validator_rpc.rs`:

```rust
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SystemHealth {
    pub peers: u64,
    #[serde(rename = "isSyncing")]
    pub is_syncing: bool,
}

pub fn parse_block_number(hex_str: &str) -> Result<u64, String> {
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    u64::from_str_radix(s, 16).map_err(|e| format!("bad block number {hex_str}: {e}"))
}

pub fn parse_system_health(v: &serde_json::Value) -> Result<SystemHealth, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("bad system_health: {e}"))
}

/// Convert a `ws(s)://host:port[/path]` validator URL to an `http(s)://` JSON-RPC base.
fn rpc_base_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

pub struct ValidatorRpc {
    base: String,
}

impl ValidatorRpc {
    pub fn new(url: &str) -> Self {
        ValidatorRpc { base: rpc_base_url(url) }
    }

    async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({"id": 1, "jsonrpc": "2.0", "method": method, "params": params});
        let resp = reqwest::Client::new()
            .post(&self.base)
            .json(&body)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("{method} request failed: {e}"))?;
        let v: serde_json::Value = resp.json().await.map_err(|e| format!("{method} bad json: {e}"))?;
        v.get("result").cloned().ok_or_else(|| format!("{method}: no result field"))
    }

    pub async fn current_block(&self) -> Result<u64, String> {
        let r = self.call("chain_getHeader", serde_json::json!([])).await?;
        let num = r.get("number").and_then(|n| n.as_str()).ok_or("header has no number")?;
        parse_block_number(num)
    }

    pub async fn system_health(&self) -> Result<SystemHealth, String> {
        let r = self.call("system_health", serde_json::json!([])).await?;
        parse_system_health(&r)
    }

    pub async fn storage_u64(&self, key: &[u8]) -> Result<Option<u64>, String> {
        let hex_key = format!("0x{}", hex::encode(key));
        let r = self.call("state_getStorage", serde_json::json!([hex_key])).await?;
        match r.as_str() {
            None => Ok(None), // null result = storage item absent
            Some(s) => {
                let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
                    .map_err(|e| format!("bad storage hex: {e}"))?;
                Ok(decode_u64_le(&bytes))
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test validator_rpc::tests && cargo clippy`
Expected: tests PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/validator_rpc.rs
git commit -m "feat(health): read-only validator JSON-RPC client"
```

---

### Task 4: Account id from keystore (`validator_rpc.rs`)

**Files:**
- Modify: `src-tauri/src/validator_rpc.rs`

**Interfaces:**
- Produces: `parse_account_id_hex(keystore_json: &str) -> Result<[u8; 32], String>` — reads the `account_id_hex` field.
- Produces: `pub fn read_account_id(keystore_path: &std::path::Path) -> Result<[u8; 32], String>` — reads the file then parses.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module:

```rust
    #[test]
    fn parses_account_id_from_keystore_json() {
        let json = r#"{"version":1,"scheme":"hybrid","encrypted":false,
            "account_id_hex":"0xb4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a"}"#;
        let id = parse_account_id_hex(json).unwrap();
        assert_eq!(
            hex::encode(id),
            "b4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a"
        );
    }

    #[test]
    fn account_id_missing_field_errs() {
        assert!(parse_account_id_hex(r#"{"version":1}"#).is_err());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test validator_rpc::tests::parses_account_id_from_keystore_json`
Expected: FAIL — function not found.

- [ ] **Step 3: Write the implementation**

Add to `validator_rpc.rs`:

```rust
pub fn parse_account_id_hex(keystore_json: &str) -> Result<[u8; 32], String> {
    let v: serde_json::Value =
        serde_json::from_str(keystore_json).map_err(|e| format!("keystore not json: {e}"))?;
    let s = v.get("account_id_hex").and_then(|x| x.as_str())
        .ok_or("keystore missing account_id_hex")?;
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| format!("bad account_id_hex: {e}"))?;
    bytes.try_into().map_err(|_| "account_id_hex is not 32 bytes".to_string())
}

pub fn read_account_id(keystore_path: &std::path::Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(keystore_path)
        .map_err(|e| format!("cannot read keystore {}: {e}", keystore_path.display()))?;
    parse_account_id_hex(&text)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test validator_rpc::tests`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/validator_rpc.rs
git commit -m "feat(health): read miner account id from keystore.json"
```

---

### Task 5: Dimension types + pure chain/participation checks (`health.rs`)

**Files:**
- Create: `src-tauri/src/health.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod health;`)

**Interfaces:**
- Consumes: `crate::settings::StackHealth`.
- Produces:
  - `#[derive(Serialize, Clone, PartialEq)] pub enum DimensionState { Ok, Warn, Fail, Unknown }`
  - `#[derive(Serialize, Clone)] pub struct DimensionStatus { pub state: DimensionState, pub detail: String }`
  - `pub fn check_chain(prev_block: Option<u64>, now_block: u64, health: &SystemHealth) -> DimensionStatus`
  - `pub fn check_participation(qblock_count: u64, latest_participation: Option<u64>, warming_up: bool) -> DimensionStatus`

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/health.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Add `mod health;` to `src-tauri/src/lib.rs`, then run:
`cd src-tauri && cargo test health::tests::chain_ok_when_block_advances_and_synced`
Expected: FAIL — `check_chain` not found.

- [ ] **Step 3: Write the implementation**

Add to `health.rs` (above `tests`):

```rust
fn status(state: DimensionState, detail: impl Into<String>) -> DimensionStatus {
    DimensionStatus { state, detail: detail.into() }
}

/// Chain is live when the substrate block advanced since the last poll and the
/// node has peers and is not syncing. The first poll has no baseline → Unknown.
pub fn check_chain(prev_block: Option<u64>, now_block: u64, health: &SystemHealth) -> DimensionStatus {
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
```

Note: `p + 1 >= qblock_count` expresses `p >= qblock_count - 1` without underflow when `qblock_count == 0`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test health::tests && cargo clippy`
Expected: PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/health.rs src-tauri/src/lib.rs
git commit -m "feat(health): dimension types + pure chain/participation checks"
```

---

### Task 6: Roll-up + debounce (`health.rs`)

**Files:**
- Modify: `src-tauri/src/health.rs`

**Interfaces:**
- Consumes: `crate::settings::StackHealth`, `DimensionStatus`, `DimensionState`.
- Produces:
  - `#[derive(Serialize, Clone)] pub struct HealthReport { pub overall: StackHealth, pub infra: DimensionStatus, pub chain: DimensionStatus, pub participation: DimensionStatus }`
  - `pub fn roll_up(infra: DimensionStatus, chain: DimensionStatus, participation: DimensionStatus) -> HealthReport` — worst-wins mapping to `StackHealth`.
  - `pub fn debounce(prev_overall: &StackHealth, candidate: StackHealth, consecutive_fails: &mut u32) -> StackHealth` — requires 2 consecutive non-Running verdicts before reporting Unhealthy.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `health.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test health::tests::rollup_all_ok_is_running`
Expected: FAIL — `roll_up` not found.

- [ ] **Step 3: Write the implementation**

Add to `health.rs`:

```rust
use crate::settings::StackHealth;

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

/// Suppress a single-poll Unhealthy blip as Degraded; only report Unhealthy once
/// it persists for two consecutive polls. Recovery to Running resets the counter.
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test health::tests && cargo clippy`
Expected: PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/health.rs
git commit -m "feat(health): worst-wins roll-up + unhealthy debounce"
```

---

### Task 7: Monitor loop, `get_health` command, notify (`health.rs` + `lib.rs`)

**Files:**
- Modify: `src-tauri/src/health.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `crate::compose::get_stack_status` (returns `StackStatus { overall: StackHealth, .. }`), `crate::native::get_native_node_status` (`NativeNodeStatus { running, .. }`), `crate::native::native_miner_validator_url`, `crate::settings::{load_settings, data_dir, RunMode}`, `ValidatorRpc`, `read_account_id`, `storage_value_key`, `storage_map_key`.
- Produces: `#[tauri::command] pub async fn get_health(app) -> HealthReport`; `pub fn spawn_health_monitor(app: tauri::AppHandle)`; emits `health-changed` event with `HealthReport` payload; fires a notification on the Running→Unhealthy transition.

- [ ] **Step 1: Write `check_infra` + its test**

Append to the `tests` module (pure part first):

```rust
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
```

Run: `cd src-tauri && cargo test health::tests::infra_ok_when_stack_running_and_miner_up`
Expected: FAIL — `check_infra` not found.

- [ ] **Step 2: Implement `check_infra`**

Add to `health.rs`:

```rust
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
```

Run: `cd src-tauri && cargo test health::tests` → PASS.

- [ ] **Step 3: Implement the monitor loop + command (integration glue)**

Add to `health.rs`. This orchestrates the tested pure functions; it is glue, verified end-to-end by the manual smoke test in Task 9.

```rust
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

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
    let stack = crate::compose::get_stack_status().await
        .map(|s| s.overall)
        .unwrap_or(StackHealth::Unhealthy);
    let miner_up = match run_mode {
        RunMode::Native => {
            // get_native_node_status is an async #[tauri::command] over managed
            // NativeProcessState; fetch that state and call it directly.
            let native_state = app.state::<crate::native::NativeProcessState>();
            crate::native::get_native_node_status(native_state).await
                .map(|s| s.running).unwrap_or(false)
        }
        RunMode::Docker => !matches!(stack, StackHealth::Stopped),
    };
    let infra = check_infra(stack, miner_up);

    // Dimensions B & C via validator RPC (native_miner_validator_url is pub(crate)).
    let rpc = crate::validator_rpc::ValidatorRpc::new(&crate::native::native_miner_validator_url(cfg));
    let (chain, participation) = probe_chain_and_participation(&rpc, st).await;

    let candidate = roll_up(infra, chain, participation);
    let mut guard = st.lock().unwrap();
    let prev = guard.prev_overall.clone().unwrap_or(StackHealth::Stopped);
    let debounced = debounce(&prev, candidate.overall.clone(), &mut guard.consecutive_fails);
    guard.prev_overall = Some(debounced.clone());
    HealthReport { overall: debounced, ..candidate }
}

async fn probe_chain_and_participation(
    rpc: &crate::validator_rpc::ValidatorRpc,
    st: &Mutex<MonitorState>,
) -> (DimensionStatus, DimensionStatus) {
    let now_block = match rpc.current_block().await {
        Ok(b) => b,
        Err(e) => {
            return (status(DimensionState::Unknown, e.clone()),
                    status(DimensionState::Unknown, "rpc unreachable"));
        }
    };
    let health = match rpc.system_health().await {
        Ok(h) => h,
        Err(e) => return (status(DimensionState::Unknown, e),
                          status(DimensionState::Unknown, "rpc unreachable")),
    };
    let prev_block = { st.lock().unwrap().prev_block };
    let chain = check_chain(prev_block, now_block, &health);
    { st.lock().unwrap().prev_block = Some(now_block); }

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
    let lp_key = crate::validator_rpc::storage_map_key("MinerRegistry", "LatestParticipation", &account);
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
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let start = *guard.first_poll_epoch.get_or_insert(now);
    now.saturating_sub(start) < STARTUP_GRACE_SECS
}

#[tauri::command]
pub async fn get_health(app: AppHandle) -> HealthReport {
    let st = app.state::<Mutex<MonitorState>>();
    sample(&app, st.inner()).await
}

/// Spawn the 15 s poll loop; emit `health-changed` and notify on transitions.
pub fn spawn_health_monitor(app: AppHandle) {
    app.manage(Mutex::new(MonitorState::default()));
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
        let mut last_overall: Option<StackHealth> = None;
        loop {
            ticker.tick().await;
            let st = app.state::<Mutex<MonitorState>>();
            let report = sample(&app, st.inner()).await;
            let flipped_to_unhealthy = matches!(report.overall, StackHealth::Unhealthy)
                && !matches!(last_overall, Some(StackHealth::Unhealthy));
            if flipped_to_unhealthy {
                notify_unhealthy(&app, &report);
            }
            last_overall = Some(report.overall.clone());
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
    let _ = app.notification()
        .builder()
        .title("Quip node unhealthy")
        .body(&reason)
        .show();
}
```

Note: `app.notification()` requires the `tauri-plugin-notification` plugin. If not already present, add `tauri-plugin-notification = "2"` to `Cargo.toml`, `.plugin(tauri_plugin_notification::init())` in the builder (Task 7 Step 4), and the `notification:default` permission to `src-tauri/capabilities/*.json`. If the project already has a tray, prefer reusing its notification path; otherwise this plugin is the minimal addition.

- [ ] **Step 4: Register the command + spawn the monitor**

In `src-tauri/src/lib.rs`, add `health::get_health` to the `tauri::generate_handler![...]` list (alongside `compose::get_stack_status`), and call `crate::health::spawn_health_monitor(app.handle().clone());` in the `.setup(|app| { ... })` closure (create one if the builder has none). If adding the notification plugin, register it here too.

- [ ] **Step 5: Build, test, commit**

Run: `cd src-tauri && cargo test && cargo clippy`
Expected: all PASS, clippy clean.

```bash
git add src-tauri/src/health.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/capabilities
git commit -m "feat(health): monitor loop, get_health command, unhealthy notification"
```

---

### Task 8: Publish validator RPC to host in both modes (`stack_assets.rs`)

**Files:**
- Modify: `src-tauri/src/stack_assets.rs:155-169`

**Interfaces:**
- Changes `expose_native_validator_rpc` to publish `127.0.0.1:<validator_rpc_port>:9944` in both run modes (loopback-only; needed so the host health monitor can reach the validator in Docker mode).

- [ ] **Step 1: Update the test to assert both modes publish**

In `stack_assets.rs` tests, find the test around line 291 that passes `&RunMode::Docker`. Add/adjust so a Docker-mode staging asserts the compose contains `127.0.0.1:9944:9944`. Add:

```rust
    #[test]
    fn docker_mode_also_publishes_validator_rpc_to_host() {
        let out = expose_native_validator_rpc(&RunMode::Docker, COMPOSE_WITH_VALIDATOR, 30333, 9944);
        assert!(out.contains("127.0.0.1:9944:9944"),
            "Docker mode must publish validator RPC to host loopback for the health monitor");
    }
```

(Use the same compose fixture the neighboring tests use for `COMPOSE_WITH_VALIDATOR`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test stack_assets::tests::docker_mode_also_publishes_validator_rpc_to_host`
Expected: FAIL — Docker mode currently returns the compose unchanged.

- [ ] **Step 3: Remove the Native-only guard**

In `expose_native_validator_rpc` (line ~161), delete the early return:

```rust
    if !matches!(run_mode, RunMode::Native) {
        return src.to_string();
    }
```

Rename the function to `expose_validator_rpc` (update its call site around line 131 and doc comment lines 152-155 to say "in both run modes"). Keep the `run_mode` parameter only if still used; if it becomes unused, drop it from the signature and call site.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test stack_assets && cargo clippy`
Expected: PASS, clippy clean (no unused-parameter warning).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/stack_assets.rs
git commit -m "feat(health): publish validator RPC to host loopback in both modes"
```

---

### Task 9: Manual end-to-end smoke test

**Files:** none (verification task).

- [ ] **Step 1: Point the app at the live data dir and run**

With the testnet node running (validator RPC live on `127.0.0.1:9944`):

Run: `cd src-tauri && cargo test` (full suite green) then launch `bun run dev`.

- [ ] **Step 2: Observe healthy state**

Expected: within ~30 s the status pill shows Running and the health panel shows infra=ok, chain=ok (block advancing), participation=ok (`participating in qblock N/N`). Cross-check against:

```bash
curl -s -H 'Content-Type: application/json' \
  -d '{"id":1,"jsonrpc":"2.0","method":"state_getStorage","params":["0x9b2c4dbe49d7a1aed7ce99e4b8c072e8a917cf9ea4fd296f6933d75be837c3e1"]}' \
  http://127.0.0.1:9944
```

The decoded u64 (little-endian) should match the panel's `qblock N`.

- [ ] **Step 3: Force an unhealthy transition**

Stop the validator container (`docker stop quip-validator`) or block RPC. Expected: after two polls (~30 s) the pill goes Unhealthy, the panel shows chain=fail ("block stalled" / "rpc unreachable"), and a desktop notification fires once. Restart the validator → recovers to Running, counter resets.

- [ ] **Step 4: Commit any fixes**

If glue bugs surface, fix with a focused commit referencing the dimension involved.

---

### Task 10: Frontend — health panel + status pill integration

**Files:**
- Modify: `src/index.html`
- Modify: `src/app.js`

**Interfaces:**
- Consumes: `get_health` command + `health-changed` event, payload `{ overall, infra:{state,detail}, chain:{...}, participation:{...} }`.

- [ ] **Step 1: Add the health panel markup**

In `src/index.html`, near the existing status pill / stack section, add:

```html
<div id="health-panel" class="config-section-divider">
  <div class="section-heading">Node Health</div>
  <div class="health-row"><span class="health-label">Infrastructure</span><span id="health-infra" class="health-badge">—</span></div>
  <div class="health-row"><span class="health-label">Chain</span><span id="health-chain" class="health-badge">—</span></div>
  <div class="health-row"><span class="health-label">Participation</span><span id="health-participation" class="health-badge">—</span></div>
</div>
```

- [ ] **Step 2: Render health in `app.js`**

In `src/app.js`, add a renderer and wire it to both the event and a fallback poll:

```javascript
function renderHealth(report) {
  if (!report) return;
  state.health = report;
  const paint = (id, dim) => {
    const el = document.getElementById(id);
    if (!el || !dim) return;
    el.textContent = `${dim.state}${dim.detail ? ' — ' + dim.detail : ''}`;
    el.dataset.state = dim.state; // css: [data-state=ok|warn|fail|unknown]
  };
  paint('health-infra', report.infra);
  paint('health-chain', report.chain);
  paint('health-participation', report.participation);
  refreshStatusPill();
}

// Event-driven, with a slow fallback poll consistent with the existing 10 s status poll.
listen('health-changed', (e) => renderHealth(e.payload));
setInterval(async () => {
  try { renderHealth(await invoke('get_health')); } catch (_) { /* transient */ }
}, 15000);
```

- [ ] **Step 3: Fold health into the status pill**

Locate `statusFromStack()` (≈ `src/app.js:1375`). Extend it so that when the miner is running and `state.health?.overall` is present, the pill reflects `state.health.overall` (`'running'` / `'degraded'` / `'unhealthy'`), still suppressed while `state.starting`/`state.stopping`. Keep the existing stack-only fallback when `state.health` is undefined. Name the pill refresh helper `refreshStatusPill()` (reuse the existing pill-update function if one exists rather than adding a second).

- [ ] **Step 4: Manual verify**

Run: `bun run dev`. Confirm the panel updates live and the pill matches the panel's worst dimension. Toggle the validator as in Task 9 Step 3 and watch the pill + panel flip.

- [ ] **Step 5: Commit**

```bash
git add src/index.html src/app.js
git commit -m "feat(health): health panel + status pill integration"
```

---

## Self-Review

**Spec coverage:**
- Dimension A (infra, both modes) → Task 7 `check_infra` + Task 8 (Docker RPC reachability). ✓
- Dimension B (chain live, ~18 s) → Task 3 RPC + Task 5 `check_chain` (block-advance + peers + syncing). ✓
- Dimension C (participation vs current qblockid, chain-sourced) → Tasks 1–4 (keys/decode/RPC/account) + Task 5 `check_participation` (within-1 + startup grace). ✓
- Log as diagnostic-only → unchanged log_stream; not a gate (by omission — no log gating added). ✓
- Indicate + notify → Task 7 notify + Task 10 panel/pill. ✓
- 15 s cadence, debounce 2 polls → Task 6 `debounce` + Task 7 loop. ✓
- Error handling (RPC unreachable → Degraded via Unknown; decode failure → Unknown) → Task 7 `probe_*`. ✓
- Testing (roll-up table, chain pairs, participation fixtures, SCALE fixtures) → Tasks 1,2,5,6. ✓

**Placeholder scan:** No TBD/TODO. The one runtime-API uncertainty (`XxHash64::oneshot`) is guarded by the known-answer test in Task 1 Step 3, which is a concrete acceptance criterion, not a placeholder.

**Type consistency:** `DimensionStatus`/`DimensionState`/`HealthReport`/`StackHealth`, `check_infra`/`check_chain`/`check_participation`/`roll_up`/`debounce`, `ValidatorRpc`/`SystemHealth`/`storage_u64`/`storage_value_key`/`storage_map_key`/`decode_u64_le`/`read_account_id` used consistently across tasks. `refreshStatusPill()` named once and reused.

**Confirmed signatures (verified against the tree 2026-07-02):**
- `settings::load_settings() -> AppSettings` and `settings::data_dir() -> PathBuf` take **no** `app` arg; `AppSettings { node_config, run_mode }` fields exist (`settings.rs:367-382,481,516`). Task 7 reflects this.
- `compose::get_stack_status() -> Result<StackStatus, String>` is async, **no** `app` arg (`compose.rs:1034`).
- `native::get_native_node_status(state: State<NativeProcessState>) -> Result<NativeNodeStatus, String>` is an async command over managed state; Task 7 fetches the state via `app.state()` (`native.rs:993`). `NativeNodeStatus { running, pid }`.
- `native::native_miner_validator_url(&NodeConfig)` is `pub(crate)` — callable from `health.rs` (`native.rs:219`).
- `lib.rs` already has `.setup(|app| { … })` (line 115) and `generate_handler!` (line 68) — extend both.
- No notification plugin present yet → Task 7 adds `tauri-plugin-notification`.

**Open at implementation (compiler-resolved details):**
- Exact borrow form for `State::inner()` across `.await` in `sample`/`get_health` (Task 7 uses `st.inner()`); adjust if the borrow checker requires a local binding.
