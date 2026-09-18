// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// ─── Run mode ───────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Docker,
    Native,
}

impl Default for RunMode {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            RunMode::Native
        } else {
            RunMode::Docker
        }
    }
}

// ─── Update channel ─────────────────────────────────────────────────────────

/// How fresh a build each image tracks. The manager writes this as `CHANNEL`
/// in `.env`, and also pins an exact resolved tag per image, which wins over
/// it.
///
/// - `Beta` → `CHANNEL=beta`, newest tag including `-rc`.
/// - `Release` → `CHANNEL=stable`, newest tag with no prerelease suffix.
///
/// Both join the same network. The app embeds one chain spec, so the network
/// is not a choice, and the faucet follows the spec rather than this enum
/// (see `stack_assets::FAUCET_URL`). Between the Aglais relaunch and the first
/// stable Aglais images this enum did name two networks, which put every
/// upgraded install on a validator that could not decode the new genesis.
///
/// The app's own self-update reads the same axis — Release takes stable
/// releases only, Beta also takes `-rc` builds — in `crate::update`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Release,
    /// The default, so a fresh install tracks release candidates until an
    /// operator opts into stable-only builds.
    #[default]
    Beta,
}

impl UpdateChannel {
    /// The `CHANNEL` value the compose file substitutes into every image
    /// reference: `image: <repo>:${QUIP_*_TAG:-${CHANNEL:-beta}}`. These are
    /// literal tag names, so they are lowercase and must match the moving tags
    /// each image publishes.
    pub fn compose_channel(self) -> &'static str {
        match self {
            UpdateChannel::Release => "stable",
            UpdateChannel::Beta => "beta",
        }
    }
}

// ─── GPU types ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GpuBackend {
    #[default]
    Local,
    Modal,
    Mps,
}

// ─── Image tag ──────────────────────────────────────────────────────────────

/// Which node image flavour the compose stack should run. Maps 1:1 to
/// compose `container_name`: Cpu → quip-cpu, Cuda → quip-cuda.
///
/// QPU is *not* a separate image — D-Wave mining activates via the
/// `[dwave]` section in config.toml on top of the CPU image, so the
/// operator's choice reduces to "do I have an NVIDIA GPU or not".
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageTag {
    #[default]
    Cpu,
    Cuda,
}

impl ImageTag {
    /// Compose service name (= container_name sans `quip-` prefix).
    pub fn service(&self) -> &'static str {
        match self {
            ImageTag::Cpu => "cpu",
            ImageTag::Cuda => "cuda",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GpuDeviceConfig {
    pub index: u32,
    pub enabled: bool,
    pub utilization: u8,
    pub yielding: bool,
}

impl Default for GpuDeviceConfig {
    fn default() -> Self {
        GpuDeviceConfig {
            index: 0,
            enabled: true,
            utilization: 80,
            yielding: false,
        }
    }
}

/// Apple Metal (MPS) tuning. A Mac exposes a single implicit GPU, so this
/// is a standalone config rather than a per-device list. Maps 1:1 to the
/// v0.2 `[metal]` section: `utilization`/`yielding` are shared GPU keys,
/// `active_util`/`idle_after_s` are Metal-only adaptive-cap knobs the
/// protocol keeps out of `[gpu]` inheritance.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MetalConfig {
    /// Idle/headless occupancy cap (1-100). Flat-out when nobody's present.
    pub utilization: u8,
    /// Enable the adaptive cap monitor (HID-idle / thermal / battery sensing).
    pub yielding: bool,
    /// Occupancy cap (%) while the user is present.
    pub active_util: u8,
    /// Seconds of no input before going idle/flat-out.
    pub idle_after_s: u32,
}

impl Default for MetalConfig {
    fn default() -> Self {
        MetalConfig {
            utilization: 100,
            yielding: true,
            active_util: 85,
            idle_after_s: 60,
        }
    }
}

// ─── QPU / D-Wave ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DwaveConfig {
    pub token: String,
    #[serde(default = "default_dwave_solver")]
    pub solver: String,
    #[serde(default = "default_dwave_region_url")]
    pub dwave_region_url: String,
    /// QPU access time per D-Wave quota period, as a duration such as "40h".
    /// Empty leaves the miner unmetered.
    #[serde(default)]
    pub budget: String,
    /// UTC day of the month (1-31) on which the quota period resets.
    #[serde(default = "default_budget_reset_day")]
    pub budget_reset_day: u8,
    #[serde(default)]
    pub qpu_min_blocks_for_estimation: Option<u32>,
    #[serde(default)]
    pub qpu_ema_alpha: Option<f64>,
}

