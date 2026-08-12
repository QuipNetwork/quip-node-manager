// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::VecDeque;
use std::io::Stdout;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::checklist::{CheckItem, CheckState};
use crate::log_stream::LogEntry;
use crate::settings::{AppSettings, DwaveConfig, ImageTag, RunMode, StackHealth};

// Compact status used by the TUI. The GUI exposes the full StackStatus shape.
#[derive(Clone, Debug)]
pub struct ContainerStatus {
    pub running: bool,
    pub container_id: Option<String>,
    pub image: String,
    pub status_text: String,
}

// ─── Focus IDs ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FocusId {
    StartNode,
    StopNode,
    ChecklistToggle,
    RunChecklist,
    CheckPort,
    ConfigToggle,
    RunMode,
    UpdateChannel,
    Port,
    ValidatorPort,
    SecretShow,
    SecretRegenerate,
    NodeName,
    CustomToggle,
    PublicHostEnable,
    PublicHostInput,
    PublicPortInput,
    CpuCores,
    GpuEnable,
    GpuUtilization,
    GpuYielding,
    QpuToggle,
    QpuApiKey,
    QpuDailyBudget,
    ApplyRestart,
    // Advanced (inside Custom Settings)
    LogLevel,
    NodeLog,
    HttpLog,
}

// ─── Edit mode ───────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum EditMode {
    None,
    EditingField(FocusId),
}

// ─── Actions returned from input handler ──────────────────────────────────────

pub enum Action {
    Quit,
    StartNode,
    StopNode,
    ApplyRestart,
    RegenerateSecret,
    ToggleSecretVisible,
    RunChecklist,
    CheckPort,
    ToggleLogs,
    None,
}

// ─── Form state ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FormState {
    pub port: String,
    pub validator_port: String,
    pub node_name: String,
    pub auto_mine: bool,
    pub run_mode_idx: usize,       // 0=Docker, 1=Native
    pub update_channel_idx: usize, // 0=Release, 1=Beta
    pub public_host_enabled: bool,
    pub public_host: String,
    pub public_port: String,
    pub peers: String,
    pub cpu_cores: String,
    pub gpu_utilization: u8,
    pub gpu_yielding: bool,
    pub qpu_api_key: String,
    pub qpu_daily_budget: String,
    // Advanced settings
    pub timeout: String,
    pub heartbeat_interval: String,
    pub heartbeat_timeout: String,
    pub fanout: String, // empty string = None
    pub verify_tls: bool,
    pub log_level: String,
    pub tls_cert_file: String,
    pub tls_key_file: String,
    pub rest_host: String,
    pub rest_port: String,
    pub rest_insecure_port: String,
    pub telemetry_enabled: bool,
    pub telemetry_dir: String,
    pub node_log: String,
    pub http_log: String,
    // NOTE: no `image_tag` field. It is derived from the GPU config by
    // `TuiApp::derive_image_tag` — storing it here let it go stale (QUI-895).
    /// Temporary buffer used while editing a text field.
    pub edit_buf: String,
}

impl FormState {
    pub fn from_settings(s: &AppSettings) -> Self {
        let nc = &s.node_config;
        let dw = match &nc.dwave_config {
            Some(q) => q.clone(),
            None => DwaveConfig::default(),
        };
        let first_gpu = nc
            .gpu_device_configs
            .iter()
            .find(|d| d.enabled)
            .or_else(|| nc.gpu_device_configs.first());
        let run_mode_idx = match s.run_mode {
            RunMode::Docker => 0,
            RunMode::Native => 1,
        };
        let update_channel_idx = match s.update_channel {
            crate::settings::UpdateChannel::Release => 0,
            crate::settings::UpdateChannel::Beta => 1,
        };
        FormState {
            port: nc.port.to_string(),
            validator_port: nc.validator_port.to_string(),
            node_name: nc.node_name.clone(),
            auto_mine: nc.auto_mine,
            run_mode_idx,
            update_channel_idx,
            public_host_enabled: !nc.public_host.is_empty() || nc.public_port.is_some(),
            public_host: nc.public_host.clone(),
            public_port: nc.public_port.map(|p| p.to_string()).unwrap_or_default(),
            peers: nc.peers.join("\n"),
            cpu_cores: nc.num_cpus.to_string(),
            gpu_utilization: first_gpu.map(|d| d.utilization).unwrap_or(80),
            gpu_yielding: first_gpu.map(|d| d.yielding).unwrap_or(false),
            qpu_api_key: dw.token,
            qpu_daily_budget: dw.daily_budget,
            timeout: nc.timeout.to_string(),
            heartbeat_interval: nc.heartbeat_interval.to_string(),
            heartbeat_timeout: nc.heartbeat_timeout.to_string(),
            fanout: nc.fanout.map(|f| f.to_string()).unwrap_or_default(),
            verify_tls: nc.verify_tls,
            log_level: nc.log_level.clone(),
            tls_cert_file: nc.tls_cert_file.clone(),
            tls_key_file: nc.tls_key_file.clone(),
            rest_host: nc.rest_host.clone(),
            rest_port: nc.rest_port.to_string(),
            rest_insecure_port: nc.rest_insecure_port.to_string(),
            telemetry_enabled: nc.telemetry_enabled,
            telemetry_dir: nc.telemetry_dir.clone(),
            node_log: nc.node_log.clone(),
            http_log: nc.http_log.clone(),
            edit_buf: String::new(),
        }
    }

