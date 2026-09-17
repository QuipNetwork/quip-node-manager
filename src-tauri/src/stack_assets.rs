// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage the docker-compose stack files into the user's data dir so
//! `docker compose` can run with `--project-directory`.
//!
//! The compose.yml, Caddyfile, chain spec, miner config template,
//! and validator healthcheck are embedded into the binary at compile time via
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
//!   - compose.yml: Caddy's host-published public API port is rewritten
//!     from upstream 20049 to the user configured API port.
//!   - compose.yml: the validator libp2p host port is rewritten from
//!     upstream 30333 to the configured `validator_port` (default 30333,
//!     i.e. a no-op 1:1 mapping unless the user overrides it).
//!   - compose.yml: when `public_host` is set, the validator command gets
//!     a matching `--public-addr=<multiaddr>` using the public validator port.
//!   - Caddyfile: the optional local faucet route is stripped; the manager
//!     points the miner at the public faucet for its channel directly via
//!     `FAUCET_URL` in the rendered config.toml.
//!   - Caddyfile (Native mode only): `/api/v1/*` upstream is rewritten
//!     from `quip-miner:8086` (compose network alias, absent when the miner
//!     is on the host) to `host.docker.internal:<native_rest_port>`.
//!   - compose.yml: each enabled service port is published on its configured
//!     host port. Native RPC is mandatory so the host miner can reach it.
//!     Docker health probes run inside the stack without public mappings.

use crate::settings::{data_dir, NodeConfig, RunMode};
use std::fs;
use std::path::PathBuf;

/// Upstream compose.yml, embedded at compile time from the vendored
/// `nodes.quip.network` submodule. rustc's dep-info tracks the included
/// path so `cargo build` rebuilds whenever the file changes.
pub(crate) const COMPOSE_YML: &str =
    include_str!("../../vendor/nodes.quip.network/docker-compose.yml");

/// Upstream Caddyfile, embedded alongside the compose.yml. Patched at
/// runtime for Native mode (see `sync_stack_assets`).
const CADDYFILE: &str = include_str!("../../vendor/nodes.quip.network/caddy/Caddyfile");

/// Aglais chain spec — the live Quip test network (runtime spec 117). Replaces
/// the retired `quip-testnet.json`, which upstream deleted at the relaunch.
const CHAIN_SPEC: &str =
    include_str!("../../vendor/nodes.quip.network/chain-specs/aglais-network.json");

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
pub(crate) const MIN_VALIDATOR_TAG: &str = "v0.3.0-rc1";

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
/// The miner, dashboard, and faucet all wait on `service_healthy`, so a missing
/// or non-executable script leaves the whole stack permanently unstarted.
const VALIDATOR_HEALTHCHECK: &str =
    include_str!("../../vendor/nodes.quip.network/scripts/validator-healthcheck.sh");

/// syslog-ng configuration for the merged stack log. Every service forwards
/// stdout through the Docker syslog driver to the colocated collector, which
/// writes one host-readable file at `data/logs/quip-node.log`.
const SYSLOG_CONF: &str = include_str!("../../vendor/nodes.quip.network/syslog-ng/syslog-ng.conf");

/// The collector's PID 1. It supervises syslog-ng and rotates the merged log,
/// because syslog-ng OSE has no size-based rotation and the custom entrypoint
/// bypasses the image's own supervisor. Staged executable for the same reason
/// as the validator healthcheck: a non-executable entrypoint stops the
/// container, and every other service gates on it through `depends_on`.
const SYSLOG_ENTRYPOINT: &str =
    include_str!("../../vendor/nodes.quip.network/syslog-ng/entrypoint.sh");

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

