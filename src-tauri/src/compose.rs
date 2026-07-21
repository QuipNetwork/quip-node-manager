// SPDX-License-Identifier: AGPL-3.0-or-later
//! `docker compose`-based stack orchestration.
//!
//! Docker mode drives the full v0.2 stack (miner + validator + dashboard +
//! postgres + caddy). Native mode starts the host miner plus Docker-side
//! validator/dashboard support services.

use crate::log_stream::LogStreamState;
use crate::progress::{ProgressSink, TauriSink};
use crate::settings::{
    AppSettings, ImageTag, NodeConfig, RunMode, ServiceStatus, StackHealth, StackStatus,
};
use crate::stack_assets::{stack_compose_file, stack_project_dir, sync_stack_assets};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Monotonic id stamped on every `pull-progress` / `pull-complete` event of a
/// single pull. The frontend uses it to ignore stale events delivered out of
/// order (a late layer event arriving after the pull's `pull-complete` must not
/// resurrect the progress panel) and to treat `pull-complete` as the one
/// authoritative "this pull is over" signal.
static PULL_GENERATION: AtomicU64 = AtomicU64::new(0);

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

/// Compose profile name for v0.2 — the same string as the image's service
/// name. The standard upstream profiles always include dashboard, postgres,
/// Caddy, the selected miner, and validator.
pub fn compose_profile(image_tag: ImageTag) -> &'static str {
    image_tag.service()
}

/// Explicit service list for `docker compose up -d [services...]`.
///
/// - Docker mode: empty slice means "start every service the profile allows"
///   (compose default). We don't enumerate because the profile already gates
///   things down to the correct set.
/// - Native mode: we skip the miner container and hand compose an explicit
///   list of support services. The profile is still set so these services are
///   eligible, while positional args restrict startup to them.
pub fn compose_services(run_mode: &RunMode) -> &'static [&'static str] {
    match run_mode {
        RunMode::Docker => &[],
        RunMode::Native => &["quip-validator", "dashboard", "postgres", "caddy"],
    }
}

/// Services whose state feeds the stack-health roll-up for the given mode.
/// `docker compose ps --all` lists every container in the project — including
/// Exited miners left behind by an image-type or run-mode switch (Stop never
/// removes containers) — so health must only consider the services the
/// current configuration actually starts.
pub fn expected_services(run_mode: &RunMode, image_tag: ImageTag) -> Vec<&'static str> {
    match run_mode {
        RunMode::Docker => vec![
            image_tag.service(),
            "quip-validator",
            "dashboard",
            "postgres",
            "caddy",
        ],
        RunMode::Native => compose_services(&RunMode::Native).to_vec(),
    }
}

// ── compose command builder ────────────────────────────────────────────────

fn to_forward_slash(p: PathBuf) -> String {
    // Docker Desktop on Windows is happier with forward slashes as
    // `--project-directory`; it accepts them everywhere else too.
    p.to_string_lossy().replace('\\', "/")
}

/// `docker compose -f <data_dir>/docker-compose.yml [-f
/// <data_dir>/docker-compose.override.yml] --project-directory <data_dir>
/// --project-name quip` — the common prefix for every compose invocation.
///
/// The override file is added only when it exists, and always after the base
/// file so its values win. Compose auto-loads `docker-compose.override.yml`
/// only when discovering files itself; passing `-f` disables that, so without
/// this the operator override documented upstream would be silently ignored.
pub(crate) fn compose_cmd() -> Command {
    let override_file = crate::stack_assets::stack_override_file();
    let args = compose_prefix_args(
        &to_forward_slash(stack_compose_file()),
        override_file.is_file().then(|| to_forward_slash(override_file)).as_deref(),
        &to_forward_slash(stack_project_dir()),
    );
    let mut c = crate::cmd::new("docker");
    c.args(&args);
    c
}

/// Argument prefix shared by every compose invocation, split out so the
/// `-f` ordering contract is testable without touching the filesystem.
///
/// The override must come after the base file: compose merges left to right,
/// so a later file wins. Reversing them would silently reinstate the bundled
/// defaults over the operator's changes.
fn compose_prefix_args(
    compose_file: &str,
    override_file: Option<&str>,
    project_dir: &str,
) -> Vec<String> {
    let mut args = vec!["compose".to_string(), "-f".to_string(), compose_file.to_string()];
    if let Some(o) = override_file {
        args.push("-f".to_string());
        args.push(o.to_string());
    }
    args.extend([
        "--project-directory".to_string(),
        project_dir.to_string(),
        "--project-name".to_string(),
        "quip".to_string(),
    ]);
    args
}

// ── postgres identity ──────────────────────────────────────────────────────

/// Postgres role + database the dashboard authenticates as. These mirror the
/// compose defaults (`${POSTGRES_USER:-quip}` / `${POSTGRES_DB:-quip}`); the
/// manager never overrides them in `.env`.
const PG_USER: &str = "quip";
const PG_DB: &str = "quip";
/// Fixed container name from the compose `container_name:` directive.
const PG_CONTAINER: &str = "quip-postgres";
/// Project-scoped Postgres data volume. Compose names volumes `<project>_<key>`
/// and we run under `--project-name quip` with a `pgdata` volume key (the fixed
/// global `name:` is stripped at stage time — see `stack_assets`), so the data
/// lands in `quip_pgdata`. Resetting this volume forces Postgres to
/// re-initialise with the current `POSTGRES_PASSWORD`.
pub const PGDATA_VOLUME: &str = "quip_pgdata";

// ── .env generation ────────────────────────────────────────────────────────

/// The channel-resolved tag for each stack image, decided **independently per
/// repository** (miner / validator / dashboard advance on their own cadence).
pub(crate) struct ResolvedImageTags {
    pub miner: String,
    pub validator: String,
    pub dashboard: String,
}

