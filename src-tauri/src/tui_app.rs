// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::VecDeque;
use std::io::Stdout;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::checklist::CheckItem;
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
    Port,
    SecretShow,
    SecretRegenerate,
    AutoMine,
    NodeName,
    CustomToggle,
    PublicHostEnable,
    PublicHostInput,
    Peers,
    CpuCores,
    GpuEnable,
    GpuUtilization,
    GpuYielding,
    QpuToggle,
    QpuApiKey,
    QpuDailyBudget,
    ApplyRestart,
    ViewLogs,
    // Advanced (inside Custom Settings)
    Timeout,
    HeartbeatInterval,
    HeartbeatTimeout,
    Fanout,
    VerifySsl,
    LogLevel,
}

// ─── Screens / modes ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Main,
    Logs,
}

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
    EnterLogs,
    ExitLogs,
    None,
}

// ─── Form state ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FormState {
    pub port: String,
    pub node_name: String,
    pub auto_mine: bool,
    pub public_host_enabled: bool,
    pub public_host: String,
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
    pub verify_ssl: bool,
    pub log_level: String,
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
        FormState {
            port: nc.port.to_string(),
            node_name: nc.node_name.clone(),
            auto_mine: nc.auto_mine,
            public_host_enabled: !nc.public_host.is_empty(),
            public_host: nc.public_host.clone(),
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
            verify_ssl: nc.verify_tls,
            log_level: nc.log_level.clone(),
            image_tag: s.image_tag.clone(),
            edit_buf: String::new(),
        }
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
        nc.verify_tls = self.verify_ssl;
        nc.log_level = self.log_level.clone();
        nc
    }
}

// ─── App state ────────────────────────────────────────────────────────────────

pub struct TuiApp {
    pub screen: Screen,
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
    log_tx: SyncSender<LogEntry>,
    pub log_buf: VecDeque<LogEntry>,
    pub log_stop: Arc<Mutex<bool>>,
    pub log_streaming: bool,
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
        TuiApp {
            screen: Screen::Main,
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
            log_stop: Arc::new(Mutex::new(false)),
            log_streaming: false,
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
                    Action::EnterLogs => self.enter_logs(),
                    Action::ExitLogs => self.exit_logs(),
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
                    item.passed = passed;
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
            item.label = format!("Port {} — checking via public IP…", port);
        } else {
            // No checklist run yet — insert a placeholder.
            self.checks.push(crate::checklist::CheckItem {
                id: "port".to_string(),
                passed: false,
                label: format!("Port {} — checking via public IP…", port),
            });
        }
        self.set_status(format!("Checking port {} via public IP…", port));

        let (tx, rx) = mpsc::sync_channel::<bool>(1);
        self.port_check_rx = Some(rx);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let passed = rt.block_on(crate::checklist::probe_port_forwarding(port));
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
        use std::process::Command;
        let output = Command::new("docker")
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

    // ─── Actions ──────────────────────────────────────────────────────────────

    fn start_node(&mut self) {
        use std::process::Command;
        let config = self.form.to_node_config(&self.settings.node_config);
        if let Err(e) = crate::config::write_config_toml(&config, &RunMode::Docker) {
            self.set_status(format!("Config error: {}", e));
            return;
        }

        // Remove any stale container first
        let _ = Command::new("docker").args(["rm", "-f", "quip-node"]).output();

        let home = match dirs::home_dir() {
            Some(h) => h,
            None => {
                self.set_status("Cannot determine home directory");
                return;
            }
        };
        let data_mount = format!("{}/quip-data:/data", home.display());
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

        match Command::new("docker").args(&args).output() {
            Ok(o) if o.status.success() => {
                self.set_status("Node started");
                self.refresh_status();
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                self.set_status(format!("Start failed: {}", err.trim()));
            }
            Err(e) => self.set_status(format!("Start failed: {}", e)),
        }
    }

    fn stop_node(&mut self) {
        use std::process::Command;
        let _ = Command::new("docker").args(["stop", "quip-node"]).output();
        let _ = Command::new("docker")
            .args(["rm", "-f", "quip-node"])
            .output();
        self.set_status("Node stopped");
        self.refresh_status();
    }

    fn apply_and_restart(&mut self) {
        let config = self.form.to_node_config(&self.settings.node_config);
        self.settings.node_config = config;
        self.settings.image_tag = self.form.image_tag.clone();
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
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let checks = rt.block_on(crate::checklist::run_checklist_core(&RunMode::Docker, |_| {}));
            let _ = tx.send(checks);
        });
    }

    pub fn enter_logs(&mut self) {
        self.screen = Screen::Logs;
        if !self.log_streaming {
            *self.log_stop.lock().unwrap() = false;
            let tx = self.log_tx.clone();
            let stop = Arc::clone(&self.log_stop);
            crate::log_stream::start_log_stream_core(tx, stop);
            self.log_streaming = true;
        }
    }

    pub fn exit_logs(&mut self) {
        self.screen = Screen::Main;
        *self.log_stop.lock().unwrap() = true;
        self.log_streaming = false;
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
                }
                list.push(FocusId::Peers);
                list.push(FocusId::Timeout);
                list.push(FocusId::HeartbeatInterval);
                list.push(FocusId::HeartbeatTimeout);
                list.push(FocusId::Fanout);
                list.push(FocusId::VerifySsl);
                list.push(FocusId::LogLevel);
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
        list.push(FocusId::ViewLogs);
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
