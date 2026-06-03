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
    // A genuine public DNS host (in `public_host`, or pre-formatted in the
    // hostname field) yields a named-host site so Caddy provisions a real cert
    // on :443 + :20049. Everything else — empty, localhost, an IP, a bare or
    // edited port like `localhost:20080` — must resolve to a PORT-ONLY site
    // (`:20049`). Port-only is the only form that (a) serves plain HTTP rather
    // than triggering Caddy's automatic HTTPS, and (b) matches any `Host`
    // header, which is required because the host side of the port is remapped
    // (e.g. 20052->20049) so the browser's Host never matches a named site.
    // The listen port is always the container-internal 20049 regardless of the
    // user's host-side port.
    if let Some(hostname) = caddy_hostname_from_public_host(public_host) {
        return hostname;
    }
    if fallback_has_public_dns_host(fallback_hostname) {
        return fallback_hostname.trim().to_string();
    }
    format!(":{CADDY_PUBLIC_API_PORT}")
}

/// True when the hostname field holds a real public DNS host — including the
/// pre-formatted multi-address form `example.com, example.com:20049`. Used to
/// decide whether to honor the field verbatim (DNS → real TLS) or normalize it
/// to a port-only site (localhost / IP / bare port → plain HTTP any-host).
fn fallback_has_public_dns_host(fallback: &str) -> bool {
    let first = fallback.split(',').next().unwrap_or("").trim();
    public_host_name(first)
        .map(|h| is_public_dns_host(&h))
        .unwrap_or(false)
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

    #[test]
    fn caddy_hostname_normalizes_non_public_fallbacks_to_port_only() {
        // localhost / IP / bare-or-edited port must NOT become a named site
        // (which would trigger auto-HTTPS + host matching). They collapse to a
        // port-only `:20049` so Caddy serves plain HTTP for any Host header.
        for fallback in [
            "",
            "localhost",
            "localhost:20080",
            "127.0.0.1",
            "127.0.0.1:20049",
            ":20080",
            "node.local",
        ] {
            assert_eq!(
                resolved_caddy_hostname("", fallback),
                ":20049",
                "fallback {fallback:?} should normalize to port-only"
            );
        }
        // A real DNS host in the field is still honored verbatim.
        assert_eq!(
            resolved_caddy_hostname("", "node.example.com"),
            "node.example.com"
        );
    }
}