/// Resolve each image's tag from its own GitLab container registry for the
/// settings' update channel (see `crate::registry`). Every image falls back
/// independently to `COMPOSE_IMAGE_TAG` — the compose `:-v0.2` default — when
/// its registry is unreachable or carries no canonical tag on the channel, so
/// starting the stack never hard-fails on a network hiccup and one image's
/// gap never blocks the others.
pub(crate) async fn resolve_channel_image_tags(settings: &AppSettings) -> ResolvedImageTags {
    let ch = settings.update_channel;
    let (miner, validator, dashboard) = tokio::join!(
        crate::registry::resolve_image_channel_tag(image_for_tag(settings.image_tag), ch),
        crate::registry::resolve_image_channel_tag(VALIDATOR_IMAGE, ch),
        crate::registry::resolve_image_channel_tag(DASHBOARD_IMAGE, ch),
    );
    let fallback = || COMPOSE_IMAGE_TAG.to_string();
    ResolvedImageTags {
        miner: miner.unwrap_or_else(fallback),
        validator: validator.unwrap_or_else(fallback),
        dashboard: dashboard.unwrap_or_else(fallback),
    }
}

/// Value of `key` (e.g. `QUIP_MINER_TAG`) currently pinned in `<data_dir>/.env`,
/// or `None` when `.env` hasn't been written yet or the key is absent.
pub(crate) fn current_pinned_tag(key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let contents = fs::read_to_string(stack_project_dir().join(".env")).ok()?;
    contents
        .lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Write `<data_dir>/.env` from AppSettings. Overwritten on every start —
/// there is no merge with an existing file. `tags` holds the channel-resolved
/// per-image tags (see `resolve_channel_image_tags`).
pub(crate) fn write_env_file(settings: &AppSettings, tags: &ResolvedImageTags) -> Result<(), String> {
    let (puid, pgid) = host_uid_gid();
    let pg_password = crate::settings::postgres_password();
    let lines = render_env_lines(settings, puid, pgid, &pg_password, tags);

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
    tags: &ResolvedImageTags,
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

    // GPU SM cap for NVIDIA MPS (CUDA_MPS_ACTIVE_THREAD_PERCENTAGE on the cuda
    // service). Use the first enabled GPU device's utilization; default to 100
    // (no cap) when none is configured. Only the cuda service reads it.
    let gpu_utilization = settings
        .node_config
        .gpu_device_configs
        .iter()
        .find(|d| d.enabled)
        .map(|d| d.utilization)
        .unwrap_or(100);

    let mut lines = vec![
        format!("PUID={puid}"),
        format!("PGID={pgid}"),
        format!("QUIP_HOSTNAME={hostname}"),
        format!("CERT_EMAIL={}", settings.cert_email),
        format!("ZEROSSL_API_KEY={}", settings.zerossl_api_key),
        format!("DWAVE_API_KEY={dwave_key}"),
        format!("POSTGRES_PASSWORD={pg_password}"),
        format!("QUIP_MINER_TAG={}", tags.miner),
        format!("QUIP_DASHBOARD_TAG={}", tags.dashboard),
        format!("QUIP_VALIDATOR_TAG={}", tags.validator),
        format!(
            "QUIP_MINER_CPUSET={}",
            cpu_set_for_config(&settings.node_config)
        ),
        format!("VALIDATOR_NAME={validator_name}"),
        format!("QUIP_GPU_UTILIZATION={gpu_utilization}"),
    ];

    // Miner memory ceiling. Omitted when unset so compose's own `:-16g`
    // default stays the single source of truth — writing an explicit value
    // here would fork the default across two files.
    if let Some(gb) = settings.miner_mem_limit_gb {
        lines.push(format!("QUIP_MINER_MEM_LIMIT={gb}g"));
    }

    // The miner is config-driven as of upstream nodes.quip.network's explicit
    // env contract: the compose cpu/cuda services no longer read QUIP_VALIDATORS
    // (it lives in config.toml's [miner].validators), so we don't write it.
    //
    // QUIP_VALIDATOR_RPC_URLS is intentionally NOT set here. The dashboard uses
    // it for both the chain RPC and (by stripping /rpc) the local miner REST,
    // so it must resolve both from one host — Caddy's internal :8088 listener.
    // We defer to the compose default (`ws://quip-caddy:8088/rpc`) so there's a
    // single source of truth in the upstream nodes.quip.network config.

    lines
}

// ── streaming compose output ───────────────────────────────────────────────

/// Default timeout for long-running compose ops (pull, up). Compose itself
/// respects context timeouts; this is a backstop against a wedged daemon.
const COMPOSE_LONG_TIMEOUT: Duration = Duration::from_secs(600);

/// How to surface a compose command's output.
#[derive(Clone, Copy)]
enum StdoutMode {
    /// Emit each raw line to node-log (used by up/down).
    Log,
    /// Parse `docker compose --progress json` events (emitted on stderr) into
    /// structured `pull-progress` events stamped with the given pull generation.
    /// Only image-level milestones and unparseable lines reach node-log, so the
    /// console isn't flooded with per-layer churn.
    PullJson(u64),
}

/// Parse one `--progress json` line and emit a structured `pull-progress`
/// event. Returns `true` if the line was a recognised progress event (so the
/// caller can skip treating it as error output). Image-level milestones are
/// also mirrored to node-log so the console keeps a "Pulling/Pulled <image>"
/// record without the per-layer noise.
fn emit_pull_progress_json(sink: &dyn ProgressSink, gen: u64, line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };
    let Some(id) = value.get("id").and_then(|v| v.as_str()) else {
        return false;
    };
    // Image-level events have no parent layer; mirror them to the log.
    if value.get("parent_id").is_none() && id.starts_with("Image ") {
        let text = value.get("text").and_then(|v| v.as_str()).unwrap_or("");
        sink.log("INFO", &format!("{} {}", text, id.trim_start_matches("Image ")));
    }
    // Stamp the generation so the frontend can discard events from a pull it has
    // already closed (see PULL_GENERATION).
    if let Some(obj) = value.as_object_mut() {
        let _ = obj.insert("gen".into(), serde_json::json!(gen));
    }
    sink.pull_progress(value);
    true
}

