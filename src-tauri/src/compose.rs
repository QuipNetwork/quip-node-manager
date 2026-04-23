// SPDX-License-Identifier: AGPL-3.0-or-later
//! `docker compose`-based stack orchestration — replaces the single
//! `docker run quip-node` path from docker.rs.
//!
//! Docker mode drives the full stack (node + dashboard + postgres + caddy).
//! Native mode starts only the non-node services (`dashboard`+`postgres`,
//! plus `caddy` if TLS is on) and expects the native binary to run the node
//! on the host; the dashboard reaches it via `host.docker.internal`.

use crate::log_stream::LogStreamState;
use crate::settings::{
    AppSettings, ImageTag, NodeConfig, RunMode, ServiceStatus, StackHealth,
    StackStatus,
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

/// Compose profile name for `(image_tag, dashboard, tls)`. TLS without
/// dashboard is meaningless in this stack (Caddy only fronts the dashboard),
/// so we collapse the TLS branch when dashboard is off.
pub fn compose_profile(
    image_tag: ImageTag,
    dashboard: bool,
    tls: bool,
) -> &'static str {
    match (image_tag, dashboard, tls) {
        (ImageTag::Cpu, true, true) => "cpu",
        (ImageTag::Cpu, true, false) => "cpu-notls",
        (ImageTag::Cpu, false, _) => "cpu-nodash",
        (ImageTag::Cuda, true, true) => "cuda",
        (ImageTag::Cuda, true, false) => "cuda-notls",
        (ImageTag::Cuda, false, _) => "cuda-nodash",
    }
}

/// Whether the compose stack is skipped entirely. True iff Native mode with
/// dashboard disabled — the user wants the bare native binary and nothing
/// else. Every other combo runs some compose services.
pub fn compose_skipped(
    run_mode: &RunMode,
    dashboard_enabled: bool,
) -> bool {
    *run_mode == RunMode::Native && !dashboard_enabled
}

/// Explicit service list for `docker compose up -d [services...]`.
///
/// - Docker mode: empty slice means "start every service the profile allows"
///   (compose default). We don't enumerate because the profile already gates
///   things down to the correct set.
/// - Native mode: we skip the node container and hand compose an explicit
///   list of the non-node services. The profile is still set (dashboard/
///   postgres/caddy are profile-gated), but the positional args restrict
///   startup to just those.
pub fn compose_services(
    run_mode: &RunMode,
    tls_enabled: bool,
) -> &'static [&'static str] {
    match (run_mode, tls_enabled) {
        (RunMode::Docker, _) => &[],
        (RunMode::Native, true) => &["dashboard", "postgres", "caddy"],
        (RunMode::Native, false) => &["dashboard-direct", "postgres"],
    }
}

// ── Native REST port ───────────────────────────────────────────────────────

