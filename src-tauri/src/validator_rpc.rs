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
}