async fn run_compose_streaming(
    sink: Arc<dyn ProgressSink>,
    args: Vec<String>,
) -> Result<(), String> {
    run_compose_streaming_mode(sink, args, StdoutMode::Log).await
}

async fn run_compose_streaming_mode(
    sink: Arc<dyn ProgressSink>,
    args: Vec<String>,
    mode: StdoutMode,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let mut child = compose_cmd()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn docker compose: {e}"))?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let sink_out = Arc::clone(&sink);
        let stdout_thread = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if matches!(mode, StdoutMode::Log) {
                    sink_out.pull_progress(serde_json::json!({ "line": &line }));
                }
                sink_out.log("INFO", &line);
            }
        });

        // docker compose writes `--progress json` events to stderr, so the
        // PullJson parsing lives here. Non-progress lines stay error output.
        let sink_err = Arc::clone(&sink);
        let stderr_thread = std::thread::spawn(move || {
            let mut last = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let StdoutMode::PullJson(gen) = mode {
                    if emit_pull_progress_json(&*sink_err, gen, &line) {
                        continue;
                    }
                }
                sink_err.log("INFO", &line);
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
pub const COMPOSE_IMAGE_TAG: &str = "v0.2";

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
/// registry for the configured v0.2 tags even if local copies exist.
#[tauri::command]
pub async fn pull_compose_images(app: AppHandle) -> Result<(), String> {
    pull_compose_images_core(Arc::new(TauriSink::new(app))).await
}

/// Pull every compose stack image, reporting per-image progress through `sink`.
///
/// Loads the current app settings, stages the compose assets and `.env` file
/// (so compose sees the configured v0.2 image tags), then runs
/// `docker compose --profile <p> pull [services...]`. The `pull-complete`
/// notification is delivered via `sink.pull_complete` when the command exits.
pub(crate) async fn pull_compose_images_core(
    sink: Arc<dyn ProgressSink>,
) -> Result<(), String> {
    let settings = crate::settings::load_settings();

    // Ensure assets are staged before compose tries to read the compose file.
    sync_stack_assets(
        &settings.run_mode,
        settings.node_config.port,
        settings.node_config.validator_port,
        &settings.node_config.public_host,
        crate::config::native_rest_port(&settings.node_config),
        settings.node_config.validator_rpc_port,
    )?;
    // Write .env too: without it compose substitutes the compose.yml
    // `${QUIP_*_TAG:-…}` defaults, so a standalone pull (outside the full
    // start sequence) would silently fetch the wrong tag.
    let tags = resolve_channel_image_tags(&settings).await;
    write_env_file(&settings, &tags)?;

    pull_compose_images_for_settings(sink, &settings).await
}

async fn pull_compose_images_for_settings(
    sink: Arc<dyn ProgressSink>,
    settings: &AppSettings,
) -> Result<(), String> {
    let profile = compose_profile(settings.image_tag);

    let mut args: Vec<String> = vec![
        "--progress".into(),
        "json".into(),
        "--profile".into(),
        profile.into(),
        "pull".into(),
    ];
    for s in compose_services(&settings.run_mode) {
        args.push((*s).into());
    }

    // Allocate this pull's generation up front so every progress event and the
    // terminal pull-complete carry the same id.
    let gen = PULL_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;

    sink.log("INFO", &format!("$ docker compose --profile {profile} pull ..."));
    let result = run_compose_streaming_mode(Arc::clone(&sink), args, StdoutMode::PullJson(gen)).await;

    // Tell the UI the pull is over so it can hide the progress panel. Process
    // exit is the authoritative "pull is done" signal: per-image "Pulled"
    // accounting can miss an image's terminal event on some platforms, and a
    // late progress event delivered after this one is discarded by the frontend
    // because its generation is already closed.
    sink.pull_complete(
        gen,
        result.is_ok(),
        &result.as_ref().err().cloned().unwrap_or_default(),
    );
    result
}

/// Best-effort start of the per-user NVIDIA MPS control daemon so the cuda
/// container (which runs `ipc: host` and mounts `/tmp/nvidia-mps`) can share
/// the GPU's SMs in hardware, capped at the operator's configured utilization.
///
/// Linux + native NVIDIA only: MPS is unsupported under WSL2 / Docker Desktop,
/// so this is compiled out on other platforms. Non-fatal — a missing
/// `nvidia-cuda-mps-control` binary or an already-running daemon just leaves
/// MPS inactive, and the miner falls back to software / NVML throttling.
#[cfg(target_os = "linux")]
async fn ensure_mps_daemon(sink: Arc<dyn ProgressSink>) {
    let out = tokio::task::spawn_blocking(|| {
        crate::cmd::new("nvidia-cuda-mps-control")
            .arg("-d")
            .output()
    })
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => {
            sink.log("INFO", "$ nvidia-cuda-mps-control -d");
            sink.log("INFO", "Started NVIDIA MPS control daemon for GPU SM sharing.");
        }
        // Non-success is almost always "an instance is already running" — fine.
        // A missing binary (spawn error) means no NVIDIA tooling; stay quiet.
        Ok(Ok(o)) => {
            let detail = String::from_utf8_lossy(&o.stderr);
            let detail = detail.trim();
            if !detail.is_empty() {
                sink.log(
                    "INFO",
                    &format!("NVIDIA MPS daemon already active or unavailable: {detail}"),
                );
            }
        }
        _ => {}
    }
}

/// Start the compose stack (and, in Native mode, arrange for the native
/// binary to be started separately by `native::start_native_node`).
///
/// Thin Tauri command wrapper — see [`start_stack_core`] for the full sequence.
///
/// Sequence:
///   1. migrate existing v0.1 config/env, if present
///   2. auto-detect public_host in Docker mode
///   3. force native miner REST settings when Native mode is used
///   4. sync_stack_assets (staging + Caddyfile/public-addr patches)
///   5. write .env
///   6. write_config_toml
///   7. docker compose down  (clean slate; no-op on first start)
///   8. docker compose --profile <p> pull
///   9. docker compose --profile <p> up -d [services...]
#[tauri::command]
pub async fn start_stack(app: AppHandle) -> Result<(), String> {
    start_stack_core(Arc::new(TauriSink::new(app))).await
}

/// Stage assets, pull, and `up -d` the compose stack, reporting via `sink`.
///
/// Sequence:
///   1. migrate existing v0.1 config/env, if present
///   2. auto-detect public_host in Docker mode
///   3. force native miner REST settings when Native mode is used
///   4. sync_stack_assets (staging + Caddyfile/public-addr patches)
///   5. write .env
///   6. write_config_toml
///   7. docker compose --profile <p> pull
///   8. docker compose --profile <p> up -d [services...]
pub(crate) async fn start_stack_core(
    sink: Arc<dyn crate::progress::ProgressSink>,
) -> Result<(), String> {
    let mut settings = crate::settings::load_settings();

    // (1) Migrate any v0.1 config/env artifacts before writing fresh v0.2
    // manager-owned files. Promoted fields keep hand-edited public host/port
    // values from being lost by the generated config.
    let migration = crate::migration_v2::migrate_for_run_mode(&settings.run_mode)?;
    migration
        .promoted
        .apply_to_node_config(&mut settings.node_config);
    crate::migration_v2::persist_promoted_settings(&migration.promoted)?;
    for warning in &migration.warnings {
        for line in warning.lines() {
            sink.log("WARN", line);
        }
    }

    // (2) Docker-mode auto-detect of public_host; Native leaves it to the
    // binary.
    if settings.run_mode == RunMode::Docker && settings.node_config.public_host.is_empty() {
        match crate::network::detect_public_ip().await {
            Ok(ip) => {
                sink.log("INFO", &format!("$ Auto-detected public IP: {}", ip));
                settings.node_config.public_host = ip;
            }
            // Don't fail the start, but don't hide it either: without a
            // public_host the validator advertises no public address.
            Err(e) => sink.log(
                "ERROR",
                &format!(
                    "Warning: could not auto-detect public IP ({e}); the node will not \
                     advertise a public address. Set a public host in Settings."
                ),
            ),
        }
    }

    let rest_port = crate::config::native_rest_port(&settings.node_config);

    // (3) Materialise the resolved native REST port so config rendering and the
    // staged Caddyfile both publish the same port. (rest_host is forced to
    // loopback inside the config renderer.)
    if settings.run_mode == RunMode::Native {
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
        settings.node_config.validator_rpc_port,
    )?;

    // (5) .env — pin each QUIP_*_TAG to its image's channel-resolved tag.
    let tags = resolve_channel_image_tags(&settings).await;
    write_env_file(&settings, &tags)?;

    // (6) config.toml (host side, bind-mounted into the node container in
    // Docker mode; read directly by the native binary in Native mode).
    sink.log("INFO", "$ Writing config.toml");
    crate::config::write_config_toml(&settings.node_config, &settings.run_mode)?;

    let profile = compose_profile(settings.image_tag);

    // (7) Pull the configured v0.2 tags (also drives the pull-progress panel).
    pull_compose_images_for_settings(Arc::clone(&sink), &settings).await?;

    // Start the host NVIDIA MPS daemon before the cuda container so it can
    // attach (native Linux GPU hosts only; no-op everywhere else).
    #[cfg(target_os = "linux")]
    {
        if settings.run_mode == RunMode::Docker && settings.image_tag == ImageTag::Cuda {
            ensure_mps_daemon(Arc::clone(&sink)).await;
        }
    }

    // (8) Up. There is no separate `down`: Stop only stops the containers, so a
    // normal restart reuses them and compose recreates just what changed.
    // `--remove-orphans` reaps containers for services the current compose no
    // longer declares (e.g. the removed quip-bootstrap) without destroying the
    // rest.
    let mut up_args: Vec<String> = vec![
        "--profile".into(),
        profile.into(),
        "up".into(),
        "-d".into(),
        "--remove-orphans".into(),
    ];
    for s in compose_services(&settings.run_mode) {
        up_args.push((*s).into());
    }
    sink.log(
        "INFO",
        &format!(
            "$ docker compose --profile {profile} up -d --remove-orphans{}",
            if up_args.len() > 5 {
                format!(" {}", up_args[5..].join(" "))
            } else {
                String::new()
            }
        ),
    );
    let mut up_result = run_compose_streaming(Arc::clone(&sink), up_args.clone()).await;

    // `up` only fails like this when a leftover container is holding one of our
    // fixed container_names and compose can't reconcile it — e.g. one created
    // by an older version under a different project label, which surfaces as a
    // name conflict rather than a container to recreate. Reap the known names
    // (and image orphans) once, then retry. This runs ONLY on failure, so a
    // normal start never force-removes anything.
    if let Err(e) = &up_result {
        sink.log(
            "ERROR",
            &format!(
                "docker compose up failed ({e}); reaping leftover containers and retrying"
            ),
        );
        force_remove_known_containers(Arc::clone(&sink)).await;
        sweep_orphan_node_containers(Arc::clone(&sink)).await;
        up_result = run_compose_streaming(Arc::clone(&sink), up_args).await;
    }

    // (10) Confirm the dashboard can actually authenticate to Postgres. A stale
    // or foreign data volume keeps an old password and would otherwise leave
    // the dashboard crash-looping behind a silent 502.
    if up_result.is_ok() {
        verify_dashboard_db(Arc::clone(&sink)).await;
    }
    up_result
}

/// Result of probing the dashboard's Postgres credentials against the live
/// volume.
enum PgAuthProbe {
    /// The current password authenticated successfully.
    Ok,
    /// Postgres rejected the current password (`28P01`) — the data volume was
    /// initialised with a different one.
    AuthFailed,
    /// Postgres never became ready in time, or failed for a non-auth reason.
    /// We don't raise the mismatch alarm in this case to avoid false positives.
    Inconclusive,
}

/// After the stack is up, confirm the dashboard's Postgres credentials match
/// the existing data volume. Postgres only applies `POSTGRES_PASSWORD` when it
/// first initialises a data dir, so a volume left over from another stack (or a
/// lost bootstrap.json) keeps its original password and the dashboard's startup
/// migration crash-loops with `28P01 password authentication failed` — which
/// the user only sees as a 502 behind Caddy. Surface it explicitly via the
/// `dashboard-db-mismatch` event instead. Non-fatal: the validator and miner
/// are unaffected, so we don't abort the whole start.
async fn verify_dashboard_db(sink: Arc<dyn ProgressSink>) {
    let password = crate::settings::postgres_password();
    let outcome = tokio::task::spawn_blocking(move || probe_postgres_auth(&password))
        .await
        .unwrap_or(PgAuthProbe::Inconclusive);
    match outcome {
        PgAuthProbe::Ok | PgAuthProbe::Inconclusive => {}
        PgAuthProbe::AuthFailed => {
            let msg = format!(
                "Dashboard database password mismatch: the existing `{PGDATA_VOLUME}` volume \
                 was initialised with a different password (e.g. by another Quip stack), so the \
                 dashboard can't start. Use \u{201c}Reset dashboard database\u{201d} on the \
                 Dashboard tab to recreate it."
            );
            sink.log("ERROR", &msg);
            sink.dashboard_db_mismatch(&msg);
        }
    }
}

/// Wait (bounded) for `quip-postgres` to accept connections, then attempt an
/// authenticated TCP query with the current password. TCP (`-h 127.0.0.1`)
/// exercises the same password path the dashboard uses, unlike the local socket
/// which the official image leaves as `trust`. `PGPASSWORD` is passed via the
/// environment so it never lands in logs or `ps`.
fn probe_postgres_auth(password: &str) -> PgAuthProbe {
    let mut ready = false;
    for _ in 0..30 {
        let r = crate::cmd::new("docker")
            .args([
                "exec",
                PG_CONTAINER,
                "pg_isready",
                "-U",
                PG_USER,
                "-d",
                PG_DB,
            ])
            .output();
        if matches!(r, Ok(ref o) if o.status.success()) {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if !ready {
        return PgAuthProbe::Inconclusive;
    }

    let pgpass = format!("PGPASSWORD={password}");
    let output = crate::cmd::new("docker")
        .args([
            "exec",
            "-e",
            pgpass.as_str(),
            PG_CONTAINER,
            "psql",
            "-h",
            "127.0.0.1",
            "-U",
            PG_USER,
            "-d",
            PG_DB,
            "-tAc",
            "select 1",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => PgAuthProbe::Ok,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            if err.contains("password authentication failed") || err.contains("28P01") {
                PgAuthProbe::AuthFailed
            } else {
                PgAuthProbe::Inconclusive
            }
        }
        Err(_) => PgAuthProbe::Inconclusive,
    }
}

/// Delete the dashboard's database + indexer state. Used to clear stale
/// dashboard data (e.g. a cached node identity) or recover from a Postgres
/// password mismatch (see `verify_dashboard_db`).
///
/// This ONLY deletes data — it does not run `docker compose up` or start
/// anything. It force-removes the dashboard + Postgres containers (by their
/// fixed names) so the data volume is free, deletes that volume, and clears the
/// dashboard's bind-mounted data folder. The validator, miner and Caddy are
/// left as-is. The dashboard comes back, empty, on the next Start.
///
/// In v0.2 the dashboard keeps everything in Postgres (`quip_pgdata`) — chain
/// index, indexer state, and the cached `self_address`; the `dashboard-data`
/// folder is unused but cleared for good measure.
#[tauri::command]
pub async fn reset_dashboard_database(app: AppHandle) -> Result<(), String> {
    log_cmd(&app, "Resetting dashboard database");

    // Force-remove only the dashboard + Postgres containers (by fixed name) so
    // the data volume is free to delete. Best-effort: missing containers just
    // error per-name, which we ignore. Deliberately no `compose up`.
    log_cmd(&app, "docker rm -f quip-postgres quip-dashboard");
    let _ = tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args(["rm", "-f", PG_CONTAINER, "quip-dashboard"])
            .output()
    })
    .await;

    // Delete the Postgres data volume (the database + indexer state, including
    // the cached self identity).
    log_cmd(&app, &format!("docker volume rm {PGDATA_VOLUME}"));
    let rm = tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args(["volume", "rm", PGDATA_VOLUME])
            .output()
    })
    .await
    .map_err(|e| e.to_string())?;
    match rm {
        Ok(o) if o.status.success() => {}
        // "No such volume" means there's nothing to delete — fine.
        Ok(o) if String::from_utf8_lossy(&o.stderr).contains("No such volume") => {}
        Ok(o) => {
            return Err(format!(
                "failed to remove {PGDATA_VOLUME}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => return Err(format!("failed to remove {PGDATA_VOLUME}: {e}")),
    }

    // Clear the dashboard's bind-mounted data folder, then recreate it so the
    // mount target exists and stays host-owned on the next Start.
    let dash_data = crate::settings::data_dir().join("dashboard-data");
    if let Err(e) = std::fs::remove_dir_all(&dash_data) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log_err(
                &app,
                &format!("Warning: clearing {} failed: {e}", dash_data.display()),
            );
        }
    }
    let _ = std::fs::create_dir_all(&dash_data);

    log_output(
        &app,
        "Dashboard database cleared. Start the node to bring the dashboard back up.",
    );
    Ok(())
}