    pub fn run_mode(&self) -> RunMode {
        if self.run_mode_idx == 1 {
            RunMode::Native
        } else {
            RunMode::Docker
        }
    }

    pub fn update_channel(&self) -> crate::settings::UpdateChannel {
        if self.update_channel_idx == 1 {
            crate::settings::UpdateChannel::Beta
        } else {
            crate::settings::UpdateChannel::Release
        }
    }

    pub fn to_node_config(
        &self,
        base: &crate::settings::NodeConfig,
    ) -> crate::settings::NodeConfig {
        let mut nc = base.clone();
        nc.port = self.port.parse().unwrap_or(20049);
        nc.validator_port = self.validator_port.parse().unwrap_or(30333);
        nc.node_name = self.node_name.clone();
        nc.auto_mine = self.auto_mine;
        nc.public_host = if self.public_host_enabled {
            self.public_host.clone()
        } else {
            String::new()
        };
        nc.public_port = if self.public_host_enabled {
            self.public_port.trim().parse().ok()
        } else {
            None
        };
        nc.peers = self
            .peers
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        nc.num_cpus = self.cpu_cores.parse().unwrap_or(1);
        // GPU: update utilization/yielding on existing device configs
        for d in &mut nc.gpu_device_configs {
            d.utilization = self.gpu_utilization;
            d.yielding = self.gpu_yielding;
        }
        let dwave_token = self.qpu_api_key.trim();
        nc.dwave_config = if dwave_token.is_empty() {
            None
        } else {
            Some(DwaveConfig {
                token: dwave_token.to_string(),
                solver: "Advantage2_System1.13".to_string(),
                dwave_region_url: "https://na-west-1.cloud.dwavesys.com/sapi/v2/".to_string(),
                daily_budget: self.qpu_daily_budget.clone(),
                qpu_min_blocks_for_estimation: None,
                qpu_ema_alpha: None,
            })
        };
        nc.timeout = self.timeout.parse().unwrap_or(3);
        nc.heartbeat_interval = self.heartbeat_interval.parse().unwrap_or(15);
        nc.heartbeat_timeout = self.heartbeat_timeout.parse().unwrap_or(300);
        nc.fanout = self.fanout.trim().parse().ok().filter(|&f: &u32| f > 0);
        nc.verify_tls = self.verify_tls;
        nc.log_level = self.log_level.clone();
        nc.tls_cert_file = self.tls_cert_file.clone();
        nc.tls_key_file = self.tls_key_file.clone();
        nc.rest_host = self.rest_host.clone();
        nc.rest_port = self.rest_port.parse().unwrap_or(-1);
        nc.rest_insecure_port = self.rest_insecure_port.parse().unwrap_or(-1);
        nc.telemetry_enabled = self.telemetry_enabled;
        nc.telemetry_dir = self.telemetry_dir.clone();
        nc.node_log = self.node_log.clone();
        nc.http_log = self.http_log.clone();
        nc
    }
}

// ─── App state ────────────────────────────────────────────────────────────────

