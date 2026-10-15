// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage the docker-compose stack files into the user's data dir so
//! `docker compose` can run with `--project-directory`.
//!
//! The compose.yml, chain spec, miner config template, and validator
//! healthcheck are embedded into the binary at compile time via
//! `include_str!`. That avoids Tauri's resource-bundler path entirely, so a raw
//! exe (e.g. Windows `--no-bundle` builds that ship just
//! `quip-node-manager.exe` with no sibling resource folder) still has the files
//! available at runtime.
//!
//! Everything the compose file bind-mounts must be staged here. Docker
//! fabricates an empty *directory* for a missing bind-mount source, so an
//! unstaged file does not fail loudly — it mounts a directory where a file was
//! expected. `every_relative_bind_mount_has_a_staged_source` guards that.
//!
//! Runtime patches are applied before writing:
//!   - compose.yml: each enabled service port is published on its configured
//!     host port. Native RPC and the log collector are mandatory: the host
//!     miner needs the RPC, and every service's Docker logging driver sends
//!     to the collector.
//!   - compose.yml: the validator libp2p host port is rewritten from
//!     upstream 30333 to the configured `validator_port` (default 30333,
//!     i.e. a no-op 1:1 mapping unless the user overrides it).
//!   - compose.yml: when `public_host` is set, the validator command gets
//!     a matching `--public-addr=<multiaddr>` using the public validator port.
//!   - compose.yml: the validator's dependents are ungated so they start as
//!     soon as its container is up, rather than waiting for a full sync.
//!   - compose.yml: `QUIP_MINER_UPSTREAM` is added for the dashboard's
//!     embedded Caddy, which upstream compose does not pass.
//!   - compose.yml: the fixed Caddy volume names are stripped so they scope
//!     to the compose project instead of colliding across stacks.

use crate::settings::{data_dir, NodeConfig, RunMode};
use std::fs;
use std::path::PathBuf;

/// Upstream compose.yml, embedded at compile time from the vendored
/// `nodes.quip.network` submodule. rustc's dep-info tracks the included
/// path so `cargo build` rebuilds whenever the file changes.
pub(crate) const COMPOSE_YML: &str =
    include_str!("../../vendor/nodes.quip.network/docker-compose.yml");

/// Aglais chain spec — the live Quip test network (runtime spec 119, relaunched
/// from a fresh genesis). Replaces the retired `quip-testnet.json`, which
/// upstream deleted at the first Aglais launch.
const CHAIN_SPEC: &str =
    include_str!("../../vendor/nodes.quip.network/chain-specs/aglais-network.json");

/// The validator's base path, relative to the data dir: one directory per
/// genesis. Every Aglais relaunch keeps the chain id `quip_testnet`, so a
/// reused directory would hand the old chain's database to the new spec.
/// Older directories stay in place for the operator to delete.
pub(crate) const VALIDATOR_DB_DIR: &str = "data/aglais-119-chain-db";

/// The dashboard's data directory, relative to the data dir: one directory per
/// genesis, because the dashboard index binds to the first genesis it sees.
pub(crate) const DASHBOARD_DATA_DIR: &str = "dashboard-data-119";

/// Oldest validator image that can run `CHAIN_SPEC`.
///
/// Aglais rotated the GRANDPA authorities to hybrid FN-DSA keys, which a
/// pre-Aglais node cannot decode: it opens the genesis authority set, fails on
/// `Public.0`, and reports `GRANDPA DB is corrupted` on a database it just
/// created. Nothing in the compose file selects a spec per channel — the
/// validator always mounts `CHAIN_SPEC` — so an image below this floor is
/// never a usable answer, on any channel.
///
/// Lives beside `CHAIN_SPEC` because the two move together: re-point the spec
/// at a new network and this floor is what stops the old images coming with it.
///
/// The spec-119 relaunch is a v0.4.0 chain spec export, so the floor is the
/// first validator built for that runtime.
pub(crate) const MIN_VALIDATOR_TAG: &str = "v0.4.0";

/// Oldest dashboard image that can run `COMPOSE_YML`.
///
/// The compose file has no Caddy or collector service. It expects both inside
/// the dashboard image, which first ships them in `v0.2.3-rc1`. It also mounts
/// the Caddy certificate volumes under the image's `/data`, and `v0.2.3-rc2` is
/// the first tag that serves only the public `/files` trees from there.
pub(crate) const MIN_DASHBOARD_TAG: &str = "v0.2.3-rc2";

