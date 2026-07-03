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

/// Decode the leading SCALE `u64` (little-endian) from a storage value.
pub fn decode_u64_le(bytes: &[u8]) -> Option<u64> {
    let head: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
    Some(u64::from_le_bytes(head))
}

// ── Task 3: read-only JSON-RPC client ────────────────────────────────────────

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

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "id": 1,
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let resp = reqwest::Client::new()
            .post(&self.base)
            .json(&body)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("{method} request failed: {e}"))?;
        let v: serde_json::Value =
            resp.json().await.map_err(|e| format!("{method} bad json: {e}"))?;
        v.get("result").cloned().ok_or_else(|| format!("{method}: no result field"))
    }

    pub async fn current_block(&self) -> Result<u64, String> {
        let r = self.call("chain_getHeader", serde_json::json!([])).await?;
        let num = r
            .get("number")
            .and_then(|n| n.as_str())
            .ok_or("header has no number")?;
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
            None => Ok(None),
            Some(s) => {
                let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
                    .map_err(|e| format!("bad storage hex: {e}"))?;
                Ok(decode_u64_le(&bytes))
            }
        }
    }
}

// ── Task 4: Account id from keystore ─────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twox_128_matches_known_substrate_vectors() {
        assert_eq!(hex::encode(twox_128(b"System")), "26aa394eea5630e07c48ae0c9558cef7");
        assert_eq!(hex::encode(twox_128(b"Account")), "b99d880ec681799c0cf30e8886371da9");
    }

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
            hex::decode("b4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a")
                .unwrap();
        assert_eq!(
            hex::encode(storage_map_key("MinerRegistry", "LatestParticipation", &account)),
            "491850926eb92ce9861fdcc5504d045e3b989607de2d2e007e711506edd2b037\
c3d972b93e95d78499f6ca768214647b\
b4e65b8ce157ce9ec3aa818920e7b81b04a23fdce38cf2374eee037d4320da7a"
        );
    }

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

    #[test]
    fn parses_hex_block_number() {
        // 0x81b5b = 8*65536 + 4096 + 11*256 + 80 + 11 = 531291
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
}
