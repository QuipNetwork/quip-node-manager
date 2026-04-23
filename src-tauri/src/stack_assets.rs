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
//! Two runtime patches are applied before writing:
//!   - compose.yml: the node's host-published port mappings are rewritten
//!     from the upstream default (20049) to whatever the user configured.
//!     The container-internal port is always 20049 — changing the host
//!     side lets the user pick any router-forwarded port without touching
//!     config.toml.
//!   - Caddyfile (Native mode only): `/api/v1/*` upstream is rewritten
//!     from `quip-node:80` (compose network alias, absent when node is on
//!     the host) to `host.docker.internal:<native_rest_port>`.

use crate::settings::{data_dir, RunMode};
use std::fs;
use std::path::PathBuf;

/// Upstream compose.yml, embedded at compile time from the vendored
/// `nodes.quip.network` submodule. rustc's dep-info tracks the included
/// path so `cargo build` rebuilds whenever the file changes.
const COMPOSE_YML: &str =
    include_str!("../../vendor/nodes.quip.network/docker-compose.yml");

/// Upstream Caddyfile, embedded alongside the compose.yml. Patched at
/// runtime for Native mode (see `sync_stack_assets`).
const CADDYFILE: &str =
    include_str!("../../vendor/nodes.quip.network/caddy/Caddyfile");

/// Internal node port inside every compose container. Never changes.
/// User-visible port changes only remap the host side of the publish.
const CONTAINER_NODE_PORT: u16 = 20049;

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
/// `public_port` replaces the upstream compose.yml's hardcoded `20049` on
/// the host side of every node port mapping (cpu/cuda/qpu, UDP+TCP). The
/// container-internal port stays 20049 so the node never has to be told a
/// different port.
///
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is also
/// rewritten from `quip-node:80` to `host.docker.internal:<rest_port>`.
pub fn sync_stack_assets(
    run_mode: &RunMode,
    public_port: u16,
    native_rest_port: u16,
) -> Result<(), String> {
    let base = data_dir();
    for sub in ["data", "dashboard-data", "caddy"] {
        fs::create_dir_all(base.join(sub))
            .map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    let compose_out = patch_compose_ports(COMPOSE_YML, public_port);
    fs::write(stack_compose_file(), compose_out)
        .map_err(|e| format!("write docker-compose.yml: {e}"))?;

    let caddy_out = match run_mode {
        RunMode::Native => CADDYFILE.replace(
            "quip-node:80",
            &format!("host.docker.internal:{native_rest_port}"),
        ),
        RunMode::Docker => CADDYFILE.to_string(),
    };
    fs::write(stack_caddyfile(), caddy_out)
        .map_err(|e| format!("write Caddyfile: {e}"))?;

    Ok(())
}

/// Remap the host side of every `"20049:20049/<proto>"` port directive to
/// `"<public_port>:20049/<proto>"`. A literal string replace is safe here
/// because the upstream compose.yml uses the canonical `HOST:CONTAINER`
/// form with no whitespace around the colon — anything else would fail
/// compose's own parser.
fn patch_compose_ports(src: &str, public_port: u16) -> String {
    if public_port == CONTAINER_NODE_PORT {
        return src.to_string();
    }
    src.replace(
        &format!("\"{CONTAINER_NODE_PORT}:{CONTAINER_NODE_PORT}/udp\""),
        &format!("\"{public_port}:{CONTAINER_NODE_PORT}/udp\""),
    )
    .replace(
        &format!("\"{CONTAINER_NODE_PORT}:{CONTAINER_NODE_PORT}/tcp\""),
        &format!("\"{public_port}:{CONTAINER_NODE_PORT}/tcp\""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_compose_ports_noop_for_default() {
        let patched = patch_compose_ports(COMPOSE_YML, CONTAINER_NODE_PORT);
        assert_eq!(patched, COMPOSE_YML);
    }

    #[test]
    fn patch_compose_ports_remaps_udp_and_tcp() {
        let patched = patch_compose_ports(COMPOSE_YML, 20052);
        assert!(patched.contains("\"20052:20049/udp\""));
        assert!(patched.contains("\"20052:20049/tcp\""));
        assert!(!patched.contains("\"20049:20049/udp\""));
        assert!(!patched.contains("\"20049:20049/tcp\""));
    }
}
