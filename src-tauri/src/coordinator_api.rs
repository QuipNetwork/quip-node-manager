// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only client for the coordinator's `/api/v1` dashboard surface.
//!
//! The coordinator is the only process that knows which account it signs with:
//! it derives that account from the hybrid keypair it loads from the signer
//! key, and `GET /api/v1/status` reports the result. Nothing on disk carries
//! the same value — `quip-coordinator keygen` writes only `master_seed_hex` —
//! so anything that needs the account must ask the running process for it.

use crate::settings::{AppSettings, RunMode};
use std::time::Duration;

const STATUS_PATH: &str = "/api/v1/status";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Native probes run on the host. Docker probes run inside the stack.
#[derive(Debug, PartialEq)]
pub enum StatusEndpoint {
    Native(String),
    Docker,
}

pub fn status_endpoint(settings: &AppSettings) -> StatusEndpoint {
    match settings.run_mode {
        RunMode::Native => StatusEndpoint::Native(format!(
            "http://127.0.0.1:{}{STATUS_PATH}",
            crate::config::native_rest_port(&settings.node_config)
        )),
        RunMode::Docker => StatusEndpoint::Docker,
    }
}

/// Pull `data.account_id_hex` out of a status envelope.
///
/// An unkeyed coordinator answers 200 with an empty string here rather than an
/// error, so an empty value is reported as the distinct "no signing identity"
/// state instead of decoding to the all-zero account.
pub fn parse_status_account_id(body: &str) -> Result<[u8; 32], String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("coordinator status not json: {e}"))?;
    let s = v
        .pointer("/data/account_id_hex")
        .and_then(|x| x.as_str())
        .ok_or("coordinator status has no data.account_id_hex")?;
    if s.is_empty() {
        return Err("coordinator has no signing identity".to_string());
    }
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| format!("bad account_id_hex: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| "account_id_hex is not 32 bytes".to_string())
}

/// The account the coordinator signs extrinsics with.
pub async fn fetch_account_id(endpoint: &StatusEndpoint) -> Result<[u8; 32], String> {
    let body = match endpoint {
        StatusEndpoint::Docker => {
            crate::container_http::request("quip-miner", 8086, "GET", STATUS_PATH, "").await?
        }
        StatusEndpoint::Native(url) => reqwest::Client::new()
            .get(url)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("coordinator unreachable at {url}: {e}"))?
            .error_for_status()
            .map_err(|e| format!("coordinator HTTP error: {e}"))?
            .text()
            .await
            .map_err(|e| format!("coordinator status body: {e}"))?,
    };
    parse_status_account_id(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::NodeConfig;

    const ACCOUNT: &str = "b394c09ba995e070273342ab868f1017fd6cd9d35e752265082edbcb85dd204a";

    fn envelope(account_id_hex: &str) -> String {
        format!(r#"{{"data":{{"account_id_hex":"{account_id_hex}","is_mining":true}}}}"#)
    }

    fn docker(public_host: &str, hostname: &str, port: u16) -> AppSettings {
        AppSettings {
            run_mode: RunMode::Docker,
            hostname: hostname.to_string(),
            node_config: NodeConfig {
                port,
                public_host: public_host.to_string(),
                ..NodeConfig::default()
            },
            ..AppSettings::default()
        }
    }

    #[test]
    fn native_endpoint_talks_to_the_coordinator_directly() {
        let settings = AppSettings {
            run_mode: RunMode::Native,
            ..AppSettings::default()
        };
        assert_eq!(
            status_endpoint(&settings),
            StatusEndpoint::Native("http://127.0.0.1:20100/api/v1/status".into())
        );
    }

    #[test]
    fn native_endpoint_follows_the_configured_rest_port() {
        let settings = AppSettings {
            run_mode: RunMode::Native,
            node_config: NodeConfig {
                rest_insecure_port: 20123,
                ..NodeConfig::default()
            },
            ..AppSettings::default()
        };
        assert_eq!(
            status_endpoint(&settings),
            StatusEndpoint::Native("http://127.0.0.1:20123/api/v1/status".into())
        );
    }

    #[test]
    fn docker_probes_do_not_depend_on_public_bindings_or_tls() {
        for host in ["", "203.0.113.9", "node.example.com"] {
            let mut settings = docker(host, ":20049", 21049);
            settings.node_config.public_api_enabled = false;
            assert_eq!(status_endpoint(&settings), StatusEndpoint::Docker);
        }
    }

    #[test]
    fn parses_the_account_from_a_status_envelope() {
        let id = parse_status_account_id(&envelope(&format!("0x{ACCOUNT}"))).unwrap();
        assert_eq!(hex::encode(id), ACCOUNT);
    }

    #[test]
    fn parses_an_unprefixed_account() {
        let id = parse_status_account_id(&envelope(ACCOUNT)).unwrap();
        assert_eq!(hex::encode(id), ACCOUNT);
    }

    /// An unkeyed coordinator answers 200 with an empty identity. That must not
    /// decode to the all-zero account, which would read an unrelated key.
    #[test]
    fn empty_identity_is_an_error_not_the_zero_account() {
        let err = parse_status_account_id(&envelope("")).unwrap_err();
        assert!(err.contains("no signing identity"), "{err}");
    }

    #[test]
    fn missing_field_short_wrong_length_and_non_json_all_error() {
        assert!(parse_status_account_id(r#"{"data":{"is_mining":true}}"#).is_err());
        assert!(parse_status_account_id(&envelope("0xdeadbeef")).is_err());
        assert!(parse_status_account_id(&envelope("0xzz")).is_err());
        assert!(parse_status_account_id("not json").is_err());
    }
}