pub struct TuiApp {
    pub focus: FocusId,
    pub edit_mode: EditMode,
    pub settings: AppSettings,
    pub dirty: bool,
    pub status: ContainerStatus,
    pub checks: Vec<CheckItem>,
    pub checklist_running: bool,
    checklist_rx: Option<mpsc::Receiver<Vec<CheckItem>>>,
    pub port_checking: bool,
    port_check_rx: Option<mpsc::Receiver<bool>>,
    pub log_rx: mpsc::Receiver<LogEntry>,
    #[allow(dead_code)] // Kept alive to prevent channel close
    log_tx: SyncSender<LogEntry>,
    pub log_buf: VecDeque<LogEntry>,
    pub log_stop: Arc<Mutex<bool>>,
    pub log_streaming: bool,
    pub log_expanded: bool,
    pub checklist_expanded: bool,
    pub config_expanded: bool,
    pub custom_expanded: bool,
    pub qpu_expanded: bool,
    pub form: FormState,
    pub node_secret: String,
    pub secret_visible: bool,
    pub status_message: Option<(String, Instant)>,
    pub scroll_offset: u16,
    pub content_height: u16,
    last_status_check: Instant,
    /// Shared native-process state; mirrors what the GUI holds in `tauri::State`.
    native_state: std::sync::Arc<crate::native::NativeProcessState>,
    /// Whether a stable (non-rc) release exists, fetched once in the background.
    /// `None` until the check returns; `Some(false)` grays out the Release
    /// channel and forces Beta (mirrors the web UI gating).
    channel_stable: Arc<Mutex<Option<bool>>>,
    /// GPU backend reported by the hardware survey ("cuda", "metal", "none", …).
    /// Retained because `GpuDeviceConfig` records only index/enabled and carries
    /// no vendor, so an enabled device alone cannot distinguish an NVIDIA card
    /// from an Apple Metal GPU. `derive_image_tag` needs both signals.
    gpu_backend: String,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiApp {
    pub fn new() -> Self {
        let mut settings = crate::settings::load_settings();
        let survey = crate::hardware::run_survey();
        merge_surveyed_gpus(&mut settings.node_config, &survey);
        let form = FormState::from_settings(&settings);
        let qpu_expanded = !form.qpu_api_key.is_empty();
        let (tx, rx) = mpsc::sync_channel(512);
        let secret = load_secret_sync();
        let log_stop = Arc::new(Mutex::new(false));
        // Start log streaming immediately so the bottom panel always has data
        {
            let stream_tx = tx.clone();
            let stream_stop = Arc::clone(&log_stop);
            crate::log_stream::start_log_stream_core(stream_tx, stream_stop);
        }
        // Fetch stable-release availability in the background; drives the
        // Release-channel gray-out without blocking startup.
        let channel_stable: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        {
            let slot = Arc::clone(&channel_stable);
            std::thread::spawn(move || {
                if let Ok(rt) = tokio::runtime::Runtime::new() {
                    let info = rt.block_on(crate::update::resolve_channel_info(
                        crate::settings::UpdateChannel::Beta,
                    ));
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(info.stable_available);
                    }
                }
            });
        }
        TuiApp {
            focus: FocusId::StartNode,
            edit_mode: EditMode::None,
            form,
            dirty: false,
            status: ContainerStatus {
                running: false,
                container_id: None,
                image: String::new(),
                status_text: "unknown".to_string(),
            },
            checks: vec![],
            checklist_running: false,
            checklist_rx: None,
            port_checking: false,
            port_check_rx: None,
            log_rx: rx,
            log_tx: tx,
            log_buf: VecDeque::with_capacity(500),
            log_stop,
            log_streaming: true,
            log_expanded: false,
            checklist_expanded: true,
            config_expanded: false,
            custom_expanded: false,
            qpu_expanded,
            node_secret: secret,
            secret_visible: false,
            status_message: None,
            scroll_offset: 0,
            content_height: 0,
            last_status_check: Instant::now() - Duration::from_secs(10),
            settings,
            native_state: std::sync::Arc::new(crate::native::NativeProcessState::new()),
            channel_stable,
            gpu_backend: survey.gpu_backend.clone(),
        }
    }

    /// Whether the Release channel is selectable. Unknown (check still running)
    /// counts as available so the default Release choice stays usable; only a
    /// confirmed `Some(false)` grays it out and forces Beta.
    pub fn release_channel_available(&self) -> bool {
        self.channel_stable
            .lock()
            .map(|g| g.unwrap_or(true))
            .unwrap_or(true)
    }