/// `<data_dir>/caddy/Caddyfile` — staged from the embedded bytes, possibly
/// patched for Native mode.
pub fn stack_caddyfile() -> PathBuf {
    data_dir().join("caddy").join("Caddyfile")
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

/// Write the embedded compose.yml, Caddyfile, and chain spec into
/// `<data_dir>/`, and create the subdirectories compose bind-mounts.
/// Idempotent — always overwrites.
///
/// The port settings control only host publications. All internal ports stay fixed.
/// `config.public_host`, when set and P2P publishing is enabled, is converted
/// into a Substrate public multiaddr using `config.validator_port`.
///
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is also
/// rewritten from `quip-miner:8086` to `host.docker.internal:<rest_port>`.
pub fn sync_stack_assets(run_mode: &RunMode, config: &NodeConfig) -> Result<(), String> {
    crate::service_ports::validate(config, run_mode)?;
    let base = data_dir();
    for sub in [
        "data",
        // The validator's base path. Aglais kept the chain id `quip_testnet`,
        // so its database lands in the same `chains/quip_testnet` subdirectory
        // the retired network used; a separate host directory is what keeps an
        // Aglais node from opening the old network's database and failing with
        // a GRANDPA decode error. Created here rather than left to Docker,
        // which fabricates a missing bind-mount source owned by root while the
        // validator runs as PUID/PGID.
        "data/aglais-chain-db",
        "dashboard-data",
        "caddy",
        "chain-specs",
        "config",
        "scripts",
        "syslog-ng",
        // Destination of the merged stack log. Created here for the same
        // reason as the validator database above: Docker would fabricate the
        // missing bind-mount source owned by root, and the collector writes as
        // PUID/PGID, so it could not create quip-node.log inside it.
        "data/logs",
    ] {
        fs::create_dir_all(base.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    let compose_out = patch_compose_file(COMPOSE_YML, config, run_mode)?;
    fs::write(stack_compose_file(), compose_out)
        .map_err(|e| format!("write docker-compose.yml: {e}"))?;

    let caddy_out = patch_caddyfile(run_mode, CADDYFILE, crate::config::native_rest_port(config));
    let caddy_out = configure_caddy_admin(&caddy_out, config.service_ports.caddy_admin.enabled)?;
    fs::write(stack_caddyfile(), caddy_out).map_err(|e| format!("write Caddyfile: {e}"))?;

    fs::write(stack_chain_spec_file(), CHAIN_SPEC).map_err(|e| format!("write chain spec: {e}"))?;

    fs::write(stack_miner_config_file(), miner_config_template()?)
        .map_err(|e| format!("write miner config template: {e}"))?;

    write_healthcheck_script()?;
    write_syslog_assets()?;

    Ok(())
}

/// `<data_dir>/syslog-ng/syslog-ng.conf` — staged from the embedded bytes.
pub fn stack_syslog_conf_file() -> PathBuf {
    data_dir().join("syslog-ng").join("syslog-ng.conf")
}

/// `<data_dir>/syslog-ng/entrypoint.sh` — staged executable.
pub fn stack_syslog_entrypoint_file() -> PathBuf {
    data_dir().join("syslog-ng").join("entrypoint.sh")
}

/// `<data_dir>/data/logs/quip-node.log` — the merged stack log the collector
/// writes, and the single source the log pane tails. Nothing here creates it;
/// the collector does, on its first received line.
pub fn merged_log_file() -> PathBuf {
    data_dir().join("data").join("logs").join("quip-node.log")
}

/// Stage the collector's config and entrypoint.
///
/// Both are written with LF endings. The entrypoint is a shell script embedded
/// at compile time from `vendor/`, which is its own git repo and so is not
/// covered by this repo's `.gitattributes`; a Windows build machine with
/// `core.autocrlf=true` would bake in `set -eu\r`, which `/bin/sh` rejects, and
/// would turn the `LOG=/logs/quip-node.log` assignment into a path ending in a
/// carriage return. The container runs Linux whatever the host is.
fn write_syslog_assets() -> Result<(), String> {
    fs::write(stack_syslog_conf_file(), SYSLOG_CONF.replace("\r\n", "\n"))
        .map_err(|e| format!("write syslog-ng.conf: {e}"))?;

    let path = stack_syslog_entrypoint_file();
    fs::write(&path, SYSLOG_ENTRYPOINT.replace("\r\n", "\n"))
        .map_err(|e| format!("write syslog-ng entrypoint: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod syslog-ng entrypoint: {e}"))?;
    }

    Ok(())
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
) -> Result<String, String> {
    use crate::service_ports::{PortId, PORT_SPECS};
    crate::service_ports::validate(config, mode)?;
    let mut output = src.to_string();
    for (service, name) in [
        ("quip-validator", "quip-validator"),
        ("cpu", "quip-miner"),
        ("cuda", "quip-miner"),
        ("dashboard", "quip-dashboard"),
        ("postgres", "quip-postgres"),
        ("caddy", "quip-caddy"),
    ] {
        let mut mappings = Vec::new();
        for spec in PORT_SPECS.iter().filter(|spec| spec.service == name) {
            if spec.id == PortId::MinerRest && *mode == RunMode::Native {
                continue; // The miner binds directly on the host in Native mode.
            }
            let binding = spec.id.binding(config, mode);
            if binding.enabled || spec.id.required(mode) {
                let address = if spec.id == PortId::ValidatorRpc
                    && *mode == RunMode::Native
                    && !binding.enabled
                {
                    "127.0.0.1:"
                } else {
                    ""
                };
                for protocol in spec.protocols {
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

fn configure_caddy_admin(src: &str, enabled: bool) -> Result<String, String> {
    if !enabled {
        return Ok(src.to_string());
    }
    if !src.contains("\n{\n") {
        return Err("embedded Caddyfile has no global options block".into());
    }
    Ok(src.replacen("\n{\n", "\n{\n\tadmin :2019\n", 1))
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

fn patch_compose_file(src: &str, config: &NodeConfig, mode: &RunMode) -> Result<String, String> {
    let patched = apply_port_publications(src, config, mode)?;
    let public_host = if config.validator_p2p_enabled {
        &config.public_host
    } else {
        ""
    };
    let patched = patch_validator_public_addr(&patched, public_host, config.validator_port);
    let patched = ungate_validator_dependents(&patched);
    Ok(strip_volume_names(&patched))
}

/// Start the miner, dashboard, and faucet as soon as the validator's container
/// is up, rather than waiting for it to finish syncing.
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
/// Only the `quip-validator` dependencies are relaxed. The dashboard's
/// dependency on `postgres` is also `service_healthy`, but that check passes in
/// seconds and skipping it would race the database.
fn ungate_validator_dependents(src: &str) -> String {
    // Two indentation levels: the `x-dashboard` anchor and the services.
    src.replace(
        "    quip-validator:\n      condition: service_healthy",
        "    quip-validator:\n      condition: service_started",
    )
    .replace(
        "      quip-validator:\n        condition: service_healthy",
        "      quip-validator:\n        condition: service_started",
    )
}

/// Drop the fixed `name: quip-*` directives from the top-level `volumes:`
/// block so each volume is scoped to the compose project (`quip_<key>`) rather
/// than a global name. The upstream compose pins `name: quip-pgdata` (and the
/// caddy volumes), which makes them collide with any other Quip stack on the
/// same host — e.g. a developer running the raw `docker compose`. Sharing the
/// Postgres volume across stacks breaks the dashboard: `POSTGRES_PASSWORD` is
/// only applied when the data dir is first initialised, so a volume created by
/// one stack keeps its original password and authentication fails for the
/// other.
/// Upstream renamed the Postgres volume `quip-pgdata` -> `aglais-pgdata` at the
/// Aglais relaunch. These are literal matches, so an upstream rename silently
/// stops stripping and reintroduces the collision this exists to prevent;
/// `strip_volume_names_leaves_no_fixed_name` fails the build if that recurs.
fn strip_volume_names(src: &str) -> String {
    src.replace("\n    name: aglais-pgdata", "")
        .replace("\n    name: quip-pgdata", "")
        .replace("\n    name: quip-caddy-data", "")
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

fn patch_caddyfile(run_mode: &RunMode, src: &str, native_rest_port: u16) -> String {
    let src = strip_local_faucet_route(src);
    match run_mode {
        RunMode::Native => src.replace(
            "quip-miner:8086",
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

    /// The collector is the service every other service gates on through
    /// `depends_on`, so a CRLF entrypoint stops the whole stack rather than
    /// only the log. A carriage return would also land in the `LOG=` path and
    /// name the file with a trailing return.
    #[test]
    fn staged_syslog_assets_are_lf_whatever_the_build_machine_checked_out() {
        for embedded in [SYSLOG_ENTRYPOINT, SYSLOG_CONF] {
            let crlf = embedded.replace('\n', "\r\n");
            let staged = crlf.replace("\r\n", "\n");
            assert!(!staged.contains('\r'));
            assert_eq!(embedded.replace("\r\n", "\n"), staged);
        }
        assert!(SYSLOG_ENTRYPOINT
            .replace("\r\n", "\n")
            .contains("set -eu\n"));
        assert!(SYSLOG_ENTRYPOINT.contains("LOG=/logs/quip-node.log"));
    }

    /// The path the collector writes, the directory compose mounts, and the
    /// directory `sync_stack_assets` creates all have to name the same place,
    /// or the app stages a directory that nothing ever writes into.
    #[test]
    fn collector_writes_into_the_mounted_log_directory() {
        assert!(COMPOSE_YML.contains("- ./data/logs:/logs"));
        assert!(SYSLOG_CONF.contains("/logs/quip-node.log"));
        assert!(SYSLOG_ENTRYPOINT.contains("LOG=/logs/quip-node.log"));
    }

    /// The log pane depends on the collector preserving the message it relays.
    ///
    /// A bare `$(sanitize ${MESSAGE})` rewrites `/` and every control character
    /// to `_`. That mangles every URL the stack logs and collapses Caddy's
    /// tab-delimited console format, so a 502 renders as INFO. It degrades the
    /// pane silently — every line still arrives, just wrong — which is why this
    /// is pinned here, against the embedded config, rather than trusted to stay
    /// fixed upstream (nodes.quip.network!28).
    #[test]
    fn collector_template_keeps_slashes_and_tabs() {
        assert!(
            SYSLOG_CONF.contains(r"$(sanitize --no-ctrl-chars --invalid-chars '\n\r' ${MESSAGE})"),
            "the merged-log template lost its narrowed sanitize options"
        );
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
            apply_port_publications(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker).unwrap();
        assert!(!output.contains(":9944\""));
        assert!(output.contains("\"20049:20049/tcp\""));
    }

    #[test]
    fn native_rpc_is_required_and_keeps_the_container_port() {
        let config = NodeConfig {
            validator_rpc_port: 29944,
            ..NodeConfig::default()
        };
        let output = apply_port_publications(COMPOSE_YML, &config, &RunMode::Native).unwrap();
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
        let output = apply_port_publications(COMPOSE_YML, &config, &RunMode::Native).unwrap();
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
        let output = apply_port_publications(COMPOSE_YML, &config, &RunMode::Docker).unwrap();
        for expected in [
            "40000:30333/tcp",
            "40000:30333/udp",
            "40001:9944/tcp",
            "40002:9615/tcp",
            "40003:8086/tcp",
            "40004:3001/tcp",
            "40005:5432/tcp",
            "40006:20049/tcp",
            "40007:80/tcp",
            "40008:443/tcp",
            "40009:443/udp",
            "40010:8088/tcp",
            "40011:2019/tcp",
            "40012:20049/udp",
        ] {
            assert!(output.contains(expected), "missing mapping {expected}");
        }
        assert_eq!(output.matches("40003:8086/tcp").count(), 2, "CPU and CUDA");
        assert!(output.contains("@postgres:5432/"));
        assert!(output.contains("ws://quip-caddy:8088/rpc"));
        assert!(output.contains("--rpc-port=9944"));
    }

    #[test]
    fn all_host_publications_can_be_disabled_in_docker() {
        use crate::service_ports::PORT_SPECS;
        let mut config = NodeConfig::default();
        for spec in PORT_SPECS {
            let mut binding = spec.id.binding(&config, &RunMode::Docker);
            binding.enabled = false;
            spec.id.set_binding(&mut config, &RunMode::Docker, binding);
        }
        let output = apply_port_publications(COMPOSE_YML, &config, &RunMode::Docker).unwrap();
        assert!(!output.lines().any(|line| line.trim() == "ports:"));
        assert!(output.contains("--rpc-port=9944"));
    }

    #[test]
    fn admin_listener_is_reachable_only_when_requested() {
        assert_eq!(configure_caddy_admin(CADDYFILE, false).unwrap(), CADDYFILE);
        let enabled = configure_caddy_admin(CADDYFILE, true).unwrap();
        assert!(enabled.contains("admin :2019"));
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
        assert!(patched.contains("\n  pgdata:"));
        assert!(patched.contains("\n  caddy-data:"));
        assert!(patched.contains("\n  caddy-config:"));
    }

    #[test]
    fn strip_volume_names_leaves_container_names_untouched() {
        // `container_name:` is a different directive — it must survive so the
        // cleanup/reaping logic can still find containers by name.
        let patched = strip_volume_names(COMPOSE_YML);
        assert!(patched.contains("container_name: quip-postgres"));
        assert!(patched.contains("container_name: quip-dashboard"));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_dns_public_host() {
        let config = NodeConfig {
            validator_port: 30033,
            public_host: "node.example.com".into(),
            ..NodeConfig::default()
        };
        let patched = patch_compose_file(COMPOSE_YML, &config, &RunMode::Docker).unwrap();
        assert!(patched.contains("      - --public-addr=/dns4/node.example.com/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_ip_public_host() {
        let config = NodeConfig {
            validator_port: 30033,
            public_host: "1.2.3.4".into(),
            ..NodeConfig::default()
        };
        let patched = patch_compose_file(COMPOSE_YML, &config, &RunMode::Docker).unwrap();
        assert!(patched.contains("      - --public-addr=/ip4/1.2.3.4/tcp/30033\n"));

        let config = NodeConfig {
            validator_port: 30033,
            public_host: "[2001:db8::1]".into(),
            ..NodeConfig::default()
        };
        let patched = patch_compose_file(COMPOSE_YML, &config, &RunMode::Docker).unwrap();
        assert!(patched.contains("      - --public-addr=/ip6/2001:db8::1/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_omits_public_addr_when_public_host_is_empty() {
        let patched =
            patch_compose_file(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker).unwrap();
        assert!(!patched.contains("--public-addr"));
    }

    /// Upstream gates the miner, dashboard, and faucet on the validator being
    /// *synced*, which is hours on a fresh install. Only those three relax to
    /// `service_started`; the dashboard's postgres dependency must keep waiting
    /// for health, since that check passes in seconds and skipping it races the
    /// database.
    #[test]
    fn ungating_relaxes_only_the_validator_dependencies() {
        let patched =
            patch_compose_file(COMPOSE_YML, &NodeConfig::default(), &RunMode::Docker).unwrap();

        assert!(
            !patched.contains("quip-validator:\n        condition: service_healthy"),
            "a service still waits for the validator to finish syncing"
        );
        assert!(
            !patched.contains("quip-validator:\n      condition: service_healthy"),
            "the dashboard anchor still waits for the validator to finish syncing"
        );
        assert!(
            patched.contains("postgres:\n      condition: service_healthy"),
            "the dashboard must still wait for postgres to be healthy"
        );

        // Every validator dependency upstream declares is accounted for.
        assert_eq!(
            COMPOSE_YML.matches("condition: service_healthy").count(),
            patched.matches("condition: service_healthy").count()
                + patched.matches("condition: service_started").count()
                - COMPOSE_YML.matches("condition: service_started").count(),
        );

        // The healthcheck itself stays: it still reports sync progress.
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
        let staged = [
            "./data",
            "./data/aglais-chain-db",
            "./dashboard-data",
            "./caddy/Caddyfile",
            "./chain-specs/aglais-network.json",
            "./config/quip-miner.toml",
            "./scripts/validator-healthcheck.sh",
            "./syslog-ng/syslog-ng.conf",
            "./syslog-ng/entrypoint.sh",
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

    /// The Caddyfile upstream and the `[dashboard].listen` we render must name
    /// the same container port. They live in different repositories, so nothing
    /// but this test stops one from moving without the other and leaving
    /// `/api/v1/*` proxying into a closed port.
    #[test]
    fn caddy_miner_upstream_matches_the_rendered_dashboard_port() {
        assert!(CADDYFILE.contains(&format!(
            "reverse_proxy quip-miner:{}",
            crate::config::DOCKER_MINER_REST_PORT
        )));
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

    /// Caddy picks console vs JSON from whether stderr is a terminal, and under
    /// compose it never is. The Caddyfile pins console explicitly; if the
    /// submodule drops that block, every Caddy line reverts to a JSON blob that
    /// `log_stream::parse_log_line` cannot level-tag.
    #[test]
    fn caddyfile_pins_human_readable_console_logging() {
        assert!(
            CADDYFILE.contains("wrap console"),
            "Caddyfile must pin the console encoder"
        );
        assert!(
            patch_caddyfile(&RunMode::Native, CADDYFILE, 20100).contains("wrap console"),
            "patching must not drop the log block"
        );
    }

    #[test]
    fn docker_caddyfile_keeps_v02_routes_and_miner_upstream() {
        let patched = patch_caddyfile(&RunMode::Docker, CADDYFILE, 20100);
        assert!(patched.contains("handle /rpc"));
        assert!(patched.contains("handle /api/v1/*"));
        assert!(patched.contains("reverse_proxy quip-miner:8086"));
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
        assert!(!patched.contains("reverse_proxy quip-miner:8086"));
        assert!(!patched.contains("/api/faucet"));
        assert!(!patched.contains("quip-faucet"));
    }
}
