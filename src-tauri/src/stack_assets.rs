// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage the docker-compose stack files into the user's data dir so
//! `docker compose` can run with `--project-directory`.
//!
//! The compose.yml and Caddyfile are embedded into the binary at compile
//! time via `include_str!`. That avoids Tauri's resource-bundler path
//! entirely, so a raw exe (e.g. Windows `--no-bundle` builds that ship
//! just `quip-node-manager.exe` with no sibling resource folder) still
//! has the files available at runtime.
//!
//! Runtime patches are applied before writing:
//!   - compose.yml: Caddy's host-published public API port is rewritten
//!     from upstream 20049 to the user configured API port.
//!   - compose.yml: the validator libp2p host port is rewritten from
//!     upstream 30333 to the manager default/configured 30033.
//!   - Caddyfile (Native mode only): `/api/v1/*` upstream is rewritten
//!     from `quip-miner:80` (compose network alias, absent when the miner
//!     is on the host) to `host.docker.internal:<native_rest_port>`.

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

/// Public API port inside the Caddy container. The host side is configurable.
const CONTAINER_PUBLIC_API_PORT: u16 = 20049;
/// Validator libp2p port inside the validator container. The host side
/// defaults to 30033; `--public-addr` is intentionally deferred until the
/// running v0.2 stack proves whether it is needed.
const CONTAINER_VALIDATOR_PORT: u16 = 30333;

/// `<data_dir>/docker-compose.yml` — staged from the embedded bytes.
pub fn stack_compose_file() -> PathBuf {
    data_dir().join("docker-compose.yml")
}

/// `<data_dir>/caddy/Caddyfile` — staged from the embedded bytes, possibly
/// patched for Native mode.
pub fn stack_caddyfile() -> PathBuf {
    data_dir().join("caddy").join("Caddyfile")
}

/// `--project-directory` for every `docker compose` invocation.
pub fn stack_project_dir() -> PathBuf {
    data_dir()
}

/// Write the embedded compose.yml + Caddyfile into `<data_dir>/` and create
/// the subdirectories compose bind-mounts. Idempotent — always overwrites.
///
/// `public_api_port` replaces Caddy's upstream host-side `20049`.
/// `validator_port` replaces the validator's upstream host-side `30333`
/// while preserving the container-internal `30333`.
///
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is also
/// rewritten from `quip-miner:80` to `host.docker.internal:<rest_port>`.
pub fn sync_stack_assets(
    run_mode: &RunMode,
    public_api_port: u16,
    validator_port: u16,
    native_rest_port: u16,
) -> Result<(), String> {
    let base = data_dir();
    for sub in ["data", "dashboard-data", "caddy"] {
        fs::create_dir_all(base.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    let compose_out = patch_compose_ports(COMPOSE_YML, public_api_port, validator_port);
    fs::write(stack_compose_file(), compose_out)
        .map_err(|e| format!("write docker-compose.yml: {e}"))?;

    let caddy_out = patch_caddyfile(run_mode, CADDYFILE, native_rest_port);
    fs::write(stack_caddyfile(), caddy_out).map_err(|e| format!("write Caddyfile: {e}"))?;

    Ok(())
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

fn patch_caddyfile(run_mode: &RunMode, src: &str, native_rest_port: u16) -> String {
    match run_mode {
        RunMode::Native => src.replace(
            "quip-miner:80",
            &format!("host.docker.internal:{native_rest_port}"),
        ),
        RunMode::Docker => src.to_string(),
    }
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
    fn docker_caddyfile_keeps_v02_routes_and_miner_upstream() {
        let patched = patch_caddyfile(&RunMode::Docker, CADDYFILE, 20100);
        assert!(patched.contains("handle /rpc"));
        assert!(patched.contains("handle /api/v1/*"));
        assert!(patched.contains("reverse_proxy quip-miner:80"));
        assert!(!patched.contains("host.docker.internal:20100"));
    }

    #[test]
    fn native_caddyfile_rewrites_only_miner_upstream() {
        let patched = patch_caddyfile(&RunMode::Native, CADDYFILE, 20100);
        assert!(patched.contains("handle /rpc"));
        assert!(patched.contains("handle /api/v1/*"));
        assert!(patched.contains("reverse_proxy host.docker.internal:20100"));
        assert!(!patched.contains("reverse_proxy quip-miner:80"));
    }
}
