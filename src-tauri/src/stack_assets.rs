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
//! In Native mode the Caddyfile is patched so `/api/v1/*` reaches the
//! host-bound native binary via `host.docker.internal` instead of the
//! `quip-node` compose alias (which doesn't exist when the node runs
//! on the host rather than in a container).

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
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is rewritten from
/// `quip-node:80` (compose network alias, absent when node is on the host)
/// to `host.docker.internal:<native_rest_port>`. Docker mode writes the
/// file verbatim.
pub fn sync_stack_assets(
    run_mode: &RunMode,
    native_rest_port: u16,
) -> Result<(), String> {
    let base = data_dir();
    for sub in ["data", "dashboard-data", "caddy"] {
        fs::create_dir_all(base.join(sub))
            .map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    fs::write(stack_compose_file(), COMPOSE_YML)
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