fn default_dwave_solver() -> String {
    "Advantage2_System1.13".to_string()
}
fn default_dwave_region_url() -> String {
    "https://na-west-1.cloud.dwavesys.com/sapi/v2/".to_string()
}
fn default_budget_reset_day() -> u8 {
    1
}

impl Default for DwaveConfig {
    fn default() -> Self {
        DwaveConfig {
            token: String::new(),
            solver: default_dwave_solver(),
            dwave_region_url: default_dwave_region_url(),
            budget: String::new(),
            budget_reset_day: default_budget_reset_day(),
            qpu_min_blocks_for_estimation: None,
            qpu_ema_alpha: None,
        }
    }
}

impl DwaveConfig {
    /// Ocean SDK environment for the miner. The v0.3 miner takes no D-Wave
    /// credentials from config.toml; the SDK resolves them from these vars.
    /// Docker mode writes them to `.env`, native mode sets them on the child.
    pub fn sdk_env(&self) -> [(&'static str, String); 3] {
        [
            ("DWAVE_API_TOKEN", self.token.clone()),
            // Pin the solver. Left empty the SDK picks the account default,
            // which may not be the Advantage2 system the chain topology targets.
            ("DWAVE_API_SOLVER", self.solver.clone()),
            (
                "DWAVE_API_REGION",
                leap_region_from_url(&self.dwave_region_url),
            ),
        ]
    }
}

/// The Leap region name inside a D-Wave SAPI endpoint, e.g. `na-west-1` from
/// `https://na-west-1.cloud.dwavesys.com/sapi/v2/`.
///
/// Settings store the full endpoint URL, but `DWAVE_API_REGION` is the SDK's
/// short region name, and it is the only location var the compose file
/// forwards. Anything that does not match the known host shape yields an
/// empty value, which resolves identically to unset and lets the SDK choose.
fn leap_region_from_url(url: &str) -> String {
    url.split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split('/')
        .next()
        .and_then(|host| host.strip_suffix(".cloud.dwavesys.com"))
        .filter(|region| !region.is_empty() && !region.contains('.'))
        .unwrap_or_default()
        .to_string()
}

// ─── Defaults ───────────────────────────────────────────────────────────────

fn default_port() -> u16 {
    20049
}
fn default_validator_port() -> u16 {
    30333
}
fn default_validator_rpc_port() -> u16 {
    9944
}
fn default_publication_enabled() -> bool {
    true
}
fn default_listen() -> String {
    "::".to_string()
}
fn default_num_cpus() -> u32 {
    1
}
fn default_cpu_enabled() -> bool {
    true
}
fn default_timeout() -> u32 {
    3
}
fn default_heartbeat_interval() -> u32 {
    15
}
fn default_heartbeat_timeout() -> u32 {
    300
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_genesis_config() -> String {
    "genesis_block.json".to_string()
}
fn default_tofu() -> bool {
    true
}
fn default_trust_db() -> String {
    "~/.quip/trust.db".to_string()
}
fn default_rest_host() -> String {
    "127.0.0.1".to_string()
}
fn default_rest_port() -> i32 {
    -1
}
fn default_telemetry_enabled() -> bool {
    true
}
fn default_telemetry_dir() -> String {
    "telemetry".to_string()
}

// ─── Node config ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeConfig {
    // Network
    // v0.2: public Caddy/API/dashboard/RPC host port.
    #[serde(default = "default_port")]
    pub port: u16,
    // v0.2: validator libp2p host port. Defaults to 30333 to match the
    // container-internal libp2p port (substrate's upstream default), so the
    // host publish is a 1:1 mapping unless the user overrides it.
    #[serde(default = "default_validator_port")]
    pub validator_port: u16,
    // Host RPC port. Optional in Docker; always published in Native mode.
    #[serde(default = "default_validator_rpc_port")]
    pub validator_rpc_port: u16,
    #[serde(default = "default_publication_enabled")]
    pub public_api_enabled: bool,
    #[serde(default = "default_publication_enabled")]
    pub validator_p2p_enabled: bool,
    #[serde(default)]
    pub validator_rpc_enabled: bool,
    #[serde(default)]
    pub service_ports: crate::service_ports::ServicePorts,
    // v0.1 legacy fields kept for app-settings.json compatibility.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub public_host: String,
    #[serde(default)]
    pub public_port: Option<u16>,
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default)]
    pub auto_mine: bool,

    // Identity
    #[serde(default)]
    pub secret: String,
    #[serde(default = "default_genesis_config")]
    pub genesis_config: String,

    // Trust
    #[serde(default = "default_tofu")]
    pub tofu: bool,
    #[serde(default = "default_trust_db")]
    pub trust_db: String,

    // TLS (empty = self-signed auto-generated by protocol)
    #[serde(default)]
    pub tls_cert_file: String,
    #[serde(default)]
    pub tls_key_file: String,
    #[serde(default)]
    pub verify_tls: bool,

    // REST API (disabled by default: port < 0)
    #[serde(default = "default_rest_host")]
    pub rest_host: String,
    #[serde(default = "default_rest_port")]
    pub rest_port: i32,
    #[serde(default = "default_rest_port")]
    pub rest_insecure_port: i32,

    // Telemetry
    #[serde(default = "default_telemetry_enabled")]
    pub telemetry_enabled: bool,
    #[serde(default = "default_telemetry_dir")]
    pub telemetry_dir: String,

    // Logging
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub node_log: String,
    #[serde(default)]
    pub http_log: String,

    // CPU mining. cpu_enabled controls whether config.toml gets a [cpu]
    // section at all; defaults true so pre-toggle settings keep CPU mining.
    #[serde(default = "default_cpu_enabled")]
    pub cpu_enabled: bool,
    #[serde(default = "default_num_cpus")]
    pub num_cpus: u32,
    /// `quip-cpu-*` binary the coordinator spawns for `[cpu]`. `None` means the
    /// image's own default, so an operator who never touches this keeps
    /// whatever the image ships rather than being pinned to a name this app
    /// compiled in. Same for the GPU solvers below.
    #[serde(default)]
    pub cpu_solver: Option<String>,
    /// `quip-cuda-*` binary for every enabled `[cuda.N]` device.
    #[serde(default)]
    pub cuda_solver: Option<String>,
    /// `quip-metal-*` binary for `[metal]` (macOS Native only).
    #[serde(default)]
    pub metal_solver: Option<String>,

    // GPU mining
    #[serde(default)]
    pub gpu_backend: GpuBackend,
    #[serde(default)]
    pub gpu_device_configs: Vec<GpuDeviceConfig>,
    #[serde(default)]
    pub metal_config: MetalConfig,

    // D-Wave QPU
    #[serde(default)]
    pub dwave_config: Option<DwaveConfig>,

    // Advanced
    #[serde(default = "default_timeout")]
    pub timeout: u32,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u32,
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: u32,
    #[serde(default)]
    pub fanout: Option<u32>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            port: 20049,
            validator_port: 30333,
            validator_rpc_port: 9944,
            public_api_enabled: true,
            validator_p2p_enabled: true,
            validator_rpc_enabled: false,
            service_ports: crate::service_ports::ServicePorts::default(),
            listen: "::".to_string(),
            public_host: String::new(),
            public_port: None,
            node_name: String::new(),
            peers: vec![],
            auto_mine: false,
            secret: String::new(),
            genesis_config: "genesis_block.json".to_string(),
            tofu: true,
            trust_db: "~/.quip/trust.db".to_string(),
            tls_cert_file: String::new(),
            tls_key_file: String::new(),
            verify_tls: false,
            rest_host: "127.0.0.1".to_string(),
            rest_port: -1,
            rest_insecure_port: -1,
            telemetry_enabled: true,
            telemetry_dir: "telemetry".to_string(),
            log_level: "info".to_string(),
            node_log: String::new(),
            http_log: String::new(),
            cpu_enabled: true,
            num_cpus: 1,
            cpu_solver: None,
            cuda_solver: None,
            metal_solver: None,
            gpu_backend: GpuBackend::Local,
            gpu_device_configs: vec![],
            metal_config: MetalConfig::default(),
            dwave_config: None,
            timeout: 3,
            heartbeat_interval: 15,
            heartbeat_timeout: 300,
            fanout: None,
        }
    }
}