/// Faucet for the network `CHAIN_SPEC` names.
///
/// The miner ships no built-in default: without `faucet_url` a fresh wallet
/// fails fast with `wallet-underfunded`, and a request to the wrong network's
/// faucet is quieter still — the miner keeps retrying while the balance stays
/// at zero. This tracks the embedded chain spec rather than the update
/// channel. The app embeds one spec, so both channels join one network and
/// share its faucet.
pub(crate) const FAUCET_URL: &str = "https://faucet.aglais.quip.network";

/// First-run miner config template. The compose file bind-mounts this over the
/// image's own `/app/config.toml`, which the entrypoint seeds `/data/config.toml`
/// from. An absent source makes Docker fabricate a directory in its place.
const MINER_CONFIG_TEMPLATE: &str =
    include_str!("../../vendor/nodes.quip.network/config/quip-miner.toml");

/// Validator sync gate, bind-mounted as the validator's healthcheck command.
/// The miner and the faucet wait on `service_healthy`, so a missing or
/// non-executable script leaves the whole stack permanently unstarted.
const VALIDATOR_HEALTHCHECK: &str =
    include_str!("../../vendor/nodes.quip.network/scripts/validator-healthcheck.sh");

/// `<data_dir>/docker-compose.yml` — staged from the embedded bytes.
pub fn stack_compose_file() -> PathBuf {
    data_dir().join("docker-compose.yml")
}

/// `<data_dir>/docker-compose.override.yml` — operator-owned, never written or
/// read by this app beyond checking that it exists.
///
/// `sync_stack_assets` rewrites `docker-compose.yml` from the embedded bytes on
/// every Start and Apply, so edits to the staged file do not survive. This is
/// the supported place to change the bundled stack: compose merges it over the
/// base file, and staging never touches it.
///
/// Compose auto-discovers this filename only when invoked with no `-f`. Every
/// invocation here passes `-f`, so `compose_cmd` must add it explicitly.
pub fn stack_override_file() -> PathBuf {
    data_dir().join("docker-compose.override.yml")
}

/// `<data_dir>/chain-specs/aglais-network.json` — staged from embedded bytes.
pub fn stack_chain_spec_file() -> PathBuf {
    data_dir().join("chain-specs").join("aglais-network.json")
}

/// `<data_dir>/config/quip-miner.toml` — staged from embedded bytes.
pub fn stack_miner_config_file() -> PathBuf {
    data_dir().join("config").join("quip-miner.toml")
}

/// `<data_dir>/scripts/validator-healthcheck.sh` — staged from embedded bytes,
/// written executable so the validator's `CMD` healthcheck can run it.
pub fn stack_validator_healthcheck_file() -> PathBuf {
    data_dir().join("scripts").join("validator-healthcheck.sh")
}

/// `--project-directory` for every `docker compose` invocation.
pub fn stack_project_dir() -> PathBuf {
    data_dir()
}

