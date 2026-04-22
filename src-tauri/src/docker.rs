// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::log_stream::LogStreamState;
use crate::settings::{ContainerStatus, GpuBackend, RunMode};
use std::time::Duration;
use tauri::{Emitter, Manager};

fn log_cmd(app: &tauri::AppHandle, cmd: &str) {
    let entry = serde_json::json!({
        "timestamp": "",
        "level": "INFO",
        "message": format!("$ {}", cmd),
    });
    let _ = app.emit("node-log", entry);
}

fn log_output(app: &tauri::AppHandle, text: &str) {
    for line in text.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

fn log_err(app: &tauri::AppHandle, text: &str) {
    for line in text.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "ERROR",
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

const CPU_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu";
const CUDA_IMAGE: &str =
    "registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cuda";

pub fn image_for_tag(image_tag: &str) -> &'static str {
    if image_tag == "cuda" {
        CUDA_IMAGE
    } else {
        CPU_IMAGE
    }
}

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

/// Default timeout for `docker pull`. Large CUDA images can legitimately
/// take several minutes on slow links, but past 5 min we're almost always
/// stalled, not progressing.
const PULL_TIMEOUT: Duration = Duration::from_secs(300);

/// Stream `docker pull` stdout/stderr to the UI line by line, enforce a
/// timeout, and emit start/complete events so the frontend never sits on
/// a frozen "Pulling…" button.
#[tauri::command]
pub async fn pull_node_image(
    app: tauri::AppHandle,
    image_tag: String,
) -> Result<String, String> {
    let image = format!("{}:latest", image_for_tag(&image_tag));
    log_cmd(&app, &format!("docker pull {}", image));
    let _ = app.emit(
        "pull-started",
        serde_json::json!({ "image": &image }),
    );

    let result = pull_streaming(&app, &image).await;

    match &result {
        Ok(()) => {
            let _ = app.emit(
                "pull-complete",
                serde_json::json!({
                    "image": &image,
                    "success": true,
                }),
            );
        }
        Err(err) => {
            log_err(&app, err);
            let _ = app.emit(
                "pull-complete",
                serde_json::json!({
                    "image": &image,
                    "success": false,
                    "error": err,
                }),
            );
        }
    }

    // Auto-recheck affected items regardless of outcome — a failed pull
    // still flips the image check from Running back to Fail, which is
    // exactly what the user needs to see.
    let rc_app = app.clone();
    tokio::spawn(async move {
        crate::checklist::trigger_recheck_auto(
            rc_app,
            vec!["image".into(), "version".into()],
        )
        .await;
    });

    result.map(|_| image)
}

async fn pull_streaming(
    app: &tauri::AppHandle,
    image: &str,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::Instant;

    let app = app.clone();
    let image = image.to_string();

    tokio::task::spawn_blocking(move || {
        let mut child = crate::cmd::new("docker")
            .args(["pull", &image])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let app_out = app.clone();
        let image_out = image.clone();
        let stdout_thread = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = app_out.emit(
                    "pull-progress",
                    serde_json::json!({
                        "image": &image_out,
                        "line": &line,
                    }),
                );
                let entry = serde_json::json!({
                    "timestamp": "",
                    "level": "INFO",
                    "message": &line,
                });
                let _ = app_out.emit("node-log", entry);
            }
        });

        let app_err = app.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut last = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let entry = serde_json::json!({
                    "timestamp": "",
                    "level": "ERROR",
                    "message": &line,
                });
                let _ = app_err.emit("node-log", entry);
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
                        Err(format!("docker pull failed: {}", stderr_tail))
                    } else {
                        Err(format!("docker pull exited with {}", status))
                    };
                }
                Ok(None) => {
                    if start.elapsed() > PULL_TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_thread.join();
                        let _ = stderr_thread.join();
                        return Err(format!(
                            "docker pull timed out after {}s",
                            PULL_TIMEOUT.as_secs()
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

/// Host uid/gid for the PUID/PGID env vars passed to the node container.
///
/// The node image's entrypoint (quip-protocol v0.1.7+) uses these to chown
/// `/data` so bind-mounted files are owned by the host user, not root.
///
/// Gids below 1000 are clamped up to 1000 to sidestep a collision with
/// Alpine's system groups: macOS users default to gid=20 (staff), but
/// gid=20 is already taken by the `games` group inside the node image,
/// and the entrypoint's `groupmod -g $PGID quip` fails without `-o`.
/// Keeping the real uid preserves host-side file ownership; the gid
/// just won't have a friendly name (cosmetic).
///
/// On Windows, Docker Desktop's VM has no meaningful mapping to Windows
/// users — 1000:1000 matches the compose recipe's default.
fn host_uid_gid() -> (u32, u32) {
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

#[tauri::command]
pub async fn start_node_container(
    app: tauri::AppHandle,
) -> Result<String, String> {
    let settings = crate::settings::load_settings();
    let mut config = settings.node_config;
    let image_tag = settings.image_tag;

    // Auto-detect public IP when no public_host is configured
    if config.public_host.is_empty() {
        if let Ok(ip) = crate::network::detect_public_ip().await {
            log_cmd(&app, &format!("Auto-detected public IP: {}", ip));
            config.public_host = ip;
        }
    }

    // Write config.toml before starting
    log_cmd(&app, "Writing config.toml");
    crate::config::write_config_toml(&config, &RunMode::Docker)?;

    // Remove any stale container first
    log_cmd(&app, "docker rm -f quip-node");
    let rm_out = crate::cmd::new("docker")
        .args(["rm", "-f", "quip-node"])
        .output();
    if let Ok(o) = &rm_out {
        let stderr = String::from_utf8_lossy(&o.stderr);
        if !stderr.trim().is_empty()
            && !stderr.contains("No such container")
        {
            log_output(&app, stderr.trim());
        }
    }

    // Always pull latest image before starting (cache-bust :latest)
    pull_node_image(app.clone(), image_tag.clone()).await?;

    // Honor the user's custom storage directory (bootstrap.json).
    // Ensure it exists before Docker tries to bind-mount it, otherwise
    // the daemon silently creates a root-owned directory on the host.
    crate::settings::ensure_data_dir()?;
    let data_dir = crate::settings::data_dir();
    let data_mount = format!("{}:/data", data_dir.display());
    let image = format!("{}:latest", image_for_tag(&image_tag));

    let enabled_devices: Vec<u32> = config
        .gpu_device_configs
        .iter()
        .filter(|d| d.enabled)
        .map(|d| d.index)
        .collect();

    // MPS (Metal) is not accessible from Docker — runs in a Linux VM.
    let use_gpu = config.gpu_backend == GpuBackend::Local
        && !enabled_devices.is_empty()
        && image_tag == "cuda";

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        "quip-node".to_string(),
        "-p".to_string(),
        format!("{}:{}/udp", config.port, config.port),
        "-p".to_string(),
        format!("{}:{}/tcp", config.port, config.port),
        "-v".to_string(),
        data_mount,
    ];

    // Pass host uid/gid so the entrypoint chowns /data to the host user.
    // Matches the compose recipe's PUID/PGID env scheme (nodes.quip.network).
    let (puid, pgid) = host_uid_gid();
    args.push("-e".to_string());
    args.push(format!("PUID={}", puid));
    args.push("-e".to_string());
    args.push(format!("PGID={}", pgid));

    if !config.public_host.is_empty() {
        args.push("-e".to_string());
        args.push(format!(
            "QUIP_PUBLIC_HOST={}",
            config.public_host
        ));
    }
    if !config.node_name.is_empty() {
        args.push("-e".to_string());
        args.push(format!("QUIP_NODE_NAME={}", config.node_name));
    }

    if use_gpu {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    args.push(image);

    log_cmd(&app, &format!("docker {}", args.join(" ")));

    let output = crate::cmd::new("docker")
        .args(&args)
        .output()
        .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        let cid = stdout.trim();
        log_output(
            &app,
            &format!("Container started: {}", &cid[..12.min(cid.len())]),
        );
        Ok(cid.to_string())
    } else {
        log_err(&app, stderr.trim());
        Err(stderr.trim().to_string())
    }
}

/// Grace period passed to `docker stop` (SIGTERM → SIGKILL after this).
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Outer deadline for `docker stop` to return. Deliberately just above the
/// grace period so we escalate quickly if the daemon itself is wedged.
const STOP_DEADLINE: Duration = Duration::from_secs(12);

/// Stop and remove the quip-node container, with:
/// 1. Log-streamer child killed first (unblocks BufReader::lines)
/// 2. `docker stop -t 10` with an outer 12s deadline
/// 3. Post-stop inspection; escalate to `docker kill` if still running
/// 4. `docker rm -f` + final existence check
/// 5. `stop-complete` event with success flag for the UI
#[tauri::command]
pub async fn stop_node_container(
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _ = app.emit("stop-started", serde_json::json!({}));

    // (1) Kill the log-streamer child first. This releases the container's
    // log handle immediately; without it, `docker stop` can block waiting
    // for its own log pipe to be drained, and the BufReader on our side
    // sits on a blocking read.
    let log_state = app.state::<LogStreamState>();
    log_state.kill_child();
    *log_state.stop_flag.lock().unwrap() = true;

    // (2) Graceful stop with outer deadline. We don't propagate a failure
    // here — if `docker stop` times out or errors, we fall through to the
    // kill/rm escalation below.
    log_cmd(&app, &format!("docker stop -t {} quip-node", STOP_GRACE.as_secs()));
    let grace_secs = STOP_GRACE.as_secs().to_string();
    let stop_fut = tokio::task::spawn_blocking(move || {
        crate::cmd::new("docker")
            .args(["stop", "-t", &grace_secs, "quip-node"])
            .output()
    });
    match tokio::time::timeout(STOP_DEADLINE, stop_fut).await {
        Ok(Ok(Ok(o))) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.trim().is_empty() {
                log_err(&app, stderr.trim());
            }
        }
        Ok(Ok(Err(e))) => log_err(&app, &format!("docker stop error: {}", e)),
        Err(_) => log_err(&app, "docker stop did not return within deadline"),
        _ => {}
    }

    // (3) Verify. If still running, escalate.
    if is_container_running().await {
        log_err(
            &app,
            "container still running after docker stop — escalating to docker kill",
        );
        log_cmd(&app, "docker kill quip-node");
        let _ = tokio::task::spawn_blocking(|| {
            crate::cmd::new("docker")
                .args(["kill", "quip-node"])
                .output()
        })
        .await;
    }

    // (4) Force-remove. Handles "exited but still present" too.
    log_cmd(&app, "docker rm -f quip-node");
    let _ = tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args(["rm", "-f", "quip-node"])
            .output()
    })
    .await;

    // (5) Final verification.
    let success = !container_exists().await;

    if success {
        log_output(&app, "Container removed.");
        let _ = app.emit(
            "stop-complete",
            serde_json::json!({ "success": true }),
        );
        let _ = app.emit(
            "container-status",
            serde_json::json!({
                "running": false,
                "container_id": serde_json::Value::Null,
                "image": "",
                "status_text": "not found",
            }),
        );

        // Follow-on recheck. Container state doesn't directly affect any
        // check today, but the hook is useful (e.g. image is unchanged,
        // version is unchanged — fast no-op) and it exercises the same
        // recheck path so users see a consistent console trace.
        let rc_app = app.clone();
        tokio::spawn(async move {
            crate::checklist::trigger_recheck_auto(rc_app, vec!["image".into()]).await;
        });

        Ok(())
    } else {
        let msg = "container still present after rm -f — manual cleanup may be needed";
        log_err(&app, msg);
        let _ = app.emit(
            "stop-complete",
            serde_json::json!({ "success": false, "error": msg }),
        );
        Err(msg.to_string())
    }
}