/// Stop the compose stack. Uses `docker compose stop`, which halts the
/// containers but leaves them — and the project network and named volumes
/// (quip-pgdata, quip-caddy-data, quip-caddy-config) — in place. Stop must not
/// destroy containers; recreating from a clean slate is the Start path's job
/// (`down` + name reap).
///
/// Every service in the upstream compose file is gated behind the `cpu`/`cuda`
/// profile, so `stop` MUST activate those profiles. Compose v2 reconciles
/// `stop`/`down`/`restart` against the *active* profile set; without
/// `--profile`, profile-gated services are absent from the model and `stop`
/// silently halts nothing (exit 0, no output). We activate both `cpu` and
/// `cuda` so Stop halts the whole stack regardless of the configured miner
/// type — including containers left over from the other profile after a switch.
#[tauri::command]
pub async fn stop_stack(app: AppHandle) -> Result<(), String> {
    // Kill the log-streamer child first — same ordering as the old
    // stop_node_container sequence, so `docker compose logs -f` unblocks
    // before we stop the containers.
    let log_state = app.state::<LogStreamState>();
    log_state.kill_child();
    *log_state.stop_flag.lock().unwrap() = true;

    stop_stack_core(Arc::new(TauriSink::new(app))).await
}

/// Stop the compose stack, reporting via `sink`.
///
/// Emits `stop-started`, runs `docker compose --profile cpu --profile cuda stop`
/// (v0.2 semantics: stops containers but does not remove them), then emits
/// `stop-complete`. Both cpu and cuda profiles are activated so the whole stack
/// is halted regardless of the configured miner type or containers left over
/// from a profile switch.
///
/// Args:
///     sink: Receives `stop-started`, `node-log`, and `stop-complete` events.
///
/// Returns:
///     `Ok(())` on success, `Err(message)` if the compose command fails.
pub(crate) async fn stop_stack_core(
    sink: Arc<dyn crate::progress::ProgressSink>,
) -> Result<(), String> {
    sink.stop_started();

    let stop_args: Vec<String> = vec![
        "--profile".into(),
        compose_profile(ImageTag::Cpu).into(),
        "--profile".into(),
        compose_profile(ImageTag::Cuda).into(),
        "stop".into(),
    ];
    sink.log("INFO", "$ docker compose --profile cpu --profile cuda stop");
    let result = run_compose_streaming(Arc::clone(&sink), stop_args).await;

    match &result {
        Ok(_) => {
            sink.log("INFO", "Compose stack stopped.");
            sink.stop_complete(true, None);
        }
        Err(e) => {
            for line in e.lines() {
                sink.log("ERROR", line);
            }
            sink.stop_complete(false, Some(e));
        }
    }

    result
}

