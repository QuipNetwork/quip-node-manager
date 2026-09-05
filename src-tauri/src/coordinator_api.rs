// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only client for the coordinator's `/api/v1` dashboard surface.
//!
//! The coordinator is the only process that knows which account it signs with:
//! it derives that account from the hybrid keypair it loads from the signer
//! key, and `GET /api/v1/status` reports the result. Nothing on disk carries
//! the same value — `quip-coordinator keygen` writes only `master_seed_hex` —
//! so anything that needs the account must ask the running process for it.

use crate::settings::{AppSettings, RunMode};
use std::net::SocketAddr;
use std::time::Duration;

const STATUS_PATH: &str = "/api/v1/status";
const TIMEOUT: Duration = Duration::from_secs(5);

/// How to reach `/api/v1/status` from the host.
#[derive(Debug, PartialEq)]
pub struct StatusEndpoint {
    pub url: String,
    /// `(host, addr)` override for the name resolution reqwest would otherwise
    /// perform. Set only for Caddy's TLS front door, where the request must
    /// carry the certificate's hostname but still connect over loopback.
    pub resolve: Option<(String, SocketAddr)>,
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Pick the endpoint for the current run mode.
///
/// Native: the coordinator runs on the host and binds `[dashboard].listen`
/// itself (see `config::native_rest_port`), so the probe talks to it directly.
///
/// Docker: the miner container publishes no host port, so the only route is
/// Caddy's `handle /api/v1/*` on the public API port. That listener is plain
/// HTTP for a port-only site and auto-TLS once a public DNS host is set, so
/// the scheme follows the resolved Caddy site address. In the TLS case the
/// request names the certificate's host but resolves to loopback, which keeps
/// certificate validation on without depending on public DNS resolving back
/// to this machine.
pub fn status_endpoint(settings: &AppSettings) -> StatusEndpoint {
    let cfg = &settings.node_config;
    match settings.run_mode {
        RunMode::Native => StatusEndpoint {
            url: format!(
                "http://127.0.0.1:{}{STATUS_PATH}",
                crate::config::native_rest_port(cfg)
            ),
            resolve: None,
        },
        RunMode::Docker => {
            let port = cfg.port;
            match crate::hostnames::caddy_tls_host(&cfg.public_host, &settings.hostname) {
                Some(host) => StatusEndpoint {
                    url: format!("https://{host}:{port}{STATUS_PATH}"),
                    resolve: Some((host, loopback(port))),
                },
                None => StatusEndpoint {
                    url: format!("http://127.0.0.1:{port}{STATUS_PATH}"),
                    resolve: None,
                },
            }
        }
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
    let mut builder = reqwest::Client::builder().timeout(TIMEOUT);
    if let Some((host, addr)) = &endpoint.resolve {
        builder = builder.resolve(host, *addr);
    }
    let client = builder
        .build()
        .map_err(|e| format!("cannot build http client: {e}"))?;
    let body = client
        .get(&endpoint.url)
        .send()
        .await
        .map_err(|e| format!("coordinator unreachable at {}: {e}", endpoint.url))?
        .text()
        .await
        .map_err(|e| format!("coordinator status body: {e}"))?;
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
            StatusEndpoint {
                url: "http://127.0.0.1:20100/api/v1/status".to_string(),
                resolve: None,
            }
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
        assert!(status_endpoint(&settings)
            .url
            .starts_with("http://127.0.0.1:20123/"));
    }

    #[test]
    fn docker_port_only_site_is_plain_http_on_the_public_api_port() {
        assert_eq!(
            status_endpoint(&docker("", ":20049", 20052)),
            StatusEndpoint {
                url: "http://127.0.0.1:20052/api/v1/status".to_string(),
                resolve: None,
            }
        );
    }

    /// An IP or localhost `public_host` still yields a port-only Caddy site, so
    /// the probe must stay on plain HTTP.
    #[test]
    fn docker_non_dns_public_host_stays_plain_http() {
        for host in ["203.0.113.9", "localhost", "quip.local"] {
            assert!(
                status_endpoint(&docker(host, ":20049", 20049))
                    .url
                    .starts_with("http://127.0.0.1:20049/"),
                "{host} must not select the TLS front door"
            );
        }
    }

    /// A public DNS host puts Caddy on auto-TLS. The request must name the host
    /// so the certificate validates, while connecting to loopback.
    #[test]
    fn docker_dns_host_uses_tls_pinned_to_loopback() {
        assert_eq!(
            status_endpoint(&docker("node.example.com", ":20049", 20049)),
            StatusEndpoint {
                url: "https://node.example.com:20049/api/v1/status".to_string(),
                resolve: Some(("node.example.com".to_string(), loopback(20049))),
            }
        );
    }

    /// The hostname field carries the pre-formatted two-address form when the
    /// user set it directly rather than through `public_host`.
    #[test]
    fn docker_dns_hostname_field_uses_tls_too() {
        assert_eq!(
            status_endpoint(&docker(
                "",
                "node.example.com, node.example.com:20049",
                20049
            ))
            .url,
            "https://node.example.com:20049/api/v1/status"
        );
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
