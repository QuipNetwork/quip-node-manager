// SPDX-License-Identifier: AGPL-3.0-or-later
//! `docker compose`-based stack orchestration.
//!
//! Docker mode drives the full v0.2 stack (miner + validator + dashboard +
//! postgres + caddy). Native miner installation is deferred; the remaining
//! native path starts the Docker-side validator/dashboard support services.

use crate::log_stream::LogStreamState;
use crate::settings::{
    AppSettings, ImageTag, NodeConfig, RunMode, ServiceStatus, StackHealth, StackStatus,
};
use crate::stack_assets::{
    stack_caddyfile, stack_compose_file, stack_project_dir, sync_stack_assets,
};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

// ── logging helpers (moved verbatim from docker.rs) ────────────────────────

fn log_cmd(app: &AppHandle, cmd: &str) {
    let entry = serde_json::json!({
        "timestamp": "",
        "level": "INFO",
        "message": format!("$ {}", cmd),
    });
    let _ = app.emit("node-log", entry);
}

fn log_output(app: &AppHandle, text: &str) {
    for line in text.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

fn log_err(app: &AppHandle, text: &str) {
    for line in text.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "ERROR",
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

// ── host uid/gid (moved verbatim from docker.rs) ───────────────────────────

/// Host uid/gid for the PUID/PGID env vars passed to containers that need
/// to chown bind-mounted `/data` to the host user.
///
/// Gids below 1000 are clamped up to 1000 — macOS users default to gid 20
/// (staff), which collides with Alpine's `games` group inside the node
/// image and breaks the entrypoint's `groupmod`. Keeping the real uid
/// preserves host-side ownership; the gid just won't have a friendly name.
pub(crate) fn host_uid_gid() -> (u32, u32) {
    #[cfg(unix)]
    {
        // SAFETY: getuid/getgid take no arguments and cannot fail per POSIX.
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        (uid, gid.max(1000))
    }
    #[cfg(not(unix))]
    {
        (1000, 1000)
    }
}

// ── profile + services selection ───────────────────────────────────────────

/// Compose profile name for v0.2. The standard upstream profiles always
/// include dashboard, postgres, Caddy, the selected miner, bootstrap, and
/// validator.
pub fn compose_profile(image_tag: ImageTag) -> &'static str {
    match image_tag {
        ImageTag::Cpu => "cpu",
        ImageTag::Cuda => "cuda",
    }
}

/// Explicit service list for `docker compose up -d [services...]`.
///
/// - Docker mode: empty slice means "start every service the profile allows"
///   (compose default). We don't enumerate because the profile already gates
///   things down to the correct set.
/// - Native mode: we skip the miner and bootstrap containers and hand compose
///   an explicit list of support services. The profile is still set so these
///   services are eligible, while positional args restrict startup to them.
pub fn compose_services(run_mode: &RunMode, _tls_enabled: bool) -> &'static [&'static str] {
    match run_mode {
        RunMode::Docker => &[],
        RunMode::Native => &["quip-validator", "dashboard", "postgres", "caddy"],
    }
}

// ── Native REST port ───────────────────────────────────────────────────────

/// Host REST port for the deferred native miner path. Docker miners bind
/// internal port 80 and are reached through Caddy's `/api/v1/*` route.
pub fn native_rest_port(cfg: &NodeConfig) -> u16 {
    if cfg.rest_insecure_port > 0 {
        cfg.rest_insecure_port as u16
    } else {
        20100
    }
}

// ── compose command builder ────────────────────────────────────────────────

fn to_forward_slash(p: PathBuf) -> String {
    // Docker Desktop on Windows is happier with forward slashes as
    // `--project-directory`; it accepts them everywhere else too.
    p.to_string_lossy().replace('\\', "/")
}

/// `docker compose -f <data_dir>/docker-compose.yml --project-directory
/// <data_dir> --project-name quip` — the common prefix for every compose
/// invocation.
pub(crate) fn compose_cmd() -> Command {
    let compose_file = to_forward_slash(stack_compose_file());
    let project_dir = to_forward_slash(stack_project_dir());
    let mut c = crate::cmd::new("docker");
    c.args([
        "compose",
        "-f",
        &compose_file,
        "--project-directory",
        &project_dir,
        "--project-name",
        "quip",
    ]);
    c
}