// ─── App settings ───────────────────────────────────────────────────────────

fn default_hostname() -> String {
    ":20049".to_string()
}

/// Accept the old `"qpu"` string (briefly shipped to users) as an alias for
/// Cpu, so app-settings.json files from that window still load cleanly.
/// Without this, the outer `unwrap_or_default()` in `load_settings` would
/// wipe every stored setting on first load after upgrade.
fn deserialize_image_tag_compat<'de, D>(d: D) -> Result<ImageTag, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let s = String::deserialize(d)?;
    match s.as_str() {
        "cpu" | "qpu" => Ok(ImageTag::Cpu),
        "cuda" => Ok(ImageTag::Cuda),
        other => Err(D::Error::custom(format!("unknown image_tag: {other}"))),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub node_config: NodeConfig,
    pub active_tab: String,
    pub window_maximized: bool,
    #[serde(default, deserialize_with = "deserialize_image_tag_compat")]
    pub image_tag: ImageTag,
    #[serde(default)]
    pub tls_enabled: bool,
    #[serde(default = "default_hostname", alias = "dashboard_hostname")]
    pub hostname: String,
    #[serde(default)]
    pub cert_email: String,
    #[serde(default)]
    pub zerossl_api_key: String,
    #[serde(default)]
    pub run_mode: RunMode,
    #[serde(default)]
    pub update_channel: UpdateChannel,
    /// Memory ceiling for the miner container, in GiB. `None` defers to the
    /// compose default (`QUIP_MINER_MEM_LIMIT:-16g`).
    ///
    /// Needs to live here rather than in `.env`: `write_env_file` regenerates
    /// `.env` on every Start, so a hand-edited value there never survives.
    #[serde(default)]
    pub miner_mem_limit_gb: Option<u32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            node_config: NodeConfig::default(),
            active_tab: "status".to_string(),
            window_maximized: false,
            image_tag: ImageTag::default(),
            tls_enabled: false,
            hostname: default_hostname(),
            cert_email: String::new(),
            zerossl_api_key: String::new(),
            run_mode: RunMode::default(),
            update_channel: UpdateChannel::default(),
            miner_mem_limit_gb: None,
        }
    }
}

