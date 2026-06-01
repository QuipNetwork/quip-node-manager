// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage the docker-compose stack files into the user's data dir so
//! `docker compose` can run with `--project-directory`.
//!
//! The compose.yml, Caddyfile, and chain spec are embedded into the binary at
//! compile time via `include_str!`. That avoids Tauri's resource-bundler path
//! entirely, so a raw exe (e.g. Windows `--no-bundle` builds that ship
//! just `quip-node-manager.exe` with no sibling resource folder) still
//! has the files available at runtime.
//!
//! Runtime patches are applied before writing:
//!   - compose.yml: Caddy's host-published public API port is rewritten
//!     from upstream 20049 to the user configured API port.
//!   - compose.yml: the validator libp2p host port is rewritten from
//!     upstream 30333 to the manager default/configured 30033.
//!   - compose.yml: when `public_host` is set, the validator command gets
//!     a matching `--public-addr=<multiaddr>` using the public validator port.
//!   - Caddyfile: the optional local faucet route is stripped; the manager
//!     uses the public testnet faucet exposed by the upstream miner bootstrap.
//!   - Caddyfile (Native mode only): `/api/v1/*` upstream is rewritten
//!     from `quip-miner:80` (compose network alias, absent when the miner
//!     is on the host) to `host.docker.internal:<native_rest_port>`.
//!   - compose.yml (Native mode only): the validator's JSON-RPC port is
//!     published on the host loopback (127.0.0.1:9944) so the host-side
//!     miner can connect to `ws://127.0.0.1:9944` directly.

use crate::settings::{data_dir, RunMode};
use std::fs;
use std::path::PathBuf;

/// Upstream compose.yml, embedded at compile time from the vendored
/// `nodes.quip.network` submodule. rustc's dep-info tracks the included
/// path so `cargo build` rebuilds whenever the file changes.
const COMPOSE_YML: &str = include_str!("../../vendor/nodes.quip.network/docker-compose.yml");

/// Upstream Caddyfile, embedded alongside the compose.yml. Patched at
/// runtime for Native mode (see `sync_stack_assets`).
const CADDYFILE: &str = include_str!("../../vendor/nodes.quip.network/caddy/Caddyfile");

/// Canonical Quip testnet chain spec referenced by the v0.2 compose file.
const CHAIN_SPEC: &str =
    include_str!("../../vendor/nodes.quip.network/chain-specs/quip-testnet.json");

/// Public API port inside the Caddy container. The host side is configurable.
const CONTAINER_PUBLIC_API_PORT: u16 = 20049;
/// Validator libp2p port inside the validator container. The host side
/// defaults to 30033 and is also used for generated `--public-addr` values.
const CONTAINER_VALIDATOR_PORT: u16 = 30333;
/// Validator JSON-RPC port. In Native mode it's published on the host
/// loopback so the host-side miner can reach `ws://127.0.0.1:9944` directly,
/// rather than going through Caddy's `/rpc` route.
const CONTAINER_VALIDATOR_RPC_PORT: u16 = 9944;
const HOST_VALIDATOR_RPC_PORT: u16 = 9944;

/// `<data_dir>/docker-compose.yml` — staged from the embedded bytes.
pub fn stack_compose_file() -> PathBuf {
    data_dir().join("docker-compose.yml")
}

/// `<data_dir>/caddy/Caddyfile` — staged from the embedded bytes, possibly
/// patched for Native mode.
pub fn stack_caddyfile() -> PathBuf {
    data_dir().join("caddy").join("Caddyfile")
}

/// `<data_dir>/chain-specs/quip-testnet.json` — staged from embedded bytes.
pub fn stack_chain_spec_file() -> PathBuf {
    data_dir().join("chain-specs").join("quip-testnet.json")
}

/// `--project-directory` for every `docker compose` invocation.
pub fn stack_project_dir() -> PathBuf {
    data_dir()
}