/// Port the node exposes for the dashboard to poll. Used for both modes:
///   - Native + dashboard: host binds `127.0.0.1:<port>`; dashboard reaches
///     it via `host.docker.internal:<port>` from inside the container.
///   - Docker + dashboard: node container binds `0.0.0.0:<port>`; dashboard
///     reaches it via the `quip-node` compose network alias.
/// Honors the user-configured `rest_insecure_port` when set; otherwise falls
/// back to 20100 (non-privileged — the containerized node runs as the host
/// PUID, so ports <1024 are out of reach without capabilities).
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
fn compose_cmd() -> Command {
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
fn write_env_file(settings: &AppSettings) -> Result<(), String> {
    let (puid, pgid) = host_uid_gid();
    let dwave_key = settings
        .node_config
        .dwave_config
        .as_ref()
        .map(|d| d.token.clone())
        .unwrap_or_default();
    let pg_password = crate::settings::postgres_password();

    let mut lines = vec![
        format!("PUID={puid}"),
        format!("PGID={pgid}"),
        format!("QUIP_HOSTNAME={}", settings.dashboard_hostname),
        format!("CERT_EMAIL={}", settings.cert_email),
        format!("ZEROSSL_API_KEY={}", settings.zerossl_api_key),
        format!("DWAVE_API_KEY={dwave_key}"),
        format!("POSTGRES_PASSWORD={pg_password}"),
    ];

    // Point the dashboard at the node's REST endpoint. The compose default
    // is `http://quip-node:80`, but port 80 would require the containerized
    // node to run as root. Override to a non-privileged port that we also
    // force on the node side below.
    //   - Native : host-bound binary reached via host.docker.internal
    //   - Docker : node container reached via the `quip-node` compose alias
    if settings.dashboard_enabled {
        let port = native_rest_port(&settings.node_config);
        let host = match settings.run_mode {
            RunMode::Native => "host.docker.internal",
            RunMode::Docker => "quip-node",
        };
        lines.push(format!("QUIP_NODE_URL=http://{host}:{port}"));
    }

    let path = stack_project_dir().join(".env");
    fs::write(&path, lines.join("\n") + "\n")
        .map_err(|e| format!("write .env: {e}"))?;

    // Best-effort 0600: DWAVE_API_KEY and POSTGRES_PASSWORD shouldn't be
    // world-readable on shared systems.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

// ── streaming compose output ───────────────────────────────────────────────

/// Default timeout for long-running compose ops (pull, up). Compose itself
/// respects context timeouts; this is a backstop against a wedged daemon.
const COMPOSE_LONG_TIMEOUT: Duration = Duration::from_secs(600);

async fn run_compose_streaming(
    app: &AppHandle,
    args: Vec<String>,
) -> Result<(), String> {
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
                let _ = app_out.emit(
                    "pull-progress",
                    serde_json::json!({ "line": &line }),
                );
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

const CPU_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu";
const CUDA_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cuda";

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
/// `docker compose --profile <p> pull [services...]` so the daemon talks to
/// the registry for each entry even if local copies exist (cache-bust for
/// `:latest` tags).
#[tauri::command]
pub async fn pull_compose_images(app: AppHandle) -> Result<(), String> {
    let settings = crate::settings::load_settings();
    if compose_skipped(&settings.run_mode, settings.dashboard_enabled) {
        return Ok(());
    }

    // Ensure assets are staged before compose tries to read the compose file.
    sync_stack_assets(
        &app,
        &settings.run_mode,
        native_rest_port(&settings.node_config),
    )?;

    let profile = compose_profile(
        settings.image_tag,
        settings.dashboard_enabled,
        settings.tls_enabled,
    );

    let mut args: Vec<String> =
        vec!["--profile".into(), profile.into(), "pull".into()];
    for s in compose_services(&settings.run_mode, settings.tls_enabled) {
        args.push((*s).into());
    }

    log_cmd(&app, &format!("docker compose --profile {profile} pull ..."));
    run_compose_streaming(&app, args).await
}

/// Start the compose stack (and, in Native mode, arrange for the native
/// binary to be started separately by `native::start_native_node`).
///
/// Sequence:
///   1. sync_stack_assets (staging + Caddyfile patch for Native)
///   2. write .env (credentials, QUIP_NODE_URL for Native)
///   3. auto-detect public_host in Docker mode
///   4. force rest_host=0.0.0.0 + rest_insecure_port in Native+dashboard
///   5. write_config_toml
///   6. docker compose down  (clean slate; no-op on first start)
///   7. docker compose --profile <p> pull
///   8. docker compose --profile <p> up -d [services...]
#[tauri::command]
pub async fn start_stack(app: AppHandle) -> Result<(), String> {
    let mut settings = crate::settings::load_settings();

    if compose_skipped(&settings.run_mode, settings.dashboard_enabled) {
        log_cmd(
            &app,
            "Native mode with dashboard disabled — no compose stack to start.",
        );
        return Ok(());
    }

    let rest_port = native_rest_port(&settings.node_config);

    // (1) Stage assets.
    sync_stack_assets(&app, &settings.run_mode, rest_port)?;

    // (2) Docker-mode auto-detect of public_host; Native leaves it to the
    // binary.
    if settings.run_mode == RunMode::Docker
        && settings.node_config.public_host.is_empty()
    {
        if let Ok(ip) = crate::network::detect_public_ip().await {
            log_cmd(&app, &format!("Auto-detected public IP: {}", ip));
            settings.node_config.public_host = ip;
        }
    }

    // (3) Force REST on whenever the dashboard is enabled — it's how the
    // dashboard polls telemetry. The default NodeConfig has REST disabled
    // (rest_insecure_port = -1), so without this override the dashboard
    // silently can't reach the node.
    //   - Native : bind 127.0.0.1. Docker Desktop's vpnkit proxies
    //              host.docker.internal to the host's loopback, so 0.0.0.0
    //              isn't needed — and 127.0.0.1 avoids leaking this
    //              unauthenticated admin port onto the LAN. (Native mode is
    //              macOS-only; Linux Docker CE would need a different bind.)
    //   - Docker : bind 0.0.0.0 inside the container so the dashboard can
    //              reach it across the compose network via the `quip-node`
    //              alias. The port isn't published to the host, so LAN
    //              exposure isn't a concern.
    // We mutate the in-memory copy only — app-settings.json on disk is
    // untouched.
    if settings.dashboard_enabled {
        let bind = match settings.run_mode {
            RunMode::Native => "127.0.0.1",
            RunMode::Docker => "0.0.0.0",
        };
        settings.node_config.rest_host = bind.to_string();
        settings.node_config.rest_insecure_port = rest_port as i16;
    }

    // (4) .env
    write_env_file(&settings)?;

    // (5) config.toml (host side, bind-mounted into the node container in
    // Docker mode; read directly by the native binary in Native mode).
    log_cmd(&app, "Writing config.toml");
    crate::config::write_config_toml(
        &settings.node_config,
        &settings.run_mode,
    )?;

    // (6) Clean slate. `down` is cheap and idempotent; removes stale
    // containers left behind when the user switches image_tag/profile.
    log_cmd(&app, "docker compose down");
    let _ = run_compose_streaming(&app, vec!["down".into()]).await;

    let profile = compose_profile(
        settings.image_tag,
        settings.dashboard_enabled,
        settings.tls_enabled,
    );

    // (7) Pull (always — cache-bust :latest).
    pull_compose_images(app.clone()).await?;

    // (8) Up.
    let mut up_args: Vec<String> = vec![
        "--profile".into(),
        profile.into(),
        "up".into(),
        "-d".into(),
    ];
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
            let _ = app.emit(
                "stop-complete",
                serde_json::json!({ "success": true }),
            );
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
    "quip-qpu",
    "quip-dashboard",
    "quip-postgres",
    "quip-caddy",
];

/// Force-remove every container the compose file declares by name. Runs
/// after `docker compose down` as a backstop — `down` has been observed
/// silently no-op-ing when the project label doesn't line up with what
/// we pass. `docker rm -f` on a missing name returns non-zero which we
/// ignore; we only surface output when something is actually removed.
async fn force_remove_known_containers(app: &AppHandle) {
    for &name in KNOWN_CONTAINER_NAMES {
        let out = tokio::task::spawn_blocking(move || {
            crate::cmd::new("docker")
                .args(["rm", "-f", name])
                .output()
        })
        .await;
        let Ok(Ok(output)) = out else { continue };
        if output.status.success() {
            // Docker prints the removed container's name to stdout.
            let removed = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_string();
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
        let image_ref = format!("{image}:latest");
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
                crate::cmd::new("docker")
                    .args(["rm", "-f", &id])
                    .output()
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
    let output = tokio::task::spawn_blocking(|| {
        compose_cmd().args(["config"]).output()
    })
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