// ─── Stack status ───────────────────────────────────────────────────────────

/// Per-service state from `docker compose ps --format json`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ServiceStatus {
    /// container_name (e.g. `quip-cpu`, `quip-dashboard`)
    pub name: String,
    /// compose service key (e.g. `cpu`, `dashboard`)
    pub service: String,
    pub running: bool,
    /// `healthy` | `unhealthy` | `starting` | null
    #[serde(default)]
    pub health: Option<String>,
    pub status_text: String,
    pub image: String,
}

/// Aggregate roll-up of the compose stack, exposed to the frontend as the
/// `get_stack_status` response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StackStatus {
    pub services: Vec<ServiceStatus>,
    pub overall: StackHealth,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StackHealth {
    /// every expected service running AND (no healthcheck OR healthy)
    Running,
    /// ≥1 service running, ≥1 not running
    Degraded,
    /// Services are up, but the validator is still replaying the chain. Only
    /// the health monitor produces this — the Compose roll-up cannot see it,
    /// since a syncing validator is a running container like any other.
    Syncing,
    /// ≥1 healthcheck reports unhealthy
    Unhealthy,
    /// no services running (or all exited)
    Stopped,
}

/// Bootstrap config stored at a fixed OS-standard location.
/// Holds install-level state that shouldn't live in app-settings.json
/// (e.g. the Postgres password, which must not appear in support bundles
/// or screenshots of the settings file).
#[derive(Serialize, Deserialize, Default)]
struct BootstrapConfig {
    #[serde(default)]
    data_dir: Option<String>,
    #[serde(default)]
    postgres_password: Option<String>,
}

fn bootstrap_path() -> PathBuf {
    let config = dirs::config_dir().unwrap_or_else(|| dirs::home_dir().unwrap().join(".config"));
    config.join("quip-node-manager").join("bootstrap.json")
}

fn load_bootstrap() -> BootstrapConfig {
    fs::read_to_string(bootstrap_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_bootstrap(cfg: &BootstrapConfig) -> Result<(), String> {
    let path = bootstrap_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| e.to_string())
}

fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join("quip-data")
}

pub fn data_dir() -> PathBuf {
    let bootstrap = load_bootstrap();
    match bootstrap.data_dir {
        Some(ref p) if !p.is_empty() => PathBuf::from(p),
        _ => default_data_dir(),
    }
}

pub fn ensure_data_dir() -> Result<(), String> {
    fs::create_dir_all(data_dir()).map_err(|e| e.to_string())
}

