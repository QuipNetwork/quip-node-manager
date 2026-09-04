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
//!     `UpdateChannel::faucet_url` in the rendered config.toml.
//!   - Caddyfile (Native mode only): `/api/v1/*` upstream is rewritten
//!     from `quip-miner:8086` (compose network alias, absent when the miner
//!     is on the host) to `host.docker.internal:<native_rest_port>`.
//!   - compose.yml (both modes): the validator's JSON-RPC port is published
//!     on the host loopback (127.0.0.1:9944) so the host health monitor and
//!     the host-side miner (Native) can reach `ws://127.0.0.1:9944` directly.

use crate::settings::{data_dir, RunMode};
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

/// Public API port inside the Caddy container. The host side is configurable.
const CONTAINER_PUBLIC_API_PORT: u16 = 20049;
/// Validator libp2p port inside the validator container. The host side
/// defaults to the same 30333 and is also used for generated
/// `--public-addr` values.
const CONTAINER_VALIDATOR_PORT: u16 = 30333;
/// Validator JSON-RPC port inside the container. In Native mode it's published
/// on the host loopback (on a configurable host port, default 9944) so the
/// host-side miner can connect directly rather than via Caddy's `/rpc` route.
const CONTAINER_VALIDATOR_RPC_PORT: u16 = 9944;

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
/// `public_api_port` replaces Caddy's upstream host-side `20049`.
/// `validator_port` replaces the validator's upstream host-side `30333`
/// while preserving the container-internal `30333`.
/// `public_host`, when set, is converted into a Substrate public multiaddr
/// using `validator_port`.
///
/// In Native mode the Caddyfile's upstream for `/api/v1/*` is also
/// rewritten from `quip-miner:8086` to `host.docker.internal:<rest_port>`.
pub fn sync_stack_assets(
    run_mode: &RunMode,
    public_api_port: u16,
    validator_port: u16,
    public_host: &str,
    native_rest_port: u16,
    validator_rpc_port: u16,
) -> Result<(), String> {
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
    ] {
        fs::create_dir_all(base.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
    }

    let compose_out = patch_compose_file(
        COMPOSE_YML,
        public_api_port,
        validator_port,
        public_host,
        validator_rpc_port,
    );
    fs::write(stack_compose_file(), compose_out)
        .map_err(|e| format!("write docker-compose.yml: {e}"))?;

    let caddy_out = patch_caddyfile(run_mode, CADDYFILE, native_rest_port);
    fs::write(stack_caddyfile(), caddy_out).map_err(|e| format!("write Caddyfile: {e}"))?;

    fs::write(stack_chain_spec_file(), CHAIN_SPEC).map_err(|e| format!("write chain spec: {e}"))?;

    fs::write(stack_miner_config_file(), MINER_CONFIG_TEMPLATE)
        .map_err(|e| format!("write miner config template: {e}"))?;

    write_healthcheck_script()?;

    Ok(())
}

/// Stage the validator healthcheck, executable. Compose runs it as the
/// container's `CMD` healthcheck, and a non-executable file fails every probe,
/// which keeps the validator `unhealthy` forever and blocks the miner and the
/// dashboard behind their `service_healthy` conditions.
fn write_healthcheck_script() -> Result<(), String> {
    let path = stack_validator_healthcheck_file();
    fs::write(&path, VALIDATOR_HEALTHCHECK)
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
    public_api_port: u16,
    validator_port: u16,
    public_host: &str,
    validator_rpc_port: u16,
) -> String {
    let patched = patch_compose_ports(src, public_api_port, validator_port);
    let patched = expose_validator_rpc(&patched, validator_port, validator_rpc_port);
    let patched = patch_validator_public_addr(&patched, public_host, validator_port);
    let patched = ungate_validator_dependents(&patched);
    strip_volume_names(&patched)
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

/// Publish the validator's JSON-RPC port on the host loopback
/// (`127.0.0.1:<validator_rpc_port>`) in both run modes so the host health
/// monitor can reach `ws://127.0.0.1:<validator_rpc_port>` directly.
/// In Native mode the host-side miner also uses this binding.
fn expose_validator_rpc(src: &str, validator_port: u16, validator_rpc_port: u16) -> String {
    // Anchor on the (already port-patched) validator UDP mapping so the RPC
    // mapping lands inside the quip-validator service's `ports:` list.
    let udp_line = format!("      - \"{validator_port}:{CONTAINER_VALIDATOR_PORT}/udp\"\n");
    let rpc_line =
        format!("      - \"127.0.0.1:{validator_rpc_port}:{CONTAINER_VALIDATOR_RPC_PORT}\"\n");
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
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "node.example.com",
            9944,
        );
        assert!(patched.contains("      - --public-addr=/dns4/node.example.com/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_adds_public_addr_from_ip_public_host() {
        let patched = patch_compose_file(
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "1.2.3.4",
            9944,
        );
        assert!(patched.contains("      - --public-addr=/ip4/1.2.3.4/tcp/30033\n"));

        let patched = patch_compose_file(
            COMPOSE_YML,
            CONTAINER_PUBLIC_API_PORT,
            30033,
            "[2001:db8::1]",
            9944,
        );
        assert!(patched.contains("      - --public-addr=/ip6/2001:db8::1/tcp/30033\n"));
    }

    #[test]
    fn patch_compose_file_omits_public_addr_when_public_host_is_empty() {
        let patched = patch_compose_file(COMPOSE_YML, CONTAINER_PUBLIC_API_PORT, 30033, "", 9944);
        assert!(!patched.contains("--public-addr"));
    }

    #[test]
    fn both_modes_publish_validator_rpc_on_configured_host_port() {
        let patched = patch_compose_file(COMPOSE_YML, CONTAINER_PUBLIC_API_PORT, 30033, "", 9944);
        assert!(patched.contains("      - \"127.0.0.1:9944:9944\"\n"));
        // Inserted right after the validator's UDP mapping, inside its ports.
        assert!(patched.contains("\"30033:30333/udp\"\n      - \"127.0.0.1:9944:9944\""));

        // The host side honours the configured port; the container side is
        // always the validator's fixed 9944.
        let custom = patch_compose_file(COMPOSE_YML, CONTAINER_PUBLIC_API_PORT, 30033, "", 9955);
        assert!(custom.contains("      - \"127.0.0.1:9955:9944\"\n"));
    }

    #[test]
    fn docker_mode_also_publishes_validator_rpc_to_host() {
        let out = expose_validator_rpc(COMPOSE_YML, 30333, 9944);
        assert!(
            out.contains("127.0.0.1:9944:9944"),
            "Docker mode must publish validator RPC to host loopback for the health monitor"
        );
    }

    /// Upstream gates the miner, dashboard, and faucet on the validator being
    /// *synced*, which is hours on a fresh install. Only those three relax to
    /// `service_started`; the dashboard's postgres dependency must keep waiting
    /// for health, since that check passes in seconds and skipping it races the
    /// database.
    #[test]
    fn ungating_relaxes_only_the_validator_dependencies() {
        let patched = ungate_validator_dependents(COMPOSE_YML);

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
