// SPDX-License-Identifier: AGPL-3.0-or-later

use std::net::IpAddr;

/// Caddy listens on this container-internal public API port. Docker may remap
/// the host side, but the Caddyfile site address must keep the internal port.
pub(crate) const CADDY_PUBLIC_API_PORT: u16 = 20049;

/// Extract the host portion from user-facing `public_host` values. Accepts
/// bare hosts, URLs, bracketed IPv6, and host:port strings.
pub(crate) fn public_host_name(input: &str) -> Option<String> {
    let mut value = input.trim();
    if value.is_empty() {
        return None;
    }

    if let Some((_, rest)) = value.split_once("://") {
        value = rest;
    }
    if let Some((_, rest)) = value.rsplit_once('@') {
        value = rest;
    }
    value = value.split(['/', '?', '#']).next().unwrap_or("").trim();
    if value.is_empty() {
        return None;
    }

    let host = if let Some(rest) = value.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").trim()
    } else if value.parse::<IpAddr>().is_ok() {
        value
    } else if value.matches(':').count() == 1 {
        value.split(':').next().unwrap_or("").trim()
    } else {
        value
    }
    .trim_end_matches('.');

    if host.is_empty() || host.contains(char::is_whitespace) {
        None
    } else {
        Some(host.to_string())
    }
}

pub(crate) fn validator_public_addr(public_host: &str, validator_port: u16) -> Option<String> {
    let host = public_host_name(public_host)?;
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(_)) => Some(format!("/ip4/{host}/tcp/{validator_port}")),
        Ok(IpAddr::V6(_)) => Some(format!("/ip6/{host}/tcp/{validator_port}")),
        Err(_) => Some(format!("/dns4/{host}/tcp/{validator_port}")),
    }
}

pub(crate) fn caddy_hostname_from_public_host(public_host: &str) -> Option<String> {
    let host = public_host_name(public_host)?;
    if !is_public_dns_host(&host) {
        return None;
    }
    Some(format!("{host}, {host}:{CADDY_PUBLIC_API_PORT}"))
}

pub(crate) fn resolved_caddy_hostname(public_host: &str, fallback_hostname: &str) -> String {
    if let Some(hostname) = caddy_hostname_from_public_host(public_host) {
        return hostname;
    }

    let fallback = fallback_hostname.trim();
    if fallback.is_empty() {
        format!(":{CADDY_PUBLIC_API_PORT}")
    } else {
        fallback.to_string()
    }
}

fn is_public_dns_host(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    !(lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_host_name_extracts_common_forms() {
        assert_eq!(
            public_host_name("node.example.com"),
            Some("node.example.com".into())
        );
        assert_eq!(
            public_host_name("https://node.example.com:9443/rpc"),
            Some("node.example.com".into())
        );
        assert_eq!(public_host_name("1.2.3.4:30333"), Some("1.2.3.4".into()));
        assert_eq!(
            public_host_name("[2001:db8::1]:30333"),
            Some("2001:db8::1".into())
        );
        assert_eq!(public_host_name("2001:db8::1"), Some("2001:db8::1".into()));
    }

    #[test]
    fn validator_public_addr_uses_multiaddr_protocol_for_host_kind() {
        assert_eq!(
            validator_public_addr("node.example.com", 30033).as_deref(),
            Some("/dns4/node.example.com/tcp/30033")
        );
        assert_eq!(
            validator_public_addr("1.2.3.4", 30033).as_deref(),
            Some("/ip4/1.2.3.4/tcp/30033")
        );
        assert_eq!(
            validator_public_addr("[2001:db8::1]", 30033).as_deref(),
            Some("/ip6/2001:db8::1/tcp/30033")
        );
    }

    #[test]
    fn caddy_hostname_uses_public_dns_and_falls_back_otherwise() {
        assert_eq!(
            resolved_caddy_hostname("node.example.com", ":20049"),
            "node.example.com, node.example.com:20049"
        );
        assert_eq!(resolved_caddy_hostname("203.0.113.9", ":20049"), ":20049");
        assert_eq!(
            resolved_caddy_hostname("", "dashboard.example.com, dashboard.example.com:20049"),
            "dashboard.example.com, dashboard.example.com:20049"
        );
    }
}