/// Write the embedded compose.yml, Caddyfile, and chain spec into
/// `<data_dir>/`, and create the subdirectories compose bind-mounts.
/// Idempotent — always overwrites.
///
/// `public_api_port` replaces Caddy's upstream host-side `20049`.
/// `validator_port` replaces the validator's upstream host-side `30333`
/// while preserving the container-internal `30333`.
/// `public_host`, when set, is converted into a Substrate public multiaddr
/// using `validator_port`.
///
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is also
/// rewritten from `quip-miner:80` to `host.docker.internal:<rest_port>`.
pub fn sync_stack_assets(
    run_mode: &RunMode,
    public_api_port: u16,
    validator_port: u16,
    public_host: &str,
    native_rest_port: u16,
) -> Result<(), String> {
    let base = data_dir();
    for sub in ["data", "dashboard-data", "caddy", "chain-specs"] {
        fs::create_dir_all(base.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    let compose_out = patch_compose_file(
        run_mode,
        COMPOSE_YML,
        public_api_port,
        validator_port,
        public_host,
    );
    fs::write(stack_compose_file(), compose_out)
        .map_err(|e| format!("write docker-compose.yml: {e}"))?;

    let caddy_out = patch_caddyfile(run_mode, CADDYFILE, native_rest_port);
    fs::write(stack_caddyfile(), caddy_out).map_err(|e| format!("write Caddyfile: {e}"))?;

    fs::write(stack_chain_spec_file(), CHAIN_SPEC).map_err(|e| format!("write chain spec: {e}"))?;

    Ok(())
}

fn patch_compose_file(
    run_mode: &RunMode,
    src: &str,
    public_api_port: u16,
    validator_port: u16,
    public_host: &str,
) -> String {
    let patched = patch_compose_ports(src, public_api_port, validator_port);
    let patched = expose_native_validator_rpc(run_mode, &patched, validator_port);
    patch_validator_public_addr(&patched, public_host, validator_port)
}

/// In Native mode, publish the validator's JSON-RPC port on the host loopback
/// so the host-side miner can reach `ws://127.0.0.1:9944`. In Docker mode the
/// miner is a container and reaches `quip-validator:9944` over the compose
/// network, so no host publish is needed.
fn expose_native_validator_rpc(run_mode: &RunMode, src: &str, validator_port: u16) -> String {
    if !matches!(run_mode, RunMode::Native) {
        return src.to_string();
    }
    // Anchor on the (already port-patched) validator UDP mapping so the RPC
    // mapping lands inside the quip-validator service's `ports:` list.
    let udp_line = format!("      - \"{validator_port}:{CONTAINER_VALIDATOR_PORT}/udp\"\n");
    let rpc_line =
        format!("      - \"127.0.0.1:{HOST_VALIDATOR_RPC_PORT}:{CONTAINER_VALIDATOR_RPC_PORT}\"\n");
    src.replacen(&udp_line, &format!("{udp_line}{rpc_line}"), 1)
}

/// Remap host sides of canonical upstream `HOST:CONTAINER` port directives.
fn patch_compose_ports(src: &str, public_api_port: u16, validator_port: u16) -> String {
    src.replace(
        &format!("\"{CONTAINER_PUBLIC_API_PORT}:{CONTAINER_PUBLIC_API_PORT}\""),
        &format!("\"{public_api_port}:{CONTAINER_PUBLIC_API_PORT}\""),
    )
    .replace(
        &format!("\"{CONTAINER_VALIDATOR_PORT}:{CONTAINER_VALIDATOR_PORT}/tcp\""),
        &format!("\"{validator_port}:{CONTAINER_VALIDATOR_PORT}/tcp\""),
    )
    .replace(
        &format!("\"{CONTAINER_VALIDATOR_PORT}:{CONTAINER_VALIDATOR_PORT}/udp\""),
        &format!("\"{validator_port}:{CONTAINER_VALIDATOR_PORT}/udp\""),
    )
}

fn patch_validator_public_addr(src: &str, public_host: &str, validator_port: u16) -> String {
    let Some(public_addr) = crate::hostnames::validator_public_addr(public_host, validator_port)
    else {
        return src.to_string();
    };
    let validator_arg = "      - --validator\n";
    src.replacen(
        validator_arg,
        &format!("{validator_arg}      - --public-addr={public_addr}\n"),
        1,
    )
}

fn patch_caddyfile(run_mode: &RunMode, src: &str, native_rest_port: u16) -> String {
    let src = strip_local_faucet_route(src);
    match run_mode {
        RunMode::Native => src.replace(
            "quip-miner:80",
            &format!("host.docker.internal:{native_rest_port}"),
        ),
        RunMode::Docker => src,
    }
}

fn strip_local_faucet_route(src: &str) -> String {
    let Some(start) = src.find("\t# Optional faucet sidecar") else {
        return src.to_string();
    };
    let Some(relative_end) = src[start..].find("\n\n\t# Miner telemetry") else {
        return src.to_string();
    };

    let end = start + relative_end + 2;
    let mut out = String::with_capacity(src.len());
    out.push_str(&src[..start]);
    out.push_str(&src[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_compose_ports_noop_for_upstream_defaults() {
        let patched = patch_compose_ports(
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            CONTAINER_VALIDATOR_PORT,
        );
        assert_eq!(patched, COMPOSE_YML);
    }

    #[test]
    fn patch_compose_ports_remaps_public_api_port() {
        let patched = patch_compose_ports(COMPOSE_YML, 20052, CONTAINER_VALIDATOR_PORT);
        assert!(patched.contains("\"20052:20049\""));
        assert!(!patched.contains("\"20049:20049\""));
    }

    #[test]
    fn patch_compose_ports_remaps_validator_tcp_and_udp() {
        let patched = patch_compose_ports(COMPOSE_YML, CONTAINER_PUBLIC_API_PORT, 30033);
        assert!(patched.contains("\"30033:30333/tcp\""));
        assert!(patched.contains("\"30033:30333/udp\""));
        assert!(!patched.contains("\"30333:30333/tcp\""));
        assert!(!patched.contains("\"30333:30333/udp\""));
    }

    #[test]
    fn patch_compose_ports_uses_manager_validator_default() {
        let patched = patch_compose_ports(COMPOSE_YML, CONTAINER_PUBLIC_API_PORT, 30033);
        assert!(patched.contains("\"20049:20049\""));
        assert!(patched.contains("\"30033:30333/tcp\""));
        assert!(patched.contains("\"30033:30333/udp\""));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_dns_public_host() {
        let patched = patch_compose_file(
            &RunMode::Docker,
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "node.example.com",
        );
        assert!(patched.contains("      - --public-addr=/dns4/node.example.com/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_ip_public_host() {
        let patched = patch_compose_file(
            &RunMode::Docker,
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "1.2.3.4",
        );
        assert!(patched.contains("      - --public-addr=/ip4/1.2.3.4/tcp/30033\n"));

        let patched = patch_compose_file(
            &RunMode::Docker,
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "[2001:db8::1]",
        );
        assert!(patched.contains("      - --public-addr=/ip6/2001:db8::1/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_omits_public_addr_when_public_host_is_empty() {
        let patched = patch_compose_file(
            &RunMode::Docker,
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "",
        );
        assert!(!patched.contains("--public-addr"));
    }

    #[test]
    fn native_mode_publishes_validator_rpc_on_loopback() {
        let patched = patch_compose_file(
            &RunMode::Native,
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "",
        );
        assert!(patched.contains("      - \"127.0.0.1:9944:9944\"\n"));
        // Inserted right after the validator's UDP mapping, inside its ports.
        assert!(patched.contains("\"30033:30333/udp\"\n      - \"127.0.0.1:9944:9944\""));
    }

    #[test]
    fn docker_mode_does_not_publish_validator_rpc() {
        let patched = patch_compose_file(
            &RunMode::Docker,
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "",
        );
        assert!(!patched.contains("9944:9944"));
    }

    #[test]
    fn embedded_chain_spec_is_quip_testnet_json() {
        assert!(CHAIN_SPEC.contains("\"name\": \"Quip Testnet\""));
        assert!(CHAIN_SPEC.contains("\"bootNodes\""));
    }

    #[test]
    fn docker_caddyfile_keeps_v02_routes_and_miner_upstream() {
        let patched = patch_caddyfile(&RunMode::Docker, CADDYFILE, 20100);
        assert!(patched.contains("handle /rpc"));
        assert!(patched.contains("handle /api/v1/*"));
        assert!(patched.contains("reverse_proxy quip-miner:80"));
        assert!(!patched.contains("/api/faucet"));
        assert!(!patched.contains("quip-faucet"));
        assert!(!patched.contains("host.docker.internal:20100"));
    }

    #[test]
    fn native_caddyfile_rewrites_only_miner_upstream() {
        let patched = patch_caddyfile(&RunMode::Native, CADDYFILE, 20100);
        assert!(patched.contains("handle /rpc"));
        assert!(patched.contains("handle /api/v1/*"));
        assert!(patched.contains("reverse_proxy host.docker.internal:20100"));
        assert!(!patched.contains("reverse_proxy quip-miner:80"));
        assert!(!patched.contains("/api/faucet"));
        assert!(!patched.contains("quip-faucet"));
    }
}
