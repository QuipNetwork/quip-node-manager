// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage the docker-compose stack files from bundled resources into the
//! user's data dir so `docker compose` can run with `--project-directory`.
//!
//! In Native mode the Caddyfile is patched so `/api/v1/*` reaches the
//! host-bound native binary via `host.docker.internal` instead of the
//! `quip-node` compose alias (which doesn't exist when the node runs
//! on the host rather than in a container).

use crate::settings::{data_dir, RunMode};
use std::fs;
use std::path::PathBuf;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

/// `<data_dir>/docker-compose.yml` — staged from the bundle.
pub fn stack_compose_file() -> PathBuf {
    data_dir().join("docker-compose.yml")
}

/// `<data_dir>/caddy/Caddyfile` — staged from the bundle, possibly patched.
pub fn stack_caddyfile() -> PathBuf {
    data_dir().join("caddy").join("Caddyfile")
}

/// `--project-directory` for every `docker compose` invocation.
pub fn stack_project_dir() -> PathBuf {
    data_dir()
}

/// Copy bundled compose.yml + Caddyfile into `<data_dir>/` and create the
/// subdirectories that compose bind-mounts. Idempotent — always overwrites.
///
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is rewritten from
/// `quip-node:80` (compose network alias, absent when node is on the host)
/// to `host.docker.internal:<native_rest_port>`. Docker mode uses the file
/// verbatim.
pub fn sync_stack_assets(
    app: &AppHandle,
    run_mode: &RunMode,
    native_rest_port: u16,
) -> Result<(), String> {
    let compose_src = app
        .path()
        .resolve("stack/docker-compose.yml", BaseDirectory::Resource)
        .map_err(|e| format!("resolve compose resource: {e}"))?;
    let caddy_src = app
        .path()
        .resolve("stack/caddy/Caddyfile", BaseDirectory::Resource)
        .map_err(|e| format!("resolve Caddyfile resource: {e}"))?;

    let base = data_dir();
    for sub in ["data", "dashboard-data", "caddy"] {
        fs::create_dir_all(base.join(sub))
            .map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    fs::copy(&compose_src, stack_compose_file())
        .map_err(|e| format!("copy docker-compose.yml: {e}"))?;

    let caddy_src_text = fs::read_to_string(&caddy_src)
        .map_err(|e| format!("read bundled Caddyfile: {e}"))?;
    let caddy_out = match run_mode {
        RunMode::Native => caddy_src_text.replace(
            "quip-node:80",
            &format!("host.docker.internal:{native_rest_port}"),
        ),
        RunMode::Docker => caddy_src_text,
    };
    fs::write(stack_caddyfile(), caddy_out)
        .map_err(|e| format!("write Caddyfile: {e}"))?;

    Ok(())
}