async fn is_container_running() -> bool {
    match tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args(["inspect", "--format", "{{.State.Running}}", "quip-node"])
            .output()
    })
    .await
    {
        Ok(Ok(o)) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim() == "true"
        }
        _ => false,
    }
}

async fn container_exists() -> bool {
    match tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args(["ps", "-a", "--filter", "name=quip-node", "-q"])
            .output()
    })
    .await
    {
        Ok(Ok(o)) => !o.stdout.is_empty(),
        _ => false,
    }
}

#[tauri::command]
pub async fn get_container_status() -> Result<ContainerStatus, String> {
    let output = tokio::task::spawn_blocking(|| {
        crate::cmd::new("docker")
            .args([
                "inspect",
                "--format",
                "{{.Id}}\t{{.State.Running}}\t{{.Config.Image}}\t{{.State.Status}}",
                "quip-node",
            ])
            .output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Ok(ContainerStatus {
            running: false,
            container_id: None,
            image: String::new(),
            status_text: "not found".to_string(),
        });
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = line.trim().split('\t').collect();
    if parts.len() >= 4 {
        Ok(ContainerStatus {
            running: parts[1] == "true",
            container_id: Some(
                parts[0][..12.min(parts[0].len())].to_string(),
            ),
            image: parts[2].to_string(),
            status_text: parts[3].to_string(),
        })
    } else {
        Ok(ContainerStatus {
            running: false,
            container_id: None,
            image: String::new(),
            status_text: "unknown".to_string(),
        })
    }
}

#[tauri::command]
pub async fn get_container_config() -> Result<String, String> {
    let output = crate::cmd::new("docker")
        .args(["inspect", "quip-node"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}
