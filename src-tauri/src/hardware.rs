// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::RunMode;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
    pub memory_mb: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct HardwareSurvey {
    pub os: String,
    pub arch: String,
    pub cpu_count: u32,
    pub gpu_backend: String,
    pub gpu_devices: Vec<GpuDevice>,
    pub docker_available: bool,
    pub docker_version: Option<String>,
    pub python_available: bool,
    pub python_version: Option<String>,
    pub recommended_mode: RunMode,
}

pub fn is_apple_silicon() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

pub fn list_nvidia_gpus() -> Vec<GpuDevice> {
    let output = crate::cmd::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(3, ',');
                    let index = parts.next()?.trim().parse().ok()?;
                    let name = parts.next()?.trim().to_string();
                    let mem: Option<u32> = parts
                        .next()
                        .and_then(|s| s.trim().parse().ok());
                    Some(GpuDevice {
                        index,
                        name,
                        memory_mb: mem,
                    })
                })
                .collect()
        }
        _ => vec![],
    }
}

fn detect_metal_gpu() -> Option<GpuDevice> {
    if !is_apple_silicon() {
        return None;
    }
    let output = crate::cmd::new("system_profiler")
        .args(["SPDisplaysDataType"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Chipset Model:") {
            let name = trimmed
                .trim_start_matches("Chipset Model:")
                .trim()
                .to_string();
            return Some(GpuDevice {
                index: 0,
                name,
                memory_mb: None,
            });
        }
    }
    None
}

fn detect_docker_version() -> Option<String> {
    let output = crate::cmd::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_string(),
        )
    } else {
        None
    }
}

fn detect_python() -> Option<String> {
    let output = crate::cmd::new("python3")
        .args(["--version"])
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        Some(
            text.trim()
                .trim_start_matches("Python ")
                .to_string(),
        )
    } else {
        None
    }
}

pub fn run_survey() -> HardwareSurvey {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
    .to_string();

    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
    .to_string();

    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    let nvidia_gpus = list_nvidia_gpus();
    let metal_gpu = detect_metal_gpu();

    let (gpu_backend, gpu_devices) = if !nvidia_gpus.is_empty() {
        ("cuda".to_string(), nvidia_gpus)
    } else if let Some(metal) = metal_gpu {
        ("metal".to_string(), vec![metal])
    } else {
        ("none".to_string(), vec![])
    };

    let docker_version = detect_docker_version();
    let docker_available = docker_version.is_some();
    let python_version = detect_python();
    let python_available = python_version.is_some();

    let recommended_mode = if cfg!(target_os = "macos") {
        RunMode::Native
    } else {
        RunMode::Docker
    };

    HardwareSurvey {
        os,
        arch,
        cpu_count,
        gpu_backend,
        gpu_devices,
        docker_available,
        docker_version,
        python_available,
        python_version,
        recommended_mode,
    }
}

#[tauri::command]
pub async fn detect_gpu_backend() -> Result<String, String> {
    let survey = run_survey();
    Ok(survey.gpu_backend)
}

#[tauri::command]
pub async fn list_gpu_devices() -> Result<Vec<GpuDevice>, String> {
    let survey = run_survey();
    Ok(survey.gpu_devices)
}

#[tauri::command]
pub async fn run_hardware_survey(
    app: tauri::AppHandle,
) -> Result<HardwareSurvey, String> {
    use tauri::Emitter;

    let survey = run_survey();

    let log = |msg: String| {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": "INFO",
            "message": msg,
        });
        let _ = app.emit("node-log", entry);
    };

    log(format!(
        "[Hardware Survey] OS: {} ({})",
        survey.os, survey.arch
    ));
    log(format!(
        "[Hardware Survey] CPUs: {} cores",
        survey.cpu_count
    ));
    if survey.gpu_devices.is_empty() {
        log("[Hardware Survey] GPU: none detected".to_string());
    } else {
        for dev in &survey.gpu_devices {
            let mem = dev
                .memory_mb
                .map(|m| format!(" ({} MB)", m))
                .unwrap_or_default();
            log(format!(
                "[Hardware Survey] GPU {}: {} ({}){}",
                dev.index, dev.name, survey.gpu_backend, mem
            ));
        }
    }
    match &survey.docker_version {
        Some(v) => log(format!("[Hardware Survey] Docker: v{}", v)),
        None => log(
            "[Hardware Survey] Docker: not installed".to_string(),
        ),
    }
    match &survey.python_version {
        Some(v) => log(format!("[Hardware Survey] Python: {}", v)),
        None => log(
            "[Hardware Survey] Python: not found".to_string(),
        ),
    }
    let mode_str = match survey.recommended_mode {
        RunMode::Docker => "Docker",
        RunMode::Native => "Native",
    };
    log(format!(
        "[Hardware Survey] Recommended mode: {}",
        mode_str
    ));

    Ok(survey)
}
