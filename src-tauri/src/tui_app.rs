// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::VecDeque;
use std::io::Stdout;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::checklist::{CheckItem, CheckState};
use crate::log_stream::LogEntry;
use crate::settings::{AppSettings, ContainerStatus, DwaveConfig, RunMode};

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
    Port,
    SecretShow,
    SecretRegenerate,
    AutoMine,
    NodeName,
    CustomToggle,
    PublicHostEnable,
    PublicHostInput,
    PublicPortInput,
    Peers,
    CpuCores,
    GpuEnable,
    GpuUtilization,
    GpuYielding,
    QpuToggle,
    QpuApiKey,
    QpuDailyBudget,
    ApplyRestart,
    AutoUpdate,
    // Advanced (inside Custom Settings)
    Timeout,
    HeartbeatInterval,
    HeartbeatTimeout,
    Fanout,
    VerifyTls,
    LogLevel,
    TlsCertFile,
    TlsKeyFile,
    RestHost,
    RestPort,
    RestInsecurePort,
    TelemetryEnabled,
    TelemetryDir,
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
    pub node_name: String,
    pub auto_mine: bool,
    pub run_mode_idx: usize, // 0=Docker, 1=Native
    pub public_host_enabled: bool,
    pub public_host: String,
    pub public_port: String,
    pub peers: String,
    pub cpu_cores: String,
    pub gpu_utilization: u8,
    pub gpu_yielding: bool,
    pub qpu_enabled: bool,
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
    pub auto_update: bool,
    // Image selector
    pub image_tag: String, // "cpu" or "cuda"
    /// Temporary buffer used while editing a text field.
    pub edit_buf: String,
}

impl FormState {
    pub fn from_settings(s: &AppSettings) -> Self {
        let nc = &s.node_config;
        let (qpu_enabled, dw) = match &nc.dwave_config {
            Some(q) => (true, q.clone()),
            None => (false, DwaveConfig::default()),
        };
        let first_gpu = nc.gpu_device_configs.iter().find(|d| d.enabled)
            .or_else(|| nc.gpu_device_configs.first());
        let run_mode_idx = match s.run_mode {
            RunMode::Docker => 0,
            RunMode::Native => 1,
        };
        FormState {
            port: nc.port.to_string(),
            node_name: nc.node_name.clone(),
            auto_mine: nc.auto_mine,
            run_mode_idx,
            public_host_enabled: !nc.public_host.is_empty()
                || nc.public_port.is_some(),
            public_host: nc.public_host.clone(),
            public_port: nc.public_port
                .map(|p| p.to_string())
                .unwrap_or_default(),
            peers: nc.peers.join("\n"),
            cpu_cores: nc.num_cpus.to_string(),
            gpu_utilization: first_gpu.map(|d| d.utilization).unwrap_or(80),
            gpu_yielding: first_gpu.map(|d| d.yielding).unwrap_or(false),
            qpu_enabled,
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
            auto_update: s.auto_update_enabled,
            image_tag: s.image_tag.clone(),
            edit_buf: String::new(),
        }
    }

    pub fn run_mode(&self) -> RunMode {
        if self.run_mode_idx == 1 { RunMode::Native } else { RunMode::Docker }
    }