    /// The channel to persist: the form's selection, coerced to Beta when the
    /// Release channel has no stable build to point at.
    fn effective_update_channel(&self) -> crate::settings::UpdateChannel {
        match self.form.update_channel() {
            crate::settings::UpdateChannel::Release if !self.release_channel_available() => {
                crate::settings::UpdateChannel::Beta
            }
            other => other,
        }
    }

    // ─── Main event loop ──────────────────────────────────────────────────────

    pub fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> std::io::Result<()> {
        loop {
            if self.last_status_check.elapsed() > Duration::from_secs(5) {
                self.refresh_status();
                self.last_status_check = Instant::now();
            }
            self.drain_logs();
            self.poll_checklist();
            self.poll_port_check();
            self.expire_status_message();

            terminal.draw(|f| crate::tui_ui::render(f, self))?;

            if crossterm::event::poll(Duration::from_millis(50))? {
                let event = crossterm::event::read()?;
                let action = crate::tui_input::handle_event(self, event);
                match action {
                    Action::Quit => return Ok(()),
                    Action::StartNode => self.start_node(),
                    Action::StopNode => self.stop_node(),
                    Action::ApplyRestart => self.apply_and_restart(),
                    Action::RegenerateSecret => self.regenerate_secret(),
                    Action::ToggleSecretVisible => {
                        self.secret_visible = !self.secret_visible;
                    }
                    Action::RunChecklist => self.start_checklist(),
                    Action::CheckPort => self.start_port_check(),
                    Action::ToggleLogs => {
                        self.log_expanded = !self.log_expanded;
                    }
                    Action::None => {}
                }
            }
        }
    }

    // ─── Background task polling ──────────────────────────────────────────────

    fn drain_logs(&mut self) {
        while let Ok(entry) = self.log_rx.try_recv() {
            if self.log_buf.len() >= 500 {
                self.log_buf.pop_front();
            }
            self.log_buf.push_back(entry);
        }
    }

    fn poll_checklist(&mut self) {
        if let Some(rx) = &self.checklist_rx {
            if let Ok(checks) = rx.try_recv() {
                self.checks = checks;
                self.checklist_running = false;
                self.checklist_rx = None;
            }
        }
    }

    fn poll_port_check(&mut self) {
        if let Some(rx) = &self.port_check_rx {
            if let Ok(passed) = rx.try_recv() {
                // Update the port check item in the checks list.
                let port = self.settings.node_config.port;
                if let Some(item) = self.checks.iter_mut().find(|c| c.id == "port") {
                    item.state = if passed {
                        CheckState::Pass
                    } else {
                        CheckState::Warn
                    };
                    item.label = if passed {
                        format!("Public API port {} reachable", port)
                    } else {
                        format!("Public API port {} \u{2014} no TCP response", port)
                    };
                }
                self.port_checking = false;
                self.port_check_rx = None;
                let msg = if passed {
                    "Public API port is reachable"
                } else {
                    "No TCP response received"
                };
                self.set_status(msg);
            }
        }
    }