/// Write the embedded compose.yml and chain spec into `<data_dir>/`, and
/// create the subdirectories compose bind-mounts. Idempotent — always
/// overwrites.
///
/// The port settings control only host publications. All internal ports stay fixed.
/// `config.public_host`, when set and P2P publishing is enabled, is converted
/// into a Substrate public multiaddr using `config.validator_port`.
pub fn sync_stack_assets(run_mode: &RunMode, config: &NodeConfig, tls: bool) -> Result<(), String> {
    crate::service_ports::validate(config, run_mode, tls)?;
    let base = data_dir();
    for sub in [
        "data",
        // The validator's base path. Created here rather than left to Docker,
        // which fabricates a missing bind-mount source owned by root while the
        // validator runs as PUID/PGID.
        VALIDATOR_DB_DIR,
        DASHBOARD_DATA_DIR,
        "chain-specs",
        "config",
        "scripts",
        // Destination of the merged stack log. Created here for the same
        // reason as the validator database above: Docker would fabricate the
        // missing bind-mount source owned by root, and the collector writes as
        // PUID/PGID, so it could not create quip-node.log inside it.
        "data/logs",
    ] {
        fs::create_dir_all(base.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    // The four directories above that are actually bind-mounted for container
    // writes are created as the calling process, which is uid 0 when the
    // manager runs as root. `host_uid_gid` maps root to PUID/PGID 1000 (see
    // `runtime_ids`) so the containers never run as uid 0, so a root-run
    // manager would otherwise stage root-owned directories that PUID/PGID
    // cannot write into. `chown_staged_bind_mounts` closes that gap. It is a
    // no-op on Windows and for every non-root caller, which stages the
    // directories as the uid its own containers then run as.
    chown_staged_bind_mounts(
        &base,
        ["data", VALIDATOR_DB_DIR, DASHBOARD_DATA_DIR, "data/logs"],
    )?;

    let compose_out = patch_compose_file(COMPOSE_YML, config, run_mode, tls)?;
    fs::write(stack_compose_file(), compose_out)
        .map_err(|e| format!("write docker-compose.yml: {e}"))?;

    fs::write(stack_chain_spec_file(), CHAIN_SPEC).map_err(|e| format!("write chain spec: {e}"))?;

    fs::write(stack_miner_config_file(), miner_config_template()?)
        .map_err(|e| format!("write miner config template: {e}"))?;

    write_healthcheck_script()?;

    Ok(())
}

/// Chown the given staged bind-mount directories to the runtime PUID/PGID
/// when the manager runs as root. No-op on Windows, and no-op for every
/// non-root caller.
///
/// Only `data/logs` is chowned recursively. The container entrypoints chown
/// `/data` themselves, so the chain database underneath it needs no walk here
/// — and walking it would re-own hundreds of thousands of files on every
/// Start. Nothing chowns `/logs`, which is a separate mount point, so the
/// collector depends on this call.
#[cfg(unix)]
fn chown_staged_bind_mounts<const N: usize>(
    base: &std::path::Path,
    dirs: [&str; N],
) -> Result<(), String> {
    // Only root both needs this and is allowed to do it. A non-root caller
    // already owns what it staged, so the containers (which run as its own
    // uid) can write there. It also cannot chown to a group it does not
    // belong to: a macOS account sits in gid 20 while `runtime_ids` raises
    // PGID to 1000, so chowning would fail with EPERM and abort every Start.
    // SAFETY: geteuid takes no arguments and cannot fail per POSIX.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let (puid, pgid) = crate::compose::host_uid_gid();
    for sub in dirs {
        let path = base.join(sub);
        if sub == "data/logs" {
            chown_recursive(&path, puid, pgid)?;
        } else {
            chown_one(&path, puid, pgid)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_staged_bind_mounts<const N: usize>(
    _base: &std::path::Path,
    _dirs: [&str; N],
) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn chown_recursive(path: &std::path::Path, uid: u32, gid: u32) -> Result<(), String> {
    chown_one(path, uid, gid)?;
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| format!("read_dir {}: {e}", path.display()))? {
            let entry = entry.map_err(|e| format!("read_dir entry {}: {e}", path.display()))?;
            chown_recursive(&entry.path(), uid, gid)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn chown_one(path: &std::path::Path, uid: u32, gid: u32) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| format!("chown {}: not a valid C string: {e}", path.display()))?;
    // SAFETY: c_path is a valid NUL-terminated C string that outlives this call.
    let rc = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if rc != 0 {
        return Err(format!(
            "chown {} to {uid}:{gid}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// `<data_dir>/data/logs/quip-node.log` — the merged stack log the collector
/// writes, and the single source the log pane tails. Nothing here creates it;
/// the collector does, on its first received line.
pub fn merged_log_file() -> PathBuf {
    data_dir().join("data").join("logs").join("quip-node.log")
}

fn miner_config_template() -> Result<String, String> {
    let mut template: toml::Value = toml::from_str(MINER_CONFIG_TEMPLATE)
        .map_err(|e| format!("parse embedded miner template: {e}"))?;
    let miner = template
        .get_mut("miner")
        .and_then(toml::Value::as_table_mut)
        .ok_or("embedded miner template has no [miner] table")?;
    miner.insert(
        "validators".into(),
        toml::Value::Array(vec![toml::Value::String(
            crate::config::DOCKER_VALIDATOR_RPC.into(),
        )]),
    );
    toml::to_string_pretty(&template).map_err(|e| format!("render miner template: {e}"))
}

fn apply_port_publications(
    src: &str,
    config: &NodeConfig,
    mode: &RunMode,
    tls: bool,
) -> Result<String, String> {
    use crate::service_ports::{published_protocols, PortId, PORT_SPECS};
    crate::service_ports::validate(config, mode, tls)?;
    let mut output = src.to_string();
    for (service, name) in [
        ("quip-validator", "quip-validator"),
        ("cpu", "quip-miner"),
        ("cuda", "quip-miner"),
        ("dashboard", "quip-dashboard"),
    ] {
        let mut mappings = Vec::new();
        for spec in PORT_SPECS.iter().filter(|spec| spec.service == name) {
            if spec.id == PortId::MinerRest && *mode == RunMode::Native {
                continue; // The miner binds directly on the host in Native mode.
            }
            let binding = spec.id.binding(config, mode);
            if binding.enabled || spec.id.required(mode) {
                let address = if spec.id == PortId::LogCollector
                    || (spec.id == PortId::ValidatorRpc
                        && *mode == RunMode::Native
                        && !binding.enabled)
                {
                    "127.0.0.1:"
                } else {
                    ""
                };
                for protocol in published_protocols(spec, tls) {
                    mappings.push(format!(
                        "{address}{}:{}/{protocol}",
                        binding.host_port, spec.container_port
                    ));
                }
            }
        }
        output = replace_service_ports(&output, service, &mappings)?;
    }
    Ok(output)
}

/// Replace one service's complete ports block, preserving its other YAML fields.
/// Fail if the embedded service layout changes instead of silently omitting a mapping.
fn replace_service_ports(src: &str, service: &str, mappings: &[String]) -> Result<String, String> {
    let header = format!("  {service}:");
    let mut found = false;
    let mut in_service = false;
    let mut in_ports = false;
    let mut output = String::new();
    for line in src.lines() {
        if line == header {
            if found {
                return Err(format!("duplicate {service} service in embedded compose"));
            }
            found = true;
            in_service = true;
            output.push_str(line);
            output.push('\n');
            if !mappings.is_empty() {
                output.push_str("    ports:\n");
                for mapping in mappings {
                    output.push_str(&format!("      - \"{mapping}\"\n"));
                }
            }
            continue;
        }
        if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            if !line.starts_with("    ") {
                in_service = false;
            }
            if !line.starts_with("      ") {
                in_ports = false;
            }
        }
        if in_service && line == "    ports:" {
            in_ports = true;
            continue;
        }
        if !in_ports {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !found {
        return Err(format!("missing {service} service in embedded compose"));
    }
    Ok(output)
}

/// Stage the validator healthcheck, executable. Compose runs it as the
/// container's `CMD` healthcheck, and a non-executable file fails every probe,
/// which keeps the validator `unhealthy` forever and blocks the miner and the
/// dashboard behind their `service_healthy` conditions.
///
/// CRLF is the other way to fail every probe. The script is embedded at compile
/// time from `vendor/`, which is its own git repo and so is not covered by this
/// repo's `.gitattributes`; a Windows build machine with `core.autocrlf=true`
/// bakes in `set -euo pipefail\r`, which bash rejects before the script opens a
/// socket. The container runs Linux whatever the host is, so write LF.
fn write_healthcheck_script() -> Result<(), String> {
    let path = stack_validator_healthcheck_file();
    fs::write(&path, VALIDATOR_HEALTHCHECK.replace("\r\n", "\n"))
        .map_err(|e| format!("write validator healthcheck: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod validator healthcheck: {e}"))?;
    }

    Ok(())
}

fn patch_compose_file(
    src: &str,
    config: &NodeConfig,
    mode: &RunMode,
    tls: bool,
) -> Result<String, String> {
    let patched = apply_port_publications(src, config, mode, tls)?;
    let public_host = if config.validator_p2p_enabled {
        &config.public_host
    } else {
        ""
    };
    let patched = patch_validator_public_addr(&patched, public_host, config.validator_port);
    let patched = ungate_validator_dependents(&patched);
    let patched = link_dashboard_miner_upstream(&patched)?;
    Ok(strip_volume_names(&patched))
}

/// Pass Caddy's `/api/v1/*` upstream to the dashboard container.
///
/// The dashboard image's Caddyfile reads `QUIP_MINER_UPSTREAM`, but upstream
/// compose passes only the backend's `QUIP_MINER_REST_URL`. Native mode needs
/// both to name the host miner, so `.env` supplies the value.
fn link_dashboard_miner_upstream(src: &str) -> Result<String, String> {
    let anchor = "      QUIP_MINER_REST_URL: ${QUIP_MINER_REST_URL:-http://quip-miner:8086}\n";
    if !src.contains(anchor) {
        return Err("embedded compose has no dashboard QUIP_MINER_REST_URL entry".into());
    }
    Ok(src.replacen(
        anchor,
        &format!("{anchor}      QUIP_MINER_UPSTREAM: ${{QUIP_MINER_UPSTREAM:-quip-miner:8086}}\n"),
        1,
    ))
}

/// Start the miner and the faucet as soon as the validator's container is up,
/// rather than waiting for it to finish syncing.
///
/// Upstream gates them on `service_healthy`, where healthy means "synced to the
/// chain head", with a 24h `start_period` to match. That is correct for the
/// stack on its own — the miner's coordinator preflight reads the runtime at the
/// validator's best block and exits if it is still at genesis — but it leaves a
/// fresh install sitting with nothing but a validator for hours, which reads as
/// a hung app.
///
/// The healthcheck itself is left in place: it still reports sync progress, and
/// the container shows `health: starting` rather than `unhealthy` for the whole
/// `start_period`. The tradeoff is that the miner now restart-loops against a
/// syncing validator instead of waiting, which is visible in its logs.
///
/// The miner and the faucet are the only dependents of `quip-validator` in the
/// embedded compose. The dashboard has no `depends_on` at all.
fn ungate_validator_dependents(src: &str) -> String {
    src.replace(
        "      quip-validator:\n        condition: service_healthy",
        "      quip-validator:\n        condition: service_started",
    )
}

/// Drop the fixed `name: quip-caddy-*` directives from the top-level
/// `volumes:` block so each volume is scoped to the compose project
/// (`quip_<key>`) rather than a global name. The upstream compose pins these
/// names, which makes them collide with any other Quip stack on the same host
/// — e.g. a developer running the raw `docker compose`.
///
/// The named volumes that remain hold Caddy's ACME certificates and account
/// key, which the dashboard container mounts at `/data/caddy/{data,config}`.
fn strip_volume_names(src: &str) -> String {
    src.replace("\n    name: quip-caddy-data", "")
        .replace("\n    name: quip-caddy-config", "")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The collector inside the dashboard image writes `/logs/quip-node.log`.
    /// The mount and `merged_log_file` must name the same host directory.
    #[test]
    fn collector_writes_into_the_mounted_log_directory() {
        assert!(COMPOSE_YML.contains("- ./data/logs:/logs"));
        assert!(merged_log_file().ends_with("data/logs/quip-node.log"));
    }

    /// Dual logging is what keeps `docker compose logs` — and so the app's log
    /// panel — working under the syslog driver. Setting `cache-disabled` would
    /// take the panel dark with nothing else failing.
    #[test]
    fn compose_keeps_the_docker_log_cache_enabled() {
        assert!(COMPOSE_YML.contains("driver: syslog"));
        // The literal appears in a comment warning against it, so match the key.
        assert!(!COMPOSE_YML
            .lines()
            .any(|l| l.trim().starts_with("cache-disabled:")));
    }

    /// A CRLF healthcheck exits 2 on `set -euo pipefail` before it opens a
    /// socket, so the validator never goes healthy and the miner and dashboard
    /// wait behind `service_healthy` forever. The `vendor/` scripts live in a
    /// separate git repo, so this repo's `.gitattributes` cannot pin them —
    /// normalizing at the write is what keeps a Windows build working.
    #[test]
    fn staged_healthcheck_is_lf_whatever_the_build_machine_checked_out() {
        let crlf = VALIDATOR_HEALTHCHECK.replace('\n', "\r\n");
        assert!(crlf.contains("set -euo pipefail\r\n"));
        let staged = crlf.replace("\r\n", "\n");
        assert!(!staged.contains('\r'));
        assert!(staged.contains("set -euo pipefail\n"));
        // The embedded copy on a normal checkout is already LF and unchanged.
        assert_eq!(VALIDATOR_HEALTHCHECK.replace("\r\n", "\n"), staged);
    }

    #[test]
    fn staged_miner_template_has_no_container_loopback_fallback() {
        let template: toml::Value = toml::from_str(&miner_config_template().unwrap()).unwrap();
        assert_eq!(
            template["miner"]["validators"].as_array().unwrap(),
            &[toml::Value::String("ws://quip-validator:9944".into())]
        );
    }

    #[test]
    fn unpublished_docker_rpc_has_no_host_binding() {
        let output =
            apply_port_publications(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker, false)
                .unwrap();
        assert!(!output.contains(":9944\""));
        assert!(output.contains("\"20049:20049/tcp\""));
    }

    #[test]
    fn native_rpc_is_required_and_keeps_the_container_port() {
        let config = NodeConfig {
            validator_rpc_port: 29944,
            ..NodeConfig::default()
        };
        let output =
            apply_port_publications(COMPOSE_YML, &config, &RunMode::Native, false).unwrap();
        assert!(output.contains("\"127.0.0.1:29944:9944/tcp\""));
        assert!(!output.contains(":8086/tcp\""));
    }

    #[test]
    fn native_rpc_public_access_requires_an_explicit_choice() {
        let config = NodeConfig {
            validator_rpc_enabled: true,
            validator_rpc_port: 29944,
            ..NodeConfig::default()
        };
        let output =
            apply_port_publications(COMPOSE_YML, &config, &RunMode::Native, false).unwrap();
        assert!(output.contains("\"29944:9944/tcp\""));
        assert!(!output.contains("0.0.0.0:"));
        assert!(!output.contains("127.0.0.1:29944"));
    }

    #[test]
    fn every_service_port_can_be_published_and_remapped() {
        use crate::service_ports::{PortBinding, PORT_SPECS};
        let mut config = NodeConfig::default();
        for (index, spec) in PORT_SPECS.iter().enumerate() {
            spec.id.set_binding(
                &mut config,
                &RunMode::Docker,
                PortBinding {
                    enabled: true,
                    host_port: 40000 + index as u16,
                },
            );
        }
        let output =
            apply_port_publications(COMPOSE_YML, &config, &RunMode::Docker, false).unwrap();
        for expected in [
            "40000:30333/tcp",
            "40000:30333/udp",
            "40001:9944/tcp",
            "40002:9615/tcp",
            "40003:8086/tcp",
            "40004:20049/tcp",
            "40005:80/tcp",
            "40006:443/tcp",
            "40007:8088/tcp",
            "127.0.0.1:40008:5514/udp",
        ] {
            assert!(output.contains(expected), "missing mapping {expected}");
        }
        // No TLS, so neither site publishes a QUIC listener.
        assert!(!output.contains("40004:20049/udp"));
        assert!(!output.contains("40006:443/udp"));
        assert_eq!(output.matches("40003:8086/tcp").count(), 2, "CPU and CUDA");
        assert!(output.contains("--rpc-port=9944"));
    }

    /// HTTP/3 has no row of its own. Turning TLS on publishes the UDP leg of
    /// each TLS site on that site's own host port, and turning it off removes
    /// both. Nothing else in the mapping set moves.
    #[test]
    fn tls_publishes_http3_on_the_tls_sites_own_ports() {
        use crate::service_ports::{PortBinding, PORT_SPECS};
        let mut config = NodeConfig::default();
        for (index, spec) in PORT_SPECS.iter().enumerate() {
            spec.id.set_binding(
                &mut config,
                &RunMode::Docker,
                PortBinding {
                    enabled: true,
                    host_port: 40000 + index as u16,
                },
            );
        }
        let output = apply_port_publications(COMPOSE_YML, &config, &RunMode::Docker, true).unwrap();
        // The Public API and HTTPS rows gain a UDP leg on their own port.
        assert!(output.contains("40004:20049/udp"));
        assert!(output.contains("40006:443/udp"));
        // Plaintext HTTP and the internal route stay TCP only.
        assert!(!output.contains("40005:80/udp"));
        assert!(!output.contains("40007:8088/udp"));
    }

    #[test]
    fn only_the_log_collector_stays_published_when_everything_is_off() {
        use crate::service_ports::PORT_SPECS;
        let mut config = NodeConfig::default();
        for spec in PORT_SPECS {
            let mut binding = spec.id.binding(&config, &RunMode::Docker);
            binding.enabled = false;
            spec.id.set_binding(&mut config, &RunMode::Docker, binding);
        }
        let output =
            apply_port_publications(COMPOSE_YML, &config, &RunMode::Docker, false).unwrap();
        assert_eq!(output.lines().filter(|l| l.trim() == "ports:").count(), 1);
        assert!(output.contains("\"127.0.0.1:5514:5514/udp\""));
        assert!(output.contains("--rpc-port=9944"));
    }

    /// The Docker logging driver sends to the host port in `QUIP_LOG_PORT`.
    /// The collector mapping must stay on loopback so nothing off-box can
    /// inject lines into the operator's log.
    #[test]
    fn log_collector_is_always_published_on_loopback() {
        let mut config = NodeConfig::default();
        config.service_ports.log_collector.host_port = 15514;
        for mode in [RunMode::Docker, RunMode::Native] {
            let output = apply_port_publications(COMPOSE_YML, &config, &mode, false).unwrap();
            assert!(output.contains("\"127.0.0.1:15514:5514/udp\""), "{mode:?}");
        }
        assert!(COMPOSE_YML.contains("udp://127.0.0.1:${QUIP_LOG_PORT:-5514}"));
    }

    /// Asserts on the *shape* rather than on the three names we happen to know:
    /// upstream renamed `quip-pgdata` to `aglais-pgdata` at the Aglais
    /// relaunch, and a literal-list test kept passing while the renamed volume
    /// went unstripped. Any `name:` under `volumes:` fails this.
    #[test]
    fn strip_volume_names_leaves_no_fixed_name() {
        let patched = strip_volume_names(COMPOSE_YML);
        let volumes = patched
            .split_once("\nvolumes:")
            .expect("compose has a top-level volumes block")
            .1;
        let leftover: Vec<&str> = volumes
            .lines()
            .filter(|l| l.trim_start().starts_with("name:"))
            .collect();
        assert!(
            leftover.is_empty(),
            "these volumes keep a fixed global name, so they collide with any \
             other Quip stack on the host: {leftover:?}"
        );
        // The volume keys themselves are preserved.
        assert!(patched.contains("\n  caddy-data:"));
        assert!(patched.contains("\n  caddy-config:"));
    }

    #[test]
    fn strip_volume_names_leaves_container_names_untouched() {
        // `container_name:` is a different directive — it must survive so the
        // cleanup/reaping logic can still find containers by name.
        let patched = strip_volume_names(COMPOSE_YML);
        assert!(patched.contains("container_name: quip-validator"));
        assert!(patched.contains("container_name: quip-dashboard"));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_dns_public_host() {
        let config = NodeConfig {
            validator_port: 30033,
            public_host: "node.example.com".into(),
            ..NodeConfig::default()
        };
        let patched = patch_compose_file(COMPOSE_YML, &config, &RunMode::Docker, false).unwrap();
        assert!(patched.contains("      - --public-addr=/dns4/node.example.com/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_ip_public_host() {
        let config = NodeConfig {
            validator_port: 30033,
            public_host: "1.2.3.4".into(),
            ..NodeConfig::default()
        };
        let patched = patch_compose_file(COMPOSE_YML, &config, &RunMode::Docker, false).unwrap();
        assert!(patched.contains("      - --public-addr=/ip4/1.2.3.4/tcp/30033\n"));

        let config = NodeConfig {
            validator_port: 30033,
            public_host: "[2001:db8::1]".into(),
            ..NodeConfig::default()
        };
        let patched = patch_compose_file(COMPOSE_YML, &config, &RunMode::Docker, false).unwrap();
        assert!(patched.contains("      - --public-addr=/ip6/2001:db8::1/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_omits_public_addr_when_public_host_is_empty() {
        let patched =
            patch_compose_file(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker, false)
                .unwrap();
        assert!(!patched.contains("--public-addr"));
    }

    /// Upstream gates the miner and the faucet on the validator being synced,
    /// which takes hours on a fresh install. The dashboard has no dependency.
    #[test]
    fn ungating_relaxes_every_validator_dependency() {
        let patched =
            patch_compose_file(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker, false)
                .unwrap();
        assert!(!patched.contains("quip-validator:\n        condition: service_healthy"));
        assert_eq!(
            COMPOSE_YML.matches("condition: service_healthy").count(),
            patched.matches("condition: service_started").count()
                - COMPOSE_YML.matches("condition: service_started").count(),
        );
        assert!(patched.contains("test: [\"CMD\", \"validator-healthcheck\"]"));
    }

    #[test]
    fn embedded_chain_spec_is_the_aglais_network() {
        assert!(CHAIN_SPEC.contains("\"name\": \"AGLS (Quip Testnet)\""));
        assert!(CHAIN_SPEC.contains("\"bootNodes\""));
    }

    /// Every `./`-relative bind mount in the compose file needs a real source
    /// staged under the data dir. Docker fabricates an empty *directory* for a
    /// missing source, which is how a new upstream mount turns into a silent
    /// runtime failure — the validator healthcheck mount is load-bearing, and
    /// the miner, dashboard, and faucet all gate on it being healthy.
    #[test]
    fn every_relative_bind_mount_has_a_staged_source() {
        let validator_db = format!("./{VALIDATOR_DB_DIR}");
        let dashboard_data = format!("./{DASHBOARD_DATA_DIR}");
        let staged = [
            "./data",
            validator_db.as_str(),
            dashboard_data.as_str(),
            "./chain-specs/aglais-network.json",
            "./config/quip-miner.toml",
            "./scripts/validator-healthcheck.sh",
            "./data/logs",
        ];
        for line in COMPOSE_YML.lines() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("- ./") else {
                continue;
            };
            let src = format!("./{}", rest.split(':').next().unwrap_or_default());
            assert!(
                staged.contains(&src.as_str()),
                "compose bind-mounts {src}, which sync_stack_assets does not stage"
            );
        }
    }

    /// Upstream reintroduced a miner config template mount at the Aglais
    /// relaunch, because the image's own `/app/config.toml` still names the
    /// retired testnet faucet. The mount is only safe while we stage the
    /// source; the previous assertion here (that no such mount existed) named
    /// the older `quip-miner.docker.toml` and so kept passing through the
    /// rename without noticing.
    #[test]
    fn miner_config_template_mount_is_staged() {
        assert!(
            COMPOSE_YML.contains("./config/quip-miner.toml:/app/config.toml"),
            "upstream dropped the miner config mount — stop staging it"
        );
        assert!(
            MINER_CONFIG_TEMPLATE.contains("[dashboard]"),
            "staged template should be the miner config, not an empty file"
        );
    }

    /// Caddy in the dashboard image reads its `/api/v1/*` upstream from
    /// `QUIP_MINER_UPSTREAM`. Upstream compose does not pass it, so the patch
    /// adds it. The default must keep Docker mode on the miner container.
    #[test]
    fn dashboard_receives_the_caddy_miner_upstream() {
        let patched =
            patch_compose_file(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker, false)
                .unwrap();
        assert_eq!(
            patched
                .matches("      QUIP_MINER_UPSTREAM: ${QUIP_MINER_UPSTREAM:-quip-miner:8086}\n")
                .count(),
            1
        );
    }

    /// The dashboard backend dials the miner REST listener directly. The
    /// compose default and the `[dashboard].listen` port that `config.rs`
    /// renders must name the same container port.
    #[test]
    fn compose_miner_rest_default_matches_the_rendered_dashboard_port() {
        assert!(COMPOSE_YML.contains(&format!(
            "QUIP_MINER_REST_URL: ${{QUIP_MINER_REST_URL:-http://quip-miner:{}}}",
            crate::config::DOCKER_MINER_REST_PORT
        )));
    }

    /// The front door and the collector moved into the dashboard container.
    #[test]
    fn embedded_compose_has_no_standalone_front_door_or_database() {
        for gone in ["\n  caddy:", "\n  postgres:", "\n  quip-syslog:"] {
            assert!(!COMPOSE_YML.contains(gone), "{gone}");
        }
    }

    /// The compose file expects Caddy and the collector inside the dashboard
    /// image, and mounts certificate volumes under its `/data`. Only tags at or
    /// above the floor ship both and serve just the public `/files` trees.
    #[test]
    fn dashboard_floor_admits_only_embedded_service_images() {
        use crate::update::parse_semver;
        let floor = parse_semver(MIN_DASHBOARD_TAG);
        assert!(parse_semver("v0.2.2") < floor);
        assert!(parse_semver("v0.2.3-rc1") < floor);
        assert!(parse_semver("v0.2.3-rc2") >= floor);
        assert!(parse_semver("v0.2.3") >= floor);
    }

    /// The chown step is the fix for the root-PUID bind-mount ownership gap:
    /// a root-run manager stages directories as uid 0 while the containers
    /// run as PUID/PGID 1000+ (`runtime_ids`), so those directories must be
    /// re-owned or the collector cannot write `data/logs/quip-node.log`.
    ///
    /// Both branches matter, so this asserts whichever one the caller is in.
    /// As root (the CI container), the staged directories must come back
    /// owned by the runtime ids. As any other user, the call must leave
    /// ownership alone: a macOS account sits in gid 20 while `runtime_ids`
    /// raises PGID to 1000, and chowning to a group the caller is not in
    /// fails with EPERM, which aborted every Start.
    #[cfg(unix)]
    #[test]
    fn chown_staged_bind_mounts_is_a_noop_for_a_non_root_caller() {
        use std::os::unix::fs::MetadataExt;
        let dir = unique_temp_dir("chown-noop");
        for sub in ["data", VALIDATOR_DB_DIR, DASHBOARD_DATA_DIR, "data/logs"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }

        // SAFETY: geteuid takes no arguments and cannot fail per POSIX.
        let root = unsafe { libc::geteuid() } == 0;
        let before = fs::metadata(dir.join("data/logs")).unwrap();

        chown_staged_bind_mounts(
            &dir,
            ["data", VALIDATOR_DB_DIR, DASHBOARD_DATA_DIR, "data/logs"],
        )
        .unwrap();

        let after = fs::metadata(dir.join("data/logs")).unwrap();
        if root {
            let (puid, pgid) = crate::compose::host_uid_gid();
            assert_eq!(after.uid(), puid, "root must hand the logs dir to PUID");
            assert_eq!(after.gid(), pgid, "root must hand the logs dir to PGID");
            assert_ne!(after.uid(), 0, "the collector never runs as uid 0");
        } else {
            assert_eq!(before.uid(), after.uid());
            assert_eq!(before.gid(), after.gid());
        }

        for sub in ["data", VALIDATOR_DB_DIR, DASHBOARD_DATA_DIR, "data/logs"] {
            assert!(dir.join(sub).is_dir(), "{sub} missing after chown step");
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nonce: u64 = rand::random();
        std::env::temp_dir().join(format!("quip-stack-assets-test-{label}-{nonce}"))
    }
}