/// Postgres password for the dashboard's database, generated once on first
/// access and persisted in bootstrap.json. Never regenerated — rotating
/// would desync from the existing `quip_pgdata` volume.
pub fn postgres_password() -> String {
    let mut cfg = load_bootstrap();
    if let Some(p) = cfg.postgres_password.as_ref().filter(|s| !s.is_empty()) {
        return p.clone();
    }
    let bytes: [u8; 16] = rand::random();
    let pw = hex::encode(bytes);
    cfg.postgres_password = Some(pw.clone());
    // Save is best-effort — if it fails, we'll regenerate next start (and
    // break the DB). Log via stderr so it surfaces in the terminal.
    if let Err(e) = save_bootstrap(&cfg) {
        eprintln!("warning: failed to persist postgres password: {e}");
    }
    pw
}

fn settings_path() -> PathBuf {
    data_dir().join("app-settings.json")
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    let mut settings = if let Ok(content) = fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        AppSettings::default()
    };
    // Native mode is only supported on macOS
    if !cfg!(target_os = "macos") {
        settings.run_mode = RunMode::Docker;
    }
    settings
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    crate::service_ports::validate(&settings.node_config, &settings.run_mode)?;
    ensure_data_dir()?;
    let content = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(settings_path(), content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_settings() -> Result<AppSettings, String> {
    Ok(load_settings())
}

#[tauri::command]
pub async fn update_settings(mut settings: AppSettings) -> Result<(), String> {
    // Native mode is only supported on macOS
    if !cfg!(target_os = "macos") {
        settings.run_mode = RunMode::Docker;
    }
    save_settings(&settings)
}

#[tauri::command]
pub async fn is_first_boot() -> bool {
    !bootstrap_path().exists()
}

#[tauri::command]
pub async fn get_default_data_dir() -> Result<String, String> {
    Ok(default_data_dir().to_string_lossy().to_string())
}

#[tauri::command]
pub async fn get_data_dir() -> Result<String, String> {
    Ok(data_dir().to_string_lossy().to_string())
}

#[tauri::command]
pub async fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

#[tauri::command]
pub async fn set_data_dir(path: String) -> Result<(), String> {
    let new_dir = if path.is_empty() {
        None
    } else {
        // Validate path is writable
        let p = PathBuf::from(&path);
        fs::create_dir_all(&p).map_err(|e| format!("Cannot create directory {}: {}", path, e))?;
        Some(path)
    };
    // Load-modify-save so we don't wipe postgres_password when changing the
    // data dir.
    let mut cfg = load_bootstrap();
    cfg.data_dir = new_dir;
    save_bootstrap(&cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn node_config_defaults_validator_port_for_legacy_json() {
        let config: NodeConfig = serde_json::from_value(json!({
            "port": 20049,
            "listen": "::"
        }))
        .unwrap();

        assert_eq!(config.port, 20049);
        assert_eq!(config.validator_port, 30333);
    }

    #[test]
    fn node_config_missing_cpu_enabled_defaults_to_true() {
        // Pre-toggle app-settings.json files have no cpu_enabled key; they
        // must load as enabled, not silently stop CPU mining on upgrade.
        let config: NodeConfig = serde_json::from_value(json!({})).unwrap();
        assert!(config.cpu_enabled);
    }

    #[test]
    fn app_settings_default_hostname_is_public_api_port() {
        assert_eq!(AppSettings::default().hostname, ":20049");
    }

    #[test]
    fn app_settings_deserializes_legacy_dashboard_hostname_alias() {
        let settings: AppSettings = serde_json::from_value(json!({
            "node_config": {},
            "active_tab": "status",
            "window_maximized": false,
            "image_tag": "cpu",
            "dashboard_hostname": "node.example.com, node.example.com:20049"
        }))
        .unwrap();

        assert_eq!(
            settings.hostname,
            "node.example.com, node.example.com:20049"
        );
    }

    #[test]
    fn app_settings_deserializes_legacy_qpu_image_without_losing_fields() {
        let settings: AppSettings = serde_json::from_value(json!({
            "node_config": {
                "port": 20444,
                "node_name": "legacy-node"
            },
            "active_tab": "status",
            "window_maximized": false,
            "image_tag": "qpu"
        }))
        .unwrap();

        assert_eq!(settings.node_config.port, 20444);
        assert_eq!(settings.node_config.validator_port, 30333);
        assert_eq!(settings.node_config.node_name, "legacy-node");
        assert_eq!(settings.image_tag, ImageTag::Cpu);
        assert_eq!(settings.hostname, ":20049");
    }
}