// ── .env generation ────────────────────────────────────────────────────────

/// Write `<data_dir>/.env` from AppSettings. Overwritten on every start —
/// there is no merge with an existing file.
pub(crate) fn write_env_file(settings: &AppSettings) -> Result<(), String> {
    let (puid, pgid) = host_uid_gid();
    let pg_password = crate::settings::postgres_password();
    let lines = render_env_lines(settings, puid, pgid, &pg_password);

    let path = stack_project_dir().join(".env");
    fs::write(&path, lines.join("\n") + "\n").map_err(|e| format!("write .env: {e}"))?;

    // Best-effort 0600: DWAVE_API_KEY and POSTGRES_PASSWORD shouldn't be
    // world-readable on shared systems.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

fn cpu_set_for_config(cfg: &NodeConfig) -> String {
    match cfg.num_cpus {
        0 | 1 => "0".to_string(),
        n => format!("0-{}", n - 1),
    }
}

fn render_env_lines(
    settings: &AppSettings,
    puid: u32,
    pgid: u32,
    pg_password: &str,
) -> Vec<String> {
    let dwave_key = settings
        .node_config
        .dwave_config
        .as_ref()
        .map(|d| d.token.clone())
        .unwrap_or_default();
    let hostname = crate::hostnames::resolved_caddy_hostname(
        &settings.node_config.public_host,
        &settings.hostname,
    );
    let validator_name = if settings.node_config.node_name.trim().is_empty() {
        "quip-validator"
    } else {
        settings.node_config.node_name.trim()
    };

    let mut lines = vec![
        format!("PUID={puid}"),
        format!("PGID={pgid}"),
        format!("QUIP_HOSTNAME={hostname}"),
        format!("CERT_EMAIL={}", settings.cert_email),
        format!("ZEROSSL_API_KEY={}", settings.zerossl_api_key),
        format!("DWAVE_API_KEY={dwave_key}"),
        format!("POSTGRES_PASSWORD={pg_password}"),
        format!("QUIP_MINER_TAG={COMPOSE_IMAGE_TAG}"),
        format!("QUIP_DASHBOARD_TAG={COMPOSE_IMAGE_TAG}"),
        format!("QUIP_VALIDATOR_TAG={COMPOSE_IMAGE_TAG}"),
        format!(
            "QUIP_MINER_CPUSET={}",
            cpu_set_for_config(&settings.node_config)
        ),
        format!("VALIDATOR_NAME={validator_name}"),
    ];

    if settings.run_mode == RunMode::Docker {
        lines.push("QUIP_VALIDATORS=ws://quip-validator:9944".to_string());
    }
    lines.push("QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944".to_string());

    lines
}

// ── streaming compose output ───────────────────────────────────────────────

/// Default timeout for long-running compose ops (pull, up). Compose itself
/// respects context timeouts; this is a backstop against a wedged daemon.
const COMPOSE_LONG_TIMEOUT: Duration = Duration::from_secs(600);

async fn run_compose_streaming(app: &AppHandle, args: Vec<String>) -> Result<(), String> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        let mut child = compose_cmd()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn docker compose: {e}"))?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let app_out = app.clone();
        let stdout_thread = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = app_out.emit("pull-progress", serde_json::json!({ "line": &line }));
                let _ = app_out.emit(
                    "node-log",
                    serde_json::json!({
                        "timestamp": "",
                        "level": "INFO",
                        "message": &line,
                    }),
                );
            }
        });

        let app_err = app.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut last = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = app_err.emit(
                    "node-log",
                    serde_json::json!({
                        "timestamp": "",
                        "level": "INFO",
                        "message": &line,
                    }),
                );
                last = line;
            }
            last
        });

        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let _ = stdout_thread.join();
                    let stderr_tail = stderr_thread.join().unwrap_or_default();
                    return if status.success() {
                        Ok(())
                    } else if !stderr_tail.is_empty() {
                        Err(format!("docker compose failed: {stderr_tail}"))
                    } else {
                        Err(format!("docker compose exited with {status}"))
                    };
                }
                Ok(None) => {
                    if start.elapsed() > COMPOSE_LONG_TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_thread.join();
                        let _ = stderr_thread.join();
                        return Err(format!(
                            "docker compose timed out after {}s",
                            COMPOSE_LONG_TIMEOUT.as_secs()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// ── image registry paths ───────────────────────────────────────────────────

pub const CPU_IMAGE: &str = "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cpu";
pub const CUDA_IMAGE: &str = "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda";
pub const VALIDATOR_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol-rs/quip-network-node";
pub const DASHBOARD_IMAGE: &str = "registry.gitlab.com/quip.network/dashboard.quip.network";
pub const COMPOSE_IMAGE_TAG: &str = "v0.2-preview";

/// Image path (without tag) for a given `ImageTag`. D-Wave mining rides on
/// the CPU image via config.toml's `[dwave]` section, so there's no Qpu
/// branch — it's just Cpu with a token.
pub fn image_for_tag(image_tag: ImageTag) -> &'static str {
    match image_tag {
        ImageTag::Cuda => CUDA_IMAGE,
        ImageTag::Cpu => CPU_IMAGE,
    }
}

// ── Tauri commands ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn check_docker_installed() -> Result<bool, String> {
    let status = crate::cmd::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(status.status.success())
}

#[tauri::command]
pub async fn check_docker_hello_world() -> Result<bool, String> {
    let status = crate::cmd::new("docker")
        .args(["run", "--rm", "hello-world"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(status.status.success())
}

/// `docker compose` (space) is the v2+ CLI plugin; the legacy v1 was the
/// separate `docker-compose` (hyphen) Python binary and never registered
/// as a plugin. A successful exit from `docker compose version` therefore
/// means the plugin is installed — no need to parse the version string
/// (which Docker has already rev'd past v2, e.g. "v5.1.2" in Docker 29).
#[tauri::command]
pub async fn check_docker_compose_installed() -> Result<bool, String> {
    let output = crate::cmd::new("docker")
        .args(["compose", "version"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(output.status.success())
}

/// Pull every image needed by the current profile + service list. Runs
/// `docker compose --profile <p> pull [services...]` so the daemon checks the
/// registry for the configured v0.2 preview tags even if local copies exist.
#[tauri::command]
pub async fn pull_compose_images(app: AppHandle) -> Result<(), String> {
    let settings = crate::settings::load_settings();

    // Ensure assets are staged before compose tries to read the compose file.
    sync_stack_assets(
        &settings.run_mode,
        settings.node_config.port,
        settings.node_config.validator_port,
        &settings.node_config.public_host,
        native_rest_port(&settings.node_config),
    )?;

    pull_compose_images_for_settings(&app, &settings).await
}

async fn pull_compose_images_for_settings(
    app: &AppHandle,
    settings: &AppSettings,
) -> Result<(), String> {
    let profile = compose_profile(settings.image_tag);

    let mut args: Vec<String> = vec!["--profile".into(), profile.into(), "pull".into()];
    for s in compose_services(&settings.run_mode, settings.tls_enabled) {
        args.push((*s).into());
    }

    log_cmd(app, &format!("docker compose --profile {profile} pull ..."));
    run_compose_streaming(app, args).await
}

/// Start the compose stack (and, in Native mode, arrange for the native
/// binary to be started separately by `native::start_native_node`).
///
/// Sequence:
///   1. migrate existing v0.1 config/env, if present
///   2. auto-detect public_host in Docker mode
///   3. force native miner REST settings when the deferred native path is used
///   4. sync_stack_assets (staging + Caddyfile/public-addr patches)
///   5. write .env
///   6. write_config_toml
///   7. docker compose down  (clean slate; no-op on first start)
///   8. docker compose --profile <p> pull
///   9. docker compose --profile <p> up -d [services...]
#[tauri::command]
pub async fn start_stack(app: AppHandle) -> Result<(), String> {
    let mut settings = crate::settings::load_settings();

    // (1) Migrate any v0.1 config/env artifacts before writing fresh v0.2
    // manager-owned files. Promoted fields keep hand-edited public host/port
    // values from being lost by the generated config.
    let migration = crate::migration_v2::migrate_for_run_mode(&settings.run_mode)?;
    migration
        .promoted
        .apply_to_node_config(&mut settings.node_config);
    crate::migration_v2::persist_promoted_settings(&migration.promoted)?;
    crate::migration_v2::emit_report(&app, &migration);

    // (2) Docker-mode auto-detect of public_host; Native leaves it to the
    // binary.
    if settings.run_mode == RunMode::Docker && settings.node_config.public_host.is_empty() {
        if let Ok(ip) = crate::network::detect_public_ip().await {
            log_cmd(&app, &format!("Auto-detected public IP: {}", ip));
            settings.node_config.public_host = ip;
        }
    }

    let rest_port = native_rest_port(&settings.node_config);

    // (3) Native miner installation/update is deferred. Keep the old native
    // REST override isolated to Native mode so the Docker v0.2 config always
    // uses internal miner REST port 80.
    if settings.run_mode == RunMode::Native {
        settings.node_config.rest_host = "127.0.0.1".to_string();
        settings.node_config.rest_insecure_port = rest_port as i16;
    }

    // (4) Stage assets after migration/auto-detection so public_host can drive
    // the validator's public address.
    sync_stack_assets(
        &settings.run_mode,
        settings.node_config.port,
        settings.node_config.validator_port,
        &settings.node_config.public_host,
        rest_port,
    )?;

    // (5) .env
    write_env_file(&settings)?;

    // (6) config.toml (host side, bind-mounted into the node container in
    // Docker mode; read directly by the native binary in Native mode).
    log_cmd(&app, "Writing config.toml");
    crate::config::write_config_toml(&settings.node_config, &settings.run_mode)?;

    // (7) Clean slate. `down` is cheap and idempotent; removes stale
    // containers left behind when the user switches image_tag/profile.
    log_cmd(&app, "docker compose down");
    let _ = run_compose_streaming(&app, vec!["down".into()]).await;

    let profile = compose_profile(settings.image_tag);

    // (8) Pull the configured v0.2 preview tags.
    pull_compose_images_for_settings(&app, &settings).await?;

    // (9) Up.
    let mut up_args: Vec<String> =
        vec!["--profile".into(), profile.into(), "up".into(), "-d".into()];
    for s in compose_services(&settings.run_mode, settings.tls_enabled) {
        up_args.push((*s).into());
    }
    log_cmd(
        &app,
        &format!(
            "docker compose --profile {profile} up -d{}",
            if up_args.len() > 4 {
                format!(" {}", up_args[4..].join(" "))
            } else {
                String::new()
            }
        ),
    );
    run_compose_streaming(&app, up_args).await
}

/// Stop the compose stack. Named volumes (quip-pgdata, quip-caddy-data,
/// quip-caddy-config) are preserved by default — `down` removes containers
/// and the project network only.
#[tauri::command]
pub async fn stop_stack(app: AppHandle) -> Result<(), String> {
    let _ = app.emit("stop-started", serde_json::json!({}));

    // Kill the log-streamer child first — same ordering as the old
    // stop_node_container sequence, so `docker compose logs -f` unblocks
    // before we tear containers down.
    let log_state = app.state::<LogStreamState>();
    log_state.kill_child();
    *log_state.stop_flag.lock().unwrap() = true;

    log_cmd(&app, "docker compose down");
    let result = run_compose_streaming(&app, vec!["down".into()]).await;

    // Belt-and-suspenders: force-remove each container by the explicit
    // name the compose file declares. Covers cases where `docker compose
    // down` reports success but the project-label lookup misses — which
    // has been observed with some compose/Docker version combos. Missing
    // names exit non-zero (no such container); we ignore those.
    force_remove_known_containers(&app).await;

    // Sweep orphan containers that aren't part of the compose project but
    // are running our node images. Catches stragglers from older builds
    // that ran `docker run <node-image> --version` and ended up launching
    // a full node under a random anonymous name.
    sweep_orphan_node_containers(&app).await;

    match &result {
        Ok(_) => {
            log_output(&app, "Compose stack stopped.");
            let _ = app.emit("stop-complete", serde_json::json!({ "success": true }));
        }
        Err(e) => {
            log_err(&app, e);
            let _ = app.emit(
                "stop-complete",
                serde_json::json!({ "success": false, "error": e }),
            );
        }
    }

    result
}

/// Container names declared in the upstream `docker-compose.yml`. These
/// don't change with profile or run-mode — they're fixed by compose's
/// `container_name:` directive — so we can always try to reap them.
const KNOWN_CONTAINER_NAMES: &[&str] = &[
    "quip-cpu",
    "quip-cuda",
    "quip-validator",
    "quip-bootstrap",
    "quip-dashboard",
    "quip-postgres",
    "quip-caddy",
    // Legacy one-container TUI path; kept as a cleanup-only backstop.
    "quip-node",
];

/// Force-remove every container the compose file declares by name. Runs
/// after `docker compose down` as a backstop — `down` has been observed
/// silently no-op-ing when the project label doesn't line up with what
/// we pass. `docker rm -f` on a missing name returns non-zero which we
/// ignore; we only surface output when something is actually removed.
async fn force_remove_known_containers(app: &AppHandle) {
    for &name in KNOWN_CONTAINER_NAMES {
        let out = tokio::task::spawn_blocking(move || {
            crate::cmd::new("docker").args(["rm", "-f", name]).output()
        })
        .await;
        let Ok(Ok(output)) = out else { continue };
        if output.status.success() {
            // Docker prints the removed container's name to stdout.
            let removed = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !removed.is_empty() {
                log_cmd(app, &format!("docker rm -f {name}"));
                log_output(app, &format!("Removed {removed}"));
            }
        }
    }
}

/// Force-remove any containers running our node images whose name doesn't
/// start with `quip-` (i.e. anonymous / non-compose runners). Best-effort —
/// individual failures are logged but don't fail the stop. The name prefix
/// check is a sturdier stand-in for "lacks the compose project label"
/// since `docker ps --filter label!=…` isn't portable.
async fn sweep_orphan_node_containers(app: &AppHandle) {
    for image in &[CPU_IMAGE, CUDA_IMAGE] {
        let image_ref = format!("{image}:{COMPOSE_IMAGE_TAG}");
        let ps = tokio::task::spawn_blocking({
            let image_ref = image_ref.clone();
            move || {
                crate::cmd::new("docker")
                    .args([
                        "ps",
                        "--filter",
                        &format!("ancestor={image_ref}"),
                        "--format",
                        "{{.ID}} {{.Names}}",
                    ])
                    .output()
            }
        })
        .await;
        let Ok(Ok(output)) = ps else { continue };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let Some(id) = parts.next() else { continue };
            let name = parts.next().unwrap_or("");
            if name.starts_with("quip-") {
                continue; // Managed by compose — leave for `down` to reap.
            }
            log_cmd(
                app,
                &format!("docker rm -f {id}  # orphan {name} from {image_ref}"),
            );
            let id = id.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                crate::cmd::new("docker").args(["rm", "-f", &id]).output()
            })
            .await;
        }
    }
}

/// Query the stack via `docker compose ps --all --format json`. Compose v2
/// emits JSONL: one JSON object per line.
#[tauri::command]
pub async fn get_stack_status() -> Result<StackStatus, String> {
    let output = tokio::task::spawn_blocking(|| {
        compose_cmd()
            .args(["ps", "--all", "--format", "json"])
            .output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    if !output.status.success() {
        // compose returns non-zero on "no such project" etc. Surface as an
        // empty stack rather than error — matches the "not running" UI.
        return Ok(StackStatus {
            services: Vec::new(),
            overall: StackHealth::Stopped,
        });
    }

    // Compose v2.x emits JSONL (one object per line); v2.21+ / Docker 29 /
    // Compose v5 emit a single JSON array. Try array first, fall back to
    // JSONL — neither mode is self-identifying enough to trust blindly.
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let objects: Vec<serde_json::Value> = if text.starts_with('[') {
        serde_json::from_str(text).unwrap_or_default()
    } else {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    };

    let mut services = Vec::new();
    for v in objects {
        let state = v
            .get("State")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let running = state == "running";
        let health = v
            .get("Health")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        services.push(ServiceStatus {
            name: v
                .get("Name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            service: v
                .get("Service")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            running,
            health,
            status_text: v
                .get("Status")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            image: v
                .get("Image")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }

    let overall = if services.is_empty() {
        StackHealth::Stopped
    } else if services
        .iter()
        .any(|s| s.health.as_deref() == Some("unhealthy"))
    {
        StackHealth::Unhealthy
    } else if services.iter().all(|s| {
        s.running
            && s.health
                .as_deref()
                .map(|h| h == "healthy" || h == "starting")
                .unwrap_or(true)
    }) {
        StackHealth::Running
    } else if services.iter().any(|s| s.running) {
        StackHealth::Degraded
    } else {
        StackHealth::Stopped
    };

    Ok(StackStatus { services, overall })
}

/// `docker compose config` output — replaces the old `get_container_config`.
/// Useful for debugging the merged configuration the daemon would receive.
#[tauri::command]
pub async fn get_stack_config() -> Result<String, String> {
    let output = tokio::task::spawn_blocking(|| compose_cmd().args(["config"]).output())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

// Silence unused-import warnings while the module sits alongside docker.rs
// during step 4. stack_caddyfile is re-exported for callers that want the
// patched Caddyfile path for diagnostics.
#[allow(dead_code)]
pub(crate) fn _caddyfile_path() -> PathBuf {
    stack_caddyfile()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_profile_uses_only_v02_cpu_and_cuda_profiles() {
        assert_eq!(compose_profile(ImageTag::Cpu), "cpu");
        assert_eq!(compose_profile(ImageTag::Cuda), "cuda");
    }

    #[test]
    fn compose_services_docker_uses_profile_defaults() {
        assert!(compose_services(&RunMode::Docker, true).is_empty());
        assert!(compose_services(&RunMode::Docker, false).is_empty());
    }

    #[test]
    fn compose_services_native_runs_only_support_services() {
        assert_eq!(
            compose_services(&RunMode::Native, false),
            ["quip-validator", "dashboard", "postgres", "caddy"]
        );
        assert!(!compose_services(&RunMode::Native, true).contains(&"cpu"));
        assert!(!compose_services(&RunMode::Native, true).contains(&"cuda"));
        assert!(!compose_services(&RunMode::Native, true).contains(&"quip-bootstrap"));
    }

    #[test]
    fn v02_cleanup_containers_do_not_include_qpu_service() {
        assert!(KNOWN_CONTAINER_NAMES.contains(&"quip-validator"));
        assert!(KNOWN_CONTAINER_NAMES.contains(&"quip-bootstrap"));
        assert!(!KNOWN_CONTAINER_NAMES.contains(&"quip-faucet"));
        assert!(!KNOWN_CONTAINER_NAMES.contains(&"quip-qpu"));
    }

    #[test]
    fn miner_image_paths_use_v02_names() {
        assert_eq!(
            image_for_tag(ImageTag::Cpu),
            "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cpu"
        );
        assert_eq!(
            image_for_tag(ImageTag::Cuda),
            "registry.gitlab.com/quip.network/quip-protocol/quip-miner-cuda"
        );
        assert_eq!(
            VALIDATOR_IMAGE,
            "registry.gitlab.com/quip.network/quip-protocol-rs/quip-network-node"
        );
        assert_eq!(
            DASHBOARD_IMAGE,
            "registry.gitlab.com/quip.network/dashboard.quip.network"
        );
        assert_eq!(COMPOSE_IMAGE_TAG, "v0.2-preview");
    }

    #[test]
    fn env_lines_use_v02_dashboard_and_validator_keys() {
        let mut settings = AppSettings::default();
        settings.hostname = String::new();
        settings.cert_email = "ops@example.com".to_string();
        settings.zerossl_api_key = "zero".to_string();
        settings.run_mode = RunMode::Docker;
        settings.node_config.node_name = "validator-home".to_string();
        settings.node_config.num_cpus = 4;

        let lines = render_env_lines(&settings, 501, 1000, "postgres-secret");
        let env = lines.join("\n");

        assert!(env.contains("PUID=501"));
        assert!(env.contains("PGID=1000"));
        assert!(env.contains("QUIP_HOSTNAME=:20049"));
        assert!(env.contains("CERT_EMAIL=ops@example.com"));
        assert!(env.contains("ZEROSSL_API_KEY=zero"));
        assert!(env.contains("POSTGRES_PASSWORD=postgres-secret"));
        assert!(env.contains("QUIP_MINER_TAG=v0.2-preview"));
        assert!(env.contains("QUIP_DASHBOARD_TAG=v0.2-preview"));
        assert!(env.contains("QUIP_VALIDATOR_TAG=v0.2-preview"));
        assert!(env.contains("QUIP_MINER_CPUSET=0-3"));
        assert!(env.contains("VALIDATOR_NAME=validator-home"));
        assert!(env.contains("QUIP_VALIDATORS=ws://quip-validator:9944"));
        assert!(env.contains("QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944"));
        assert!(!env.contains("QUIP_NODE_URL"));
        assert!(!env.contains("QUIP_NODE_TOKEN"));
        assert!(!env.contains("QUIP_FAUCET_URL"));
    }

    #[test]
    fn env_lines_use_public_host_for_caddy_when_it_is_dns() {
        let mut settings = AppSettings::default();
        settings.node_config.public_host = "node.example.com".to_string();
        settings.hostname = "dashboard.example.com".to_string();

        let env = render_env_lines(&settings, 501, 1000, "pg").join("\n");

        assert!(env.contains("QUIP_HOSTNAME=node.example.com, node.example.com:20049"));
    }

    #[test]
    fn env_lines_fall_back_to_hostname_when_public_host_is_not_dns() {
        let mut settings = AppSettings::default();
        settings.node_config.public_host = "203.0.113.9".to_string();
        settings.hostname = "dashboard.example.com".to_string();

        let env = render_env_lines(&settings, 501, 1000, "pg").join("\n");

        assert!(env.contains("QUIP_HOSTNAME=dashboard.example.com"));
    }

    #[test]
    fn env_lines_preserve_dwave_key() {
        let mut settings = AppSettings::default();
        settings.node_config.dwave_config = Some(crate::settings::DwaveConfig {
            token: "dwave-token".to_string(),
            ..crate::settings::DwaveConfig::default()
        });

        let env = render_env_lines(&settings, 501, 1000, "pg").join("\n");
        assert!(env.contains("DWAVE_API_KEY=dwave-token"));
    }

    #[test]
    fn env_lines_native_omits_docker_miner_validator_env() {
        let mut settings = AppSettings {
            run_mode: RunMode::Native,
            ..AppSettings::default()
        };
        settings.node_config.node_name = "physical-miner-validator".to_string();

        let env = render_env_lines(&settings, 501, 1000, "pg").join("\n");

        assert!(!env.contains("QUIP_VALIDATORS="));
        assert!(env.contains("QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944"));
    }

    #[test]
    fn env_lines_default_validator_name_and_single_cpu_cpuset() {
        let settings = AppSettings::default();
        let env = render_env_lines(&settings, 501, 1000, "pg").join("\n");

        assert!(env.contains("VALIDATOR_NAME=quip-validator"));
        assert!(env.contains("QUIP_MINER_CPUSET=0"));
    }
}