    fn start_port_check(&mut self) {
        if self.port_checking {
            return;
        }
        self.port_checking = true;
        let port = self.settings.node_config.port;
        // Show checking status immediately.
        if let Some(item) = self.checks.iter_mut().find(|c| c.id == "port") {
            item.state = CheckState::Running;
            item.label = format!("Public API port {} \u{2014} checking via public IP…", port);
        } else {
            self.checks.push(CheckItem {
                id: "port".to_string(),
                state: CheckState::Running,
                label: format!("Public API port {} \u{2014} checking via public IP…", port),
                detail: None,
                required: false,
                fixable: None,
                updated_at_ms: 0,
            });
        }
        self.set_status(format!("Checking public API port {} via public IP…", port));

        let (tx, rx) = mpsc::sync_channel::<bool>(1);
        self.port_check_rx = Some(rx);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let passed = rt.block_on(crate::checklist::probe_public_api_port_with_default_ip(
                port,
            ));
            let _ = tx.send(passed);
        });
    }

    fn expire_status_message(&mut self) {
        if let Some((_, ts)) = &self.status_message {
            if ts.elapsed() > Duration::from_secs(5) {
                self.status_message = None;
            }
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    // ─── Docker status ────────────────────────────────────────────────────────

    pub fn refresh_status(&mut self) {
        match self.form.run_mode() {
            RunMode::Native => {
                let pid_path = crate::settings::data_dir().join("node.pid");
                let running = if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
                    if let Ok(pid) = pid_str.trim().parse::<i32>() {
                        #[cfg(unix)]
                        {
                            unsafe { libc::kill(pid, 0) == 0 }
                        }
                        #[cfg(windows)]
                        {
                            true
                        } // Assume running if PID file exists on Windows
                    } else {
                        false
                    }
                } else {
                    false
                };
                self.status = ContainerStatus {
                    running,
                    container_id: None,
                    image: String::new(),
                    status_text: if running {
                        "running (native)".to_string()
                    } else {
                        "not running".to_string()
                    },
                };
            }
            RunMode::Docker => {
                self.status = self.stack_status_for_tui();
            }
        }
    }

    fn stack_status_for_tui(&self) -> ContainerStatus {
        let Ok(rt) = tokio::runtime::Runtime::new() else {
            return ContainerStatus {
                running: false,
                container_id: None,
                image: String::new(),
                status_text: "cannot create runtime".to_string(),
            };
        };
        let Ok(stack) = rt.block_on(crate::compose::get_stack_status()) else {
            return ContainerStatus {
                running: false,
                container_id: None,
                image: String::new(),
                status_text: "compose status unavailable".to_string(),
            };
        };

        let selected_service = self
            .derive_image_tag(&self.settings.node_config)
            .0
            .service();
        let selected = stack
            .services
            .iter()
            .find(|s| s.service == selected_service)
            .or_else(|| {
                stack
                    .services
                    .iter()
                    .find(|s| s.service == "quip-validator")
            })
            .or_else(|| stack.services.iter().find(|s| s.running))
            .or_else(|| stack.services.first());

        let running = matches!(stack.overall, StackHealth::Running | StackHealth::Degraded);
        let Some(service) = selected else {
            return ContainerStatus {
                running: false,
                container_id: None,
                image: String::new(),
                status_text: "not found".to_string(),
            };
        };

        ContainerStatus {
            running,
            container_id: Some(service.name.clone()),
            image: service.image.clone(),
            status_text: format!("{} ({:?})", service.status_text, stack.overall),
        }
    }

    // ─── Actions ──────────────────────────────────────────────────────────────

    /// Start the node stack for the currently selected run mode.
    ///
    /// In Docker mode: `start_stack_core` brings up the full compose stack.
    /// In Native mode: `start_stack_core` starts support services first
    /// (validator, dashboard, postgres, caddy), then `start_native_node_core`
    /// starts the host miner (which waits for the validator RPC).
    fn start_node(&mut self) {
        let mut settings = self.settings.clone();
        settings.node_config = self.form.to_node_config(&self.settings.node_config);
        let (image_tag, warning) = self.derive_image_tag(&settings.node_config);
        settings.image_tag = image_tag;
        if let Some(w) = warning {
            self.set_status(w);
        }
        settings.run_mode = self.form.run_mode();
        settings.update_channel = self.effective_update_channel();
        if let Err(e) = crate::settings::save_settings(&settings) {
            self.set_status(format!("Save error: {e}"));
            return;
        }
        let tx = self.log_tx.clone();
        let state = std::sync::Arc::clone(&self.native_state);
        let run_mode = settings.run_mode.clone();
        std::thread::spawn(move || {
            let sink: std::sync::Arc<dyn crate::progress::ProgressSink> =
                std::sync::Arc::new(crate::tui_sink::TuiSink::new(tx));
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let _ = rt.block_on(async {
                crate::compose::start_stack_core(std::sync::Arc::clone(&sink)).await?;
                if run_mode == crate::settings::RunMode::Native {
                    crate::native::start_native_node_core(sink, &state).await?;
                }
                Ok::<(), String>(())
            });
        });
        self.set_status("Starting…");
        self.config_expanded = false;
    }

    /// Stop the node stack for the currently selected run mode.
    ///
    /// In Native mode: host miner is stopped first, then compose support
    /// services. In Docker mode: compose stack is stopped directly.
    fn stop_node(&mut self) {
        let tx = self.log_tx.clone();
        let state = std::sync::Arc::clone(&self.native_state);
        let run_mode = self.form.run_mode();
        std::thread::spawn(move || {
            let sink: std::sync::Arc<dyn crate::progress::ProgressSink> =
                std::sync::Arc::new(crate::tui_sink::TuiSink::new(tx));
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let _ = rt.block_on(async {
                if run_mode == crate::settings::RunMode::Native {
                    // Best-effort: stop miner before tearing down support stack.
                    let _ =
                        crate::native::stop_native_node_core(std::sync::Arc::clone(&sink), &state)
                            .await;
                }
                crate::compose::stop_stack_core(sink).await?;
                Ok::<(), String>(())
            });
        });
        self.set_status("Stopping…");
        self.config_expanded = true;
        self.refresh_status();
    }

    /// Which miner image the compose stack should run, derived from the GPU
    /// configuration the user just edited.
    ///
    /// This MUST be derived rather than stored. `FormState.image_tag` used to
    /// hold it, but nothing ever wrote that field after `from_settings`, so
    /// toggling GPU-enable wrote `[gpu]`/`[cuda.0]` into config.toml while the
    /// compose profile stayed `cpu` — the miner ran the CPU image against a
    /// CUDA config (QUI-895).
    ///
    /// Inputs:
    /// - `config.gpu_device_configs[].enabled` — per-device toggle. Carries NO
    ///   vendor info; an enabled device may be NVIDIA or Apple Metal.
    /// - `self.gpu_backend` — survey string: "cuda", "metal", "none", …
    ///
    /// The GUI's equivalent lives at `src/app.js:713-715` and reads:
    ///     image_tag = (any device enabled && gpu_backend === 'cuda')
    ///                   ? 'cuda' : 'cpu'
    /// Note D-Wave/QPU is NOT a separate image — it activates via the
    /// config.toml `[dwave]` section (see `settings.rs:56-59`).
    /// Returns the tag plus an optional operator-facing warning. The warning is
    /// what keeps the CPU fallback from being silent: a user who ticked GPU and
    /// got a CPU miner must be told, since that mismatch is invisible in the
    /// config.toml they just wrote. Suppressed in Native mode, where no miner
    /// container runs at all and `image_tag` is therefore unused
    /// (`compose::compose_services` excludes cpu/cuda for `RunMode::Native`).
    fn derive_image_tag(&self, config: &crate::settings::NodeConfig) -> (ImageTag, Option<String>) {
        derive_image_tag(config, &self.gpu_backend, self.form.run_mode())
    }

    fn apply_and_restart(&mut self) {
        let config = self.form.to_node_config(&self.settings.node_config);
        self.settings.node_config = config;
        let (image_tag, warning) = self.derive_image_tag(&self.settings.node_config);
        self.settings.image_tag = image_tag;
        if let Some(w) = warning {
            self.set_status(w);
        }
        self.settings.run_mode = self.form.run_mode();
        self.settings.update_channel = self.effective_update_channel();
        if let Err(e) = crate::settings::save_settings(&self.settings) {
            self.set_status(format!("Save error: {}", e));
            return;
        }
        self.dirty = false;
        if self.status.running {
            self.stop_node();
            self.start_node();
        } else {
            self.set_status("Settings saved");
        }
    }

    fn regenerate_secret(&mut self) {
        use rand::Rng;
        let bytes: Vec<u8> = (0..32).map(|_| rand::thread_rng().gen::<u8>()).collect();
        let secret = hex::encode(bytes);
        let path = crate::settings::data_dir().join("node-secret.json");
        let content = format!("{{\"secret\":\"{}\"}}", secret);
        if let Ok(()) = crate::settings::ensure_data_dir() {
            if std::fs::write(&path, content).is_ok() {
                self.node_secret = secret;
                self.set_status("New secret generated");
            }
        }
    }

    fn start_checklist(&mut self) {
        if self.checklist_running {
            return;
        }
        self.checklist_running = true;
        self.checks = vec![];
        let (tx, rx) = mpsc::sync_channel::<Vec<CheckItem>>(1);
        self.checklist_rx = Some(rx);
        let run_mode = self.form.run_mode();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let checks = rt.block_on(crate::checklist::run_all_checks(&run_mode));
            let _ = tx.send(checks);
        });
    }

    // ─── Navigation helpers ───────────────────────────────────────────────────

    /// Returns the ordered list of focusable elements given current expand state.
    pub fn focus_list(&self) -> Vec<FocusId> {
        let mut list = vec![
            FocusId::StartNode,
            FocusId::StopNode,
            FocusId::ChecklistToggle,
        ];
        if self.checklist_expanded {
            list.push(FocusId::CheckPort);
            list.push(FocusId::RunChecklist);
        }
        list.push(FocusId::ConfigToggle);
        if self.config_expanded {
            list.push(FocusId::RunMode);
            list.push(FocusId::UpdateChannel);
            list.push(FocusId::Port);
            list.push(FocusId::ValidatorPort);
            list.push(FocusId::SecretShow);
            list.push(FocusId::SecretRegenerate);
            list.push(FocusId::NodeName);
            list.push(FocusId::CustomToggle);
            if self.custom_expanded {
                list.push(FocusId::PublicHostEnable);
                if self.form.public_host_enabled {
                    list.push(FocusId::PublicHostInput);
                    list.push(FocusId::PublicPortInput);
                }
                list.push(FocusId::LogLevel);
                list.push(FocusId::NodeLog);
                list.push(FocusId::HttpLog);
            }
            list.push(FocusId::CpuCores);
            list.push(FocusId::GpuEnable);
            if !self.settings.node_config.gpu_device_configs.is_empty() {
                list.push(FocusId::GpuUtilization);
                list.push(FocusId::GpuYielding);
            }
            list.push(FocusId::QpuToggle);
            if self.qpu_expanded {
                list.push(FocusId::QpuApiKey);
                list.push(FocusId::QpuDailyBudget);
            }
            list.push(FocusId::ApplyRestart);
        }
        list
    }

    pub fn next_focus(&mut self) {
        let list = self.focus_list();
        if let Some(pos) = list.iter().position(|f| *f == self.focus) {
            self.focus = list[(pos + 1) % list.len()].clone();
        } else if !list.is_empty() {
            self.focus = list[0].clone();
        }
    }

    pub fn prev_focus(&mut self) {
        let list = self.focus_list();
        if let Some(pos) = list.iter().position(|f| *f == self.focus) {
            self.focus = list[(pos + list.len() - 1) % list.len()].clone();
        } else if !list.is_empty() {
            self.focus = list[list.len() - 1].clone();
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn load_secret_sync() -> String {
    let path = crate::settings::data_dir().join("node-secret.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return String::new();
    };
    v["secret"].as_str().unwrap_or("").to_string()
}

/// Pure form of `TuiApp::derive_image_tag`, split out so the decision table can
/// be tested without standing up a whole `TuiApp` (which spawns threads and
/// reads settings from disk). See the method for the full rationale.
fn derive_image_tag(
    config: &crate::settings::NodeConfig,
    gpu_backend: &str,
    run_mode: RunMode,
) -> (ImageTag, Option<String>) {
    if !config.gpu_device_configs.iter().any(|d| d.enabled) {
        return (ImageTag::Cpu, None);
    }
    if gpu_backend == "cuda" {
        return (ImageTag::Cuda, None);
    }
    if run_mode == RunMode::Native {
        return (ImageTag::Cpu, None);
    }
    (
        ImageTag::Cpu,
        Some(format!(
            "GPU enabled but no CUDA backend detected (gpu_backend={}) \
             — starting the CPU image; config.toml still declares [cuda.*]",
            if gpu_backend.is_empty() {
                "unknown"
            } else {
                gpu_backend
            }
        )),
    )
}

/// Add a GpuDeviceConfig for each surveyed device missing from `node_config`,
/// preserving any saved enabled/utilization/yielding for existing indices.
fn merge_surveyed_gpus(
    node_config: &mut crate::settings::NodeConfig,
    survey: &crate::hardware::HardwareSurvey,
) {
    for dev in &survey.gpu_devices {
        if !node_config
            .gpu_device_configs
            .iter()
            .any(|c| c.index == dev.index)
        {
            node_config
                .gpu_device_configs
                .push(crate::settings::GpuDeviceConfig {
                    index: dev.index,
                    enabled: false,
                    utilization: 80,
                    yielding: false,
                });
        }
    }
    node_config.gpu_device_configs.sort_by_key(|c| c.index);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NodeConfig with `n` GPU devices, all set to `enabled`.
    fn nc_with_gpus(n: u32, enabled: bool) -> crate::settings::NodeConfig {
        crate::settings::NodeConfig {
            gpu_device_configs: (0..n)
                .map(|index| crate::settings::GpuDeviceConfig {
                    index,
                    enabled,
                    utilization: 80,
                    yielding: false,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// QUI-895: the reported failure. Enabling a GPU on a CUDA host must select
    /// the cuda image — previously `image_tag` stayed at its startup value, so
    /// config.toml got [cuda.0] while compose ran `--profile cpu`.
    #[test]
    fn enabled_gpu_on_cuda_host_selects_cuda_image() {
        let (tag, warn) = derive_image_tag(&nc_with_gpus(1, true), "cuda", RunMode::Docker);
        assert_eq!(tag, ImageTag::Cuda);
        assert!(warn.is_none(), "no warning expected on the happy path");
    }

    #[test]
    fn no_enabled_gpu_selects_cpu_image_without_warning() {
        let (tag, warn) = derive_image_tag(&nc_with_gpus(2, false), "cuda", RunMode::Docker);
        assert_eq!(tag, ImageTag::Cpu);
        assert!(warn.is_none(), "plain CPU mining is not a fallback");
    }

    /// Silent fallback is not acceptable: a user who ticked GPU and gets the CPU
    /// image must be told, because config.toml still declares [cuda.*] and the
    /// mismatch is otherwise invisible.
    #[test]
    fn enabled_gpu_without_cuda_backend_falls_back_loudly() {
        for backend in ["", "none", "metal"] {
            let (tag, warn) = derive_image_tag(&nc_with_gpus(1, true), backend, RunMode::Docker);
            assert_eq!(tag, ImageTag::Cpu, "backend={backend}");
            let warn = warn.unwrap_or_else(|| panic!("expected a warning for backend={backend}"));
            assert!(warn.contains("CPU image"), "backend={backend}: {warn}");
        }
        // The empty survey string is rendered as "unknown", not as a blank.
        let (_, warn) = derive_image_tag(&nc_with_gpus(1, true), "", RunMode::Docker);
        assert!(warn.unwrap().contains("gpu_backend=unknown"));
    }

    /// Native mode runs no miner container at all, so `image_tag` is unused and
    /// a macOS/Metal operator must not be nagged about a CPU fallback.
    #[test]
    fn native_mode_metal_falls_back_quietly() {
        let (tag, warn) = derive_image_tag(&nc_with_gpus(1, true), "metal", RunMode::Native);
        assert_eq!(tag, ImageTag::Cpu);
        assert!(warn.is_none(), "Native mode starts no cpu/cuda container");
    }

    /// Mirrors the GUI at src/app.js:713-715 — only devices that are actually
    /// enabled count, not merely present.
    #[test]
    fn one_enabled_device_among_several_is_enough() {
        let mut nc = nc_with_gpus(3, false);
        nc.gpu_device_configs[2].enabled = true;
        let (tag, _) = derive_image_tag(&nc, "cuda", RunMode::Docker);
        assert_eq!(tag, ImageTag::Cuda);
    }

    #[test]
    fn merge_surveyed_gpus_adds_detected_and_preserves_saved() {
        use crate::hardware::{GpuDevice, HardwareSurvey};
        let mut nc = crate::settings::NodeConfig {
            gpu_device_configs: vec![crate::settings::GpuDeviceConfig {
                index: 0,
                enabled: true,
                utilization: 55,
                yielding: true,
            }],
            ..Default::default()
        };
        let survey = HardwareSurvey {
            os: "linux".into(),
            arch: "x86_64".into(),
            cpu_count: 8,
            gpu_backend: "cuda".into(),
            gpu_devices: vec![
                GpuDevice {
                    index: 0,
                    name: "RTX 3060".into(),
                    memory_mb: Some(12288),
                },
                GpuDevice {
                    index: 1,
                    name: "RTX 3060".into(),
                    memory_mb: Some(12288),
                },
            ],
            docker_available: true,
            docker_version: None,
            python_available: false,
            python_version: None,
            recommended_mode: crate::settings::RunMode::Docker,
        };
        merge_surveyed_gpus(&mut nc, &survey);
        assert_eq!(nc.gpu_device_configs.len(), 2);
        assert!(nc.gpu_device_configs[0].enabled); // preserved
        assert_eq!(nc.gpu_device_configs[0].utilization, 55); // preserved
        assert!(!nc.gpu_device_configs[1].enabled); // new device defaults off
    }
}