    pub fn to_node_config(&self, base: &crate::settings::NodeConfig) -> crate::settings::NodeConfig {
        let mut nc = base.clone();
        nc.port = self.port.parse().unwrap_or(20049);
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
        nc.dwave_config = if self.qpu_enabled {
            Some(DwaveConfig {
                token: self.qpu_api_key.clone(),
                solver: "Advantage2_System1.13".to_string(),
                dwave_region_url: "https://na-west-1.cloud.dwavesys.com/sapi/v2/".to_string(),
                daily_budget: self.qpu_daily_budget.clone(),
                qpu_min_blocks_for_estimation: None,
                qpu_ema_alpha: None,
            })
        } else {
            None
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
}

impl TuiApp {
    pub fn new() -> Self {
        let settings = crate::settings::load_settings();
        let form = FormState::from_settings(&settings);
        let (tx, rx) = mpsc::sync_channel(512);
        let secret = load_secret_sync();
        let log_stop = Arc::new(Mutex::new(false));
        // Start log streaming immediately so the bottom panel always has data
        {
            let stream_tx = tx.clone();
            let stream_stop = Arc::clone(&log_stop);
            crate::log_stream::start_log_stream_core(stream_tx, stream_stop);
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
            qpu_expanded: false,
            node_secret: secret,
            secret_visible: false,
            status_message: None,
            scroll_offset: 0,
            content_height: 0,
            last_status_check: Instant::now() - Duration::from_secs(10),
            settings,
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
                    item.state = if passed { CheckState::Pass } else { CheckState::Warn };
                    item.label = if passed {
                        format!("Port {} forwarded", port)
                    } else {
                        format!("Port {} — no connection received", port)
                    };
                }
                self.port_checking = false;
                self.port_check_rx = None;
                let msg = if passed { "Port is reachable!" } else { "No connection received within 15s" };
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
            item.label = format!("Port {} — checking via public IP…", port);
        } else {
            self.checks.push(CheckItem {
                id: "port".to_string(),
                state: CheckState::Running,
                label: format!("Port {} — checking via public IP…", port),
                detail: None,
                required: false,
                fixable: None,
                updated_at_ms: 0,
            });
        }
        self.set_status(format!("Checking port {} via public IP…", port));

        let (tx, rx) = mpsc::sync_channel::<bool>(1);
        self.port_check_rx = Some(rx);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let passed = rt.block_on(
                crate::checklist::probe_port_forwarding_with_default_ip(port),
            );
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
                        { unsafe { libc::kill(pid, 0) == 0 } }
                        #[cfg(windows)]
                        { true } // Assume running if PID file exists on Windows
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
                    status_text: if running { "running (native)".to_string() } else { "not running".to_string() },
                };
            }
            RunMode::Docker => {
                let output = crate::cmd::new("docker")
                    .args([
                        "inspect",
                        "--format",
                        "{{.Id}}\t{{.State.Running}}\t{{.Config.Image}}\t{{.State.Status}}",
                        "quip-node",
                    ])
                    .output();
                self.status = match output {
                    Ok(o) if o.status.success() => {
                        let line = String::from_utf8_lossy(&o.stdout);
                        let parts: Vec<&str> = line.trim().split('\t').collect();
                        if parts.len() >= 4 {
                            ContainerStatus {
                                running: parts[1] == "true",
                                container_id: Some(
                                    parts[0][..12.min(parts[0].len())].to_string(),
                                ),
                                image: parts[2].to_string(),
                                status_text: parts[3].to_string(),
                            }
                        } else {
                            ContainerStatus {
                                running: false,
                                container_id: None,
                                image: String::new(),
                                status_text: "unknown".to_string(),
                            }
                        }
                    }
                    _ => ContainerStatus {
                        running: false,
                        container_id: None,
                        image: String::new(),
                        status_text: "not found".to_string(),
                    },
                };
            }
        }
    }

    // ─── Actions ──────────────────────────────────────────────────────────────

    fn start_node(&mut self) {
        let run_mode = self.form.run_mode();
        let mut config = self.form.to_node_config(&self.settings.node_config);

        // Auto-detect public IP when no public_host is configured
        if config.public_host.is_empty() {
            if let Ok(rt) = tokio::runtime::Runtime::new() {
                if let Ok(ip) = rt.block_on(crate::network::detect_public_ip()) {
                    self.set_status(format!("Auto-detected IP: {}", ip));
                    config.public_host = ip;
                }
            }
        }

        if let Err(e) = crate::config::write_config_toml(&config, &run_mode) {
            self.set_status(format!("Config error: {}", e));
            return;
        }

        match run_mode {
            RunMode::Native => self.start_node_native(&config),
            RunMode::Docker => self.start_node_docker(&config),
        }
        self.config_expanded = false;
    }

    fn start_node_docker(&mut self, config: &crate::settings::NodeConfig) {
        // Remove any stale container first
        let _ = crate::cmd::new("docker").args(["rm", "-f", "quip-node"]).output();

        let data_dir = crate::settings::data_dir();
        let data_mount = format!("{}:/data", data_dir.display());
        let image = format!(
            "{}:latest",
            crate::docker::image_for_tag(&self.settings.image_tag)
        );

        let quip_mode = if config.gpu_device_configs.iter().any(|d| d.enabled)
            && self.settings.image_tag == "cuda"
        {
            "gpu"
        } else {
            "cpu"
        };

        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            "quip-node".to_string(),
            "-p".to_string(),
            format!("{}:{}/udp", config.port, config.port),
            "-v".to_string(),
            data_mount,
            "-e".to_string(),
            format!("QUIP_MODE={}", quip_mode),
            "-e".to_string(),
            format!("QUIP_PORT={}", config.port),
            "-e".to_string(),
            format!("QUIP_LISTEN={}", config.listen),
            "-e".to_string(),
            format!("QUIP_AUTO_MINE={}", config.auto_mine),
        ];
        if !config.peers.is_empty() {
            args.push("-e".to_string());
            args.push(format!("QUIP_PEERS={}", config.peers.join(",")));
        }
        if !config.public_host.is_empty() {
            args.push("-e".to_string());
            args.push(format!("QUIP_PUBLIC_HOST={}", config.public_host));
        }
        if !config.node_name.is_empty() {
            args.push("-e".to_string());
            args.push(format!("QUIP_NODE_NAME={}", config.node_name));
        }
        if quip_mode == "gpu" {
            args.push("--gpus".to_string());
            args.push("all".to_string());
        }
        args.push(image);

        match crate::cmd::new("docker").args(&args).output() {
            Ok(o) if o.status.success() => {
                self.set_status("Node started (Docker)");
                self.refresh_status();
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                self.set_status(format!("Start failed: {}", err.trim()));
            }
            Err(e) => self.set_status(format!("Start failed: {}", e)),
        }
    }

    fn start_node_native(&mut self, _config: &crate::settings::NodeConfig) {
        if !crate::native::is_binary_available() {
            self.set_status("No native binary found. Download it first.");
            return;
        }
        // Use the async start via a blocking runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(async {
            let data_dir = crate::settings::data_dir();
            let bin = data_dir.join("bin").join(crate::native::binary_name());
            let config_path = data_dir.join("config.toml");
            let log_path = data_dir.join("node-output.log");
            let log_file = std::fs::File::create(&log_path)
                .map_err(|e| format!("Cannot create log file: {}", e))?;
            let log_err = log_file.try_clone()
                .map_err(|e| format!("Cannot clone log file: {}", e))?;
            let child = crate::cmd::new(&bin)
                .arg("--config")
                .arg(&config_path)
                .stdout(log_file)
                .stderr(log_err)
                .spawn()
                .map_err(|e| format!("Spawn failed: {}", e))?;
            let pid_path = data_dir.join("node.pid");
            let _ = std::fs::write(&pid_path, child.id().to_string());
            Ok::<(), String>(())
        }) {
            Ok(()) => {
                self.set_status("Node started (Native)");
                self.refresh_status();
            }
            Err(e) => self.set_status(format!("Start failed: {}", e)),
        }
    }

    fn stop_node(&mut self) {
        match self.form.run_mode() {
            RunMode::Docker => {
                let _ = crate::cmd::new("docker").args(["stop", "quip-node"]).output();
                let _ = crate::cmd::new("docker")
                    .args(["rm", "-f", "quip-node"])
                    .output();
            }
            RunMode::Native => {
                let pid_path = crate::settings::data_dir().join("node.pid");
                if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
                    if let Ok(pid) = pid_str.trim().parse::<i32>() {
                        #[cfg(unix)]
                        unsafe { libc::kill(-pid, libc::SIGTERM); }
                        #[cfg(windows)]
                        {
                            let _ = crate::cmd::new("taskkill")
                                .args(["/F", "/PID", &pid.to_string()])
                                .output();
                        }
                    }
                    let _ = std::fs::remove_file(&pid_path);
                }
            }
        }
        self.set_status("Node stopped");
        self.config_expanded = true;
        self.refresh_status();
    }

    fn apply_and_restart(&mut self) {
        let config = self.form.to_node_config(&self.settings.node_config);
        self.settings.node_config = config;
        self.settings.image_tag = self.form.image_tag.clone();
        self.settings.run_mode = self.form.run_mode();
        self.settings.auto_update_enabled = self.form.auto_update;
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
        let bytes: Vec<u8> =
            (0..32).map(|_| rand::thread_rng().gen::<u8>()).collect();
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
            list.push(FocusId::Port);
            list.push(FocusId::SecretShow);
            list.push(FocusId::SecretRegenerate);
            list.push(FocusId::AutoMine);
            list.push(FocusId::NodeName);
            list.push(FocusId::CustomToggle);
            if self.custom_expanded {
                list.push(FocusId::PublicHostEnable);
                if self.form.public_host_enabled {
                    list.push(FocusId::PublicHostInput);
                    list.push(FocusId::PublicPortInput);
                }
                list.push(FocusId::Peers);
                list.push(FocusId::Timeout);
                list.push(FocusId::HeartbeatInterval);
                list.push(FocusId::HeartbeatTimeout);
                list.push(FocusId::Fanout);
                list.push(FocusId::VerifyTls);
                list.push(FocusId::LogLevel);
                list.push(FocusId::TlsCertFile);
                list.push(FocusId::TlsKeyFile);
                list.push(FocusId::RestHost);
                list.push(FocusId::RestPort);
                list.push(FocusId::RestInsecurePort);
                list.push(FocusId::TelemetryEnabled);
                if self.form.telemetry_enabled {
                    list.push(FocusId::TelemetryDir);
                }
                list.push(FocusId::NodeLog);
                list.push(FocusId::HttpLog);
                list.push(FocusId::AutoUpdate);
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