/// Container names we force-remove as a Start-time cleanup backstop. The first
/// group is the fixed `container_name:` values from the current upstream
/// `docker-compose.yml` (independent of profile/run-mode). The trailing group
/// is legacy names the current compose file no longer declares, kept only so an
/// upgrade still reaps them.
const KNOWN_CONTAINER_NAMES: &[&str] = &[
    "quip-cpu",
    "quip-cuda",
    "quip-validator",
    "quip-dashboard",
    "quip-postgres",
    "quip-caddy",
    // Removed in v0.2 — the cpu/cuda miners self-bootstrap (faucet + miner +
    // descriptor registration), so the one-shot bootstrap container is gone.
    "quip-bootstrap",
    // Legacy one-container TUI path.
    "quip-node",
];

/// Force-remove every container the compose file declares by name. Runs
/// after `docker compose down` as a backstop — `down` has been observed
/// silently no-op-ing when the project label doesn't line up with what
/// we pass. `docker rm -f` on a missing name returns non-zero which we
/// ignore; we only surface output when something is actually removed.
async fn force_remove_known_containers(sink: Arc<dyn ProgressSink>) {
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
                sink.log("INFO", &format!("$ docker rm -f {name}"));
                sink.log("INFO", &format!("Removed {removed}"));
            }
        }
    }
}

/// Force-remove any containers running our node images whose name doesn't
/// start with `quip-` (i.e. anonymous / non-compose runners). Best-effort —
/// individual failures are logged but don't fail the stop. The name prefix
/// check is a sturdier stand-in for "lacks the compose project label"
/// since `docker ps --filter label!=…` isn't portable.
async fn sweep_orphan_node_containers(sink: Arc<dyn ProgressSink>) {
    // Match the tag the miner actually runs (channel-resolved, pinned in .env),
    // falling back to the compose default when .env hasn't been written yet.
    let tag = current_pinned_tag("QUIP_MINER_TAG").unwrap_or_else(|| COMPOSE_IMAGE_TAG.to_string());
    for image in &[CPU_IMAGE, CUDA_IMAGE] {
        let image_ref = format!("{image}:{tag}");
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
            sink.log(
                "INFO",
                &format!("$ docker rm -f {id}  # orphan {name} from {image_ref}"),
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

    // A genuinely empty stack reports no objects (empty stdout or `[]`). But if
    // compose printed something we couldn't parse, reporting an empty/Stopped
    // stack would be a lie — it masks a running stack and re-enables the Start
    // button. Surface the parse failure instead.
    if objects.is_empty() && !text.is_empty() && text != "[]" {
        return Err(format!(
            "could not parse `docker compose ps` output: {}",
            text.chars().take(200).collect::<String>()
        ));
    }

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

    let settings = crate::settings::load_settings();
    let expected = expected_services(&settings.run_mode, settings.image_tag);
    let overall = roll_up_health(&services, &expected);

    Ok(StackStatus { services, overall })
}

/// Roll per-service states up into a `StackHealth`, considering only the
/// `expected` services. Containers outside `expected` (stale miners from
/// another profile or run mode) are ignored; an expected service with no
/// container at all counts as not running.
fn roll_up_health(services: &[ServiceStatus], expected: &[&str]) -> StackHealth {
    let relevant: Vec<&ServiceStatus> = services
        .iter()
        .filter(|s| expected.contains(&s.service.as_str()))
        .collect();
    let running = relevant.iter().filter(|s| s.running).count();
    if running == 0 {
        return StackHealth::Stopped;
    }
    if relevant
        .iter()
        .any(|s| s.health.as_deref() == Some("unhealthy"))
    {
        return StackHealth::Unhealthy;
    }
    let all_ok = relevant.iter().all(|s| {
        s.running
            && s.health
                .as_deref()
                .map(|h| h == "healthy" || h == "starting")
                .unwrap_or(true)
    });
    if all_ok && running == expected.len() {
        StackHealth::Running
    } else {
        StackHealth::Degraded
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_prefix_omits_override_when_absent() {
        let args = compose_prefix_args("/d/docker-compose.yml", None, "/d");
        assert_eq!(
            args,
            vec![
                "compose",
                "-f",
                "/d/docker-compose.yml",
                "--project-directory",
                "/d",
                "--project-name",
                "quip"
            ]
        );
    }

    /// The override must follow the base file. Compose merges left to right, so
    /// reversing these silently reinstates the bundled defaults over the
    /// operator's changes.
    #[test]
    fn compose_prefix_puts_override_after_base_file() {
        let args = compose_prefix_args(
            "/d/docker-compose.yml",
            Some("/d/docker-compose.override.yml"),
            "/d",
        );
        let base = args.iter().position(|a| a == "/d/docker-compose.yml").unwrap();
        let over = args
            .iter()
            .position(|a| a == "/d/docker-compose.override.yml")
            .unwrap();
        assert!(base < over, "override must win: {args:?}");
        assert_eq!(args.iter().filter(|a| *a == "-f").count(), 2);
    }

    /// Same tag for all three images — most env tests don't exercise per-image
    /// differences.
    fn uniform_tags(tag: &str) -> ResolvedImageTags {
        ResolvedImageTags {
            miner: tag.to_string(),
            validator: tag.to_string(),
            dashboard: tag.to_string(),
        }
    }

    fn svc(service: &str, running: bool, health: Option<&str>) -> ServiceStatus {
        ServiceStatus {
            name: format!("quip-{service}"),
            service: service.to_string(),
            running,
            health: health.map(str::to_string),
            status_text: String::new(),
            image: String::new(),
        }
    }

    #[test]
    fn stale_exited_miner_does_not_degrade_health() {
        // A leftover Exited quip-cpu from before a cpu→cuda switch (or a
        // Docker→Native switch) must not drag the roll-up to Degraded.
        let services = vec![
            svc("cpu", false, None),
            svc("cuda", true, None),
            svc("quip-validator", true, None),
            svc("dashboard", true, Some("healthy")),
            svc("postgres", true, Some("healthy")),
            svc("caddy", true, None),
        ];
        let expected = expected_services(&RunMode::Docker, ImageTag::Cuda);
        assert_eq!(roll_up_health(&services, &expected), StackHealth::Running);

        // Native mode ignores every miner container.
        let expected = expected_services(&RunMode::Native, ImageTag::Cpu);
        assert_eq!(roll_up_health(&services, &expected), StackHealth::Running);
    }

    #[test]
    fn missing_expected_service_is_degraded() {
        // Dashboard container never created — the stack is not fully Running.
        let services = vec![
            svc("cpu", true, None),
            svc("quip-validator", true, None),
            svc("postgres", true, Some("healthy")),
            svc("caddy", true, None),
        ];
        let expected = expected_services(&RunMode::Docker, ImageTag::Cpu);
        assert_eq!(roll_up_health(&services, &expected), StackHealth::Degraded);
    }

    #[test]
    fn roll_up_health_terminal_states() {
        let expected = expected_services(&RunMode::Docker, ImageTag::Cpu);
        assert_eq!(roll_up_health(&[], &expected), StackHealth::Stopped);

        let stopped = vec![svc("cpu", false, None), svc("caddy", false, None)];
        assert_eq!(roll_up_health(&stopped, &expected), StackHealth::Stopped);

        let unhealthy = vec![
            svc("cpu", true, None),
            svc("quip-validator", true, None),
            svc("dashboard", true, Some("unhealthy")),
            svc("postgres", true, Some("healthy")),
            svc("caddy", true, None),
        ];
        assert_eq!(roll_up_health(&unhealthy, &expected), StackHealth::Unhealthy);
    }

    #[test]
    fn compose_profile_uses_only_v02_cpu_and_cuda_profiles() {
        assert_eq!(compose_profile(ImageTag::Cpu), "cpu");
        assert_eq!(compose_profile(ImageTag::Cuda), "cuda");
    }

    #[test]
    fn compose_services_docker_uses_profile_defaults() {
        assert!(compose_services(&RunMode::Docker).is_empty());
    }

    #[test]
    fn compose_services_native_runs_only_support_services() {
        let services = compose_services(&RunMode::Native);
        assert_eq!(
            services,
            ["quip-validator", "dashboard", "postgres", "caddy"]
        );
        assert!(!services.contains(&"cpu"));
        assert!(!services.contains(&"cuda"));
        assert!(!services.contains(&"quip-bootstrap"));
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
        assert_eq!(COMPOSE_IMAGE_TAG, "v0.2");
    }

    /// Unset must stay unset. Writing an explicit default here would fork the
    /// 16g default across .env and the compose file, so bumping one would
    /// silently leave the other behind.
    #[test]
    fn env_omits_miner_mem_limit_when_unset() {
        let settings = AppSettings {
            miner_mem_limit_gb: None,
            ..AppSettings::default()
        };
        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");
        assert!(!env.contains("QUIP_MINER_MEM_LIMIT"), "{env}");
    }

    #[test]
    fn env_writes_miner_mem_limit_in_gibibytes_when_set() {
        let settings = AppSettings {
            miner_mem_limit_gb: Some(48),
            ..AppSettings::default()
        };
        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");
        assert!(env.contains("QUIP_MINER_MEM_LIMIT=48g"), "{env}");
    }

    #[test]
    fn env_lines_use_v02_dashboard_and_validator_keys() {
        let mut settings = AppSettings {
            hostname: String::new(),
            cert_email: "ops@example.com".to_string(),
            zerossl_api_key: "zero".to_string(),
            run_mode: RunMode::Docker,
            ..AppSettings::default()
        };
        settings.node_config.node_name = "validator-home".to_string();
        settings.node_config.num_cpus = 4;

        let lines = render_env_lines(&settings, 501, 1000, "postgres-secret", &uniform_tags("v0.2"));
        let env = lines.join("\n");

        assert!(env.contains("PUID=501"));
        assert!(env.contains("PGID=1000"));
        assert!(env.contains("QUIP_HOSTNAME=:20049"));
        assert!(env.contains("CERT_EMAIL=ops@example.com"));
        assert!(env.contains("ZEROSSL_API_KEY=zero"));
        assert!(env.contains("POSTGRES_PASSWORD=postgres-secret"));
        assert!(env.contains("QUIP_MINER_TAG=v0.2"));
        assert!(env.contains("QUIP_DASHBOARD_TAG=v0.2"));
        assert!(env.contains("QUIP_VALIDATOR_TAG=v0.2"));
        assert!(env.contains("QUIP_MINER_CPUSET=0-3"));
        assert!(env.contains("VALIDATOR_NAME=validator-home"));
        // QUIP_VALIDATORS is no longer written — the miner is config-driven and
        // the upstream compose dropped it from the cpu/cuda env contract.
        assert!(!env.contains("QUIP_VALIDATORS"));
        // QUIP_VALIDATOR_RPC_URLS is deferred to the compose default
        // (ws://quip-caddy:8088/rpc), not written into .env.
        assert!(!env.contains("QUIP_VALIDATOR_RPC_URLS"));
        assert!(!env.contains("QUIP_NODE_URL"));
        assert!(!env.contains("QUIP_NODE_TOKEN"));
        assert!(!env.contains("QUIP_FAUCET_URL"));
    }

    #[test]
    fn env_lines_pin_all_image_tags_to_resolved_channel_tag() {
        let settings = AppSettings {
            run_mode: RunMode::Docker,
            ..AppSettings::default()
        };
        // Each image pins to its OWN channel-resolved tag — repos advance
        // independently, so the three QUIP_*_TAG lines can differ.
        let tags = ResolvedImageTags {
            miner: "v0.2.1-rc49".to_string(),
            validator: "v0.2.1-rc13".to_string(),
            dashboard: "v0.2.1-rc15".to_string(),
        };
        let env = render_env_lines(&settings, 501, 1000, "pg", &tags).join("\n");
        assert!(env.contains("QUIP_MINER_TAG=v0.2.1-rc49"));
        assert!(env.contains("QUIP_VALIDATOR_TAG=v0.2.1-rc13"));
        assert!(env.contains("QUIP_DASHBOARD_TAG=v0.2.1-rc15"));
    }

    #[test]
    fn env_lines_use_public_host_for_caddy_when_it_is_dns() {
        let mut settings = AppSettings::default();
        settings.node_config.public_host = "node.example.com".to_string();
        settings.hostname = "dashboard.example.com".to_string();

        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");

        assert!(env.contains("QUIP_HOSTNAME=node.example.com, node.example.com:20049"));
    }

    #[test]
    fn env_lines_fall_back_to_hostname_when_public_host_is_not_dns() {
        let mut settings = AppSettings::default();
        settings.node_config.public_host = "203.0.113.9".to_string();
        settings.hostname = "dashboard.example.com".to_string();

        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");

        assert!(env.contains("QUIP_HOSTNAME=dashboard.example.com"));
    }

    #[test]
    fn env_lines_preserve_dwave_key() {
        let mut settings = AppSettings::default();
        settings.node_config.dwave_config = Some(crate::settings::DwaveConfig {
            token: "dwave-token".to_string(),
            ..crate::settings::DwaveConfig::default()
        });

        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");
        assert!(env.contains("DWAVE_API_KEY=dwave-token"));
    }

    #[test]
    fn env_lines_native_omits_docker_miner_validator_env() {
        let mut settings = AppSettings {
            run_mode: RunMode::Native,
            ..AppSettings::default()
        };
        settings.node_config.node_name = "physical-miner-validator".to_string();

        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");

        assert!(!env.contains("QUIP_VALIDATORS="));
        // Deferred to the compose default in both modes (see above).
        assert!(!env.contains("QUIP_VALIDATOR_RPC_URLS"));
    }

    #[test]
    fn env_lines_default_validator_name_and_single_cpu_cpuset() {
        let settings = AppSettings::default();
        let env = render_env_lines(&settings, 501, 1000, "pg", &uniform_tags("v0.2")).join("\n");

        assert!(env.contains("VALIDATOR_NAME=quip-validator"));
        assert!(env.contains("QUIP_MINER_CPUSET=0"));
    }
}
