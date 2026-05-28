// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::settings::{data_dir, NodeConfig, RunMode};
use std::fs;
use std::path::Path;
use tauri::{AppHandle, Emitter};
use toml::{Table, Value};

const BACKUP_DIR: &str = ".v0.1_backup";
const ENV_BACKUP_FILE: &str = ".env.v0.1_backup";
const DOCKER_VALIDATOR_RPC: &str = "ws://quip-validator:9944";
const DOCKER_SIGNER_KEY: &str = "/data/keystore.json";
const DOCKER_MINER_REST_HOST: &str = "0.0.0.0";
const DOCKER_MINER_REST_PORT: i64 = 80;
const DEFAULT_NATIVE_REST_PORT: i64 = 20100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigSchema {
    V01,
    V02,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromotedMinerConfig {
    pub node_name: Option<String>,
    pub public_host: Option<String>,
    pub public_port: Option<u16>,
    pub rest_host: Option<String>,
    pub log_level: Option<String>,
    pub node_log: Option<String>,
}

impl PromotedMinerConfig {
    pub fn is_empty(&self) -> bool {
        self.node_name.is_none()
            && self.public_host.is_none()
            && self.public_port.is_none()
            && self.rest_host.is_none()
            && self.log_level.is_none()
            && self.node_log.is_none()
    }

    pub fn apply_to_node_config(&self, config: &mut NodeConfig) {
        let defaults = NodeConfig::default();

        if config.node_name.trim().is_empty() {
            if let Some(value) = self.node_name.as_ref() {
                config.node_name = value.clone();
            }
        }
        if config.public_host.trim().is_empty() {
            if let Some(value) = self.public_host.as_ref() {
                config.public_host = value.clone();
            }
        }
        if config.public_port.is_none() {
            config.public_port = self.public_port;
        }
        if config.rest_host == defaults.rest_host {
            if let Some(value) = self.rest_host.as_ref() {
                config.rest_host = value.clone();
            }
        }
        if config.log_level == defaults.log_level {
            if let Some(value) = self.log_level.as_ref() {
                config.log_level = value.clone();
            }
        }
        if config.node_log.trim().is_empty() {
            if let Some(value) = self.node_log.as_ref() {
                config.node_log = value.clone();
            }
        }
    }

    fn merge_from(&mut self, other: PromotedMinerConfig) {
        self.node_name = self.node_name.take().or(other.node_name);
        self.public_host = self.public_host.take().or(other.public_host);
        self.public_port = self.public_port.take().or(other.public_port);
        self.rest_host = self.rest_host.take().or(other.rest_host);
        self.log_level = self.log_level.take().or(other.log_level);
        self.node_log = self.node_log.take().or(other.node_log);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub changed: bool,
    pub promoted: PromotedMinerConfig,
    pub warnings: Vec<String>,
}

impl MigrationReport {
    fn merge(&mut self, other: MigrationReport) {
        self.changed |= other.changed;
        self.promoted.merge_from(other.promoted);
        self.warnings.extend(other.warnings);
    }
}

#[derive(Debug)]
struct ConfigMigration {
    content: String,
    promoted: PromotedMinerConfig,
    warnings: Vec<String>,
}

pub fn migrate_for_run_mode(run_mode: &RunMode) -> Result<MigrationReport, String> {
    let mut report = MigrationReport::default();
    let base = data_dir();
    let config_dir = match run_mode {
        RunMode::Docker => base.join("data"),
        RunMode::Native => base.clone(),
    };

    report.merge(migrate_config_dir(&config_dir, run_mode)?);
    report.merge(migrate_env_file(&base.join(".env"))?);
    Ok(report)
}

pub fn persist_promoted_settings(promoted: &PromotedMinerConfig) -> Result<(), String> {
    if promoted.is_empty() {
        return Ok(());
    }

    let mut settings = crate::settings::load_settings();
    promoted.apply_to_node_config(&mut settings.node_config);
    crate::settings::save_settings(&settings)
}

pub fn emit_report(app: &AppHandle, report: &MigrationReport) {
    if report.changed {
        emit_log(app, "INFO", "Migrated v0.1 node data to the v0.2 layout");
    }
    for warning in &report.warnings {
        emit_log(app, "WARN", warning);
    }
}

fn emit_log(app: &AppHandle, level: &str, message: &str) {
    for line in message.lines() {
        let entry = serde_json::json!({
            "timestamp": "",
            "level": level,
            "message": line,
        });
        let _ = app.emit("node-log", entry);
    }
}

fn migrate_config_dir(config_dir: &Path, run_mode: &RunMode) -> Result<MigrationReport, String> {
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        return Ok(MigrationReport::default());
    }

    let content = fs::read_to_string(&config_path)
        .map_err(|e| format!("read {}: {e}", config_path.display()))?;
    let Some(migration) = migrate_config_content(&content, run_mode)? else {
        return Ok(MigrationReport::default());
    };

    let backup_dir = config_dir.join(BACKUP_DIR);
    if backup_dir.exists() {
        return Err(format!(
            "{} is v0.1 but {} already exists; move or inspect the backup before retrying",
            config_path.display(),
            backup_dir.display()
        ));
    }

    fs::create_dir_all(&backup_dir).map_err(|e| format!("create {}: {e}", backup_dir.display()))?;
    move_existing_entries_to_backup(config_dir, &backup_dir)?;

    fs::write(&config_path, migration.content)
        .map_err(|e| format!("write {}: {e}", config_path.display()))?;

    Ok(MigrationReport {
        changed: true,
        promoted: migration.promoted,
        warnings: migration.warnings,
    })
}

fn move_existing_entries_to_backup(config_dir: &Path, backup_dir: &Path) -> Result<(), String> {
    for entry in
        fs::read_dir(config_dir).map_err(|e| format!("read {}: {e}", config_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read {} entry: {e}", config_dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy() == BACKUP_DIR {
            continue;
        }

        let dest = backup_dir.join(&name);
        fs::rename(&path, &dest)
            .map_err(|e| format!("move {} to {}: {e}", path.display(), dest.display()))?;
    }
    Ok(())
}

fn migrate_env_file(path: &Path) -> Result<MigrationReport, String> {
    if !path.exists() {
        return Ok(MigrationReport::default());
    }

    let content = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let has_legacy = content.lines().any(is_legacy_dashboard_env_line);
    let has_validator_rpc = content.lines().any(is_validator_rpc_env_line);
    if !has_legacy && has_validator_rpc {
        return Ok(MigrationReport::default());
    }

    let backup_path = path.with_file_name(ENV_BACKUP_FILE);
    if backup_path.exists() && has_legacy {
        return Err(format!(
            "{} still contains v0.1 dashboard env keys but {} already exists",
            path.display(),
            backup_path.display()
        ));
    }

    if !backup_path.exists() {
        fs::copy(path, &backup_path).map_err(|e| {
            format!(
                "backup {} to {}: {e}",
                path.display(),
                backup_path.display()
            )
        })?;
    }

    let mut next_lines: Vec<String> = content
        .lines()
        .filter(|line| !is_legacy_dashboard_env_line(line))
        .map(ToOwned::to_owned)
        .collect();
    if !has_validator_rpc {
        next_lines.push("# QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944".to_string());
    }
    fs::write(path, next_lines.join("\n") + "\n")
        .map_err(|e| format!("write {}: {e}", path.display()))?;

    let mut warnings = Vec::new();
    if has_legacy {
        warnings.push("removed v0.1 QUIP_NODE_URL/QUIP_NODE_TOKEN env keys".to_string());
    }
    if !has_validator_rpc {
        warnings.push("added QUIP_VALIDATOR_RPC_URLS placeholder to migrated .env".to_string());
    }

    Ok(MigrationReport {
        changed: true,
        promoted: PromotedMinerConfig::default(),
        warnings,
    })
}

fn is_legacy_dashboard_env_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("QUIP_NODE_URL=") || trimmed.starts_with("QUIP_NODE_TOKEN=")
}

fn is_validator_rpc_env_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("QUIP_VALIDATOR_RPC_URLS=")
        || trimmed.starts_with("# QUIP_VALIDATOR_RPC_URLS=")
}

fn migrate_config_content(
    content: &str,
    run_mode: &RunMode,
) -> Result<Option<ConfigMigration>, String> {
    let mut root: Table = toml::from_str(content).map_err(|e| format!("parse config.toml: {e}"))?;
    match detect_schema(&root)? {
        ConfigSchema::V02 => Ok(None),
        ConfigSchema::V01 => convert_v01_config(&mut root, run_mode).map(Some),
    }
}

fn detect_schema(root: &Table) -> Result<ConfigSchema, String> {
    let has_global = root.contains_key("global");
    let has_miner = root.contains_key("miner");
    match (has_global, has_miner) {
        (true, false) => Ok(ConfigSchema::V01),
        (false, true) => Ok(ConfigSchema::V02),
        (true, true) => Err("config.toml contains both [global] and [miner]".to_string()),
        (false, false) => Err("config.toml contains neither [global] nor [miner]".to_string()),
    }
}

fn convert_v01_config(root: &mut Table, run_mode: &RunMode) -> Result<ConfigMigration, String> {
    let Some(Value::Table(global)) = root.remove("global") else {
        return Err("config.toml [global] section is not a table".to_string());
    };

    let promoted = promoted_from_global(&global);
    let mut warnings = dropped_key_warnings(&global);
    let mut miner = Table::new();

    miner.insert(
        "validators".to_string(),
        Value::Array(vec![Value::String(default_validator_rpc(
            run_mode, &global,
        ))]),
    );
    miner.insert(
        "signer_key".to_string(),
        Value::String(default_signer_key(run_mode)),
    );
    miner.insert(
        "rest_host".to_string(),
        Value::String(default_rest_host(run_mode, &global)),
    );
    miner.insert(
        "rest_port".to_string(),
        Value::Integer(default_rest_port(run_mode, &global)),
    );

    insert_string_if_present(&mut miner, "node_name", promoted.node_name.as_ref());
    insert_string_if_present(&mut miner, "public_host", promoted.public_host.as_ref());
    if let Some(value) = promoted.public_port {
        miner.insert("public_port".to_string(), Value::Integer(i64::from(value)));
    }
    insert_string_if_present(&mut miner, "log_level", promoted.log_level.as_ref());
    insert_string_if_present(&mut miner, "node_log", promoted.node_log.as_ref());

    let mut next = Table::new();
    next.insert("miner".to_string(), Value::Table(miner));
    for (key, value) in std::mem::take(root) {
        if should_drop_top_level_table(&key) {
            warnings.push(format!("dropped v0.1 table [{key}]"));
        } else {
            next.insert(key, value);
        }
    }

    let content =
        toml::to_string_pretty(&next).map_err(|e| format!("render migrated config.toml: {e}"))?;
    Ok(ConfigMigration {
        content,
        promoted,
        warnings,
    })
}

fn promoted_from_global(global: &Table) -> PromotedMinerConfig {
    PromotedMinerConfig {
        node_name: string_from_table(global, "node_name"),
        public_host: string_from_table(global, "public_host"),
        public_port: u16_from_table(global, "public_port"),
        rest_host: string_from_table(global, "rest_host"),
        log_level: string_from_table(global, "log_level"),
        node_log: string_from_table(global, "node_log"),
    }
}

fn dropped_key_warnings(global: &Table) -> Vec<String> {
    global
        .keys()
        .filter(|key| !promoted_global_key(key))
        .map(|key| format!("dropped v0.1 config key [global].{key}"))
        .collect()
}

fn promoted_global_key(key: &str) -> bool {
    matches!(
        key,
        "node_name" | "public_host" | "public_port" | "rest_host" | "log_level" | "node_log"
    )
}

fn should_drop_top_level_table(key: &str) -> bool {
    matches!(key, "telemetry" | "file_telemetry" | "http")
}

fn default_validator_rpc(run_mode: &RunMode, global: &Table) -> String {
    match run_mode {
        RunMode::Docker => DOCKER_VALIDATOR_RPC.to_string(),
        RunMode::Native => {
            let port = u16_from_table(global, "port").unwrap_or(20049);
            format!("ws://127.0.0.1:{port}/rpc")
        }
    }
}

fn default_signer_key(run_mode: &RunMode) -> String {
    match run_mode {
        RunMode::Docker => DOCKER_SIGNER_KEY.to_string(),
        RunMode::Native => data_dir()
            .join("keystore.json")
            .to_string_lossy()
            .to_string(),
    }
}

fn default_rest_host(run_mode: &RunMode, global: &Table) -> String {
    match run_mode {
        RunMode::Docker => DOCKER_MINER_REST_HOST.to_string(),
        RunMode::Native => string_from_table(global, "rest_host")
            .unwrap_or_else(|| NodeConfig::default().rest_host),
    }
}

fn default_rest_port(run_mode: &RunMode, global: &Table) -> i64 {
    match run_mode {
        RunMode::Docker => DOCKER_MINER_REST_PORT,
        RunMode::Native => i64_from_table(global, "rest_insecure_port")
            .or_else(|| i64_from_table(global, "rest_port"))
            .filter(|port| *port > 0)
            .unwrap_or(DEFAULT_NATIVE_REST_PORT),
    }
}

fn insert_string_if_present(table: &mut Table, key: &str, value: Option<&String>) {
    if let Some(value) = value.filter(|s| !s.trim().is_empty()) {
        table.insert(key.to_string(), Value::String(value.clone()));
    }
}

fn string_from_table(table: &Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn u16_from_table(table: &Table, key: &str) -> Option<u16> {
    i64_from_table(table, key).and_then(|n| u16::try_from(n).ok())
}

fn i64_from_table(table: &Table, key: &str) -> Option<i64> {
    table.get(key).and_then(Value::as_integer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const CPU_V01: &str = r#"
[global]
listen = "0.0.0.0"
port = 20049
node_name = "cpu-home"
public_host = "node.example.com"
public_port = 24444
rest_host = "127.0.0.1"
log_level = "debug"
node_log = "/data/node.log"
peer = ["legacy-peer:20049"]
telemetry_enabled = false

[cpu]
num_cpus = 4
"#;

    const CUDA_V01: &str = r#"
[global]
port = 20049
log_level = "info"

[gpu]
utilization = 80
yielding = false

[cuda.0]

[cuda.1]
utilization = 50
yielding = true

[nvidia.0]
device = 0
"#;

    const QPU_V01: &str = r#"
[global]
port = 20049
node_name = "qpu-home"

[cpu]
num_cpus = 1

[qpu]

[dwave]
token = "DWAVE-TOKEN"
daily_budget = "60s"
solver = "Advantage2_System1.13"
"#;

    #[test]
    fn migrates_cpu_config_and_promotes_public_settings() {
        let migration = migrate_config_content(CPU_V01, &RunMode::Docker)
            .unwrap()
            .expect("v0.1 config should migrate");

        assert!(migration.content.contains("[miner]\n"));
        assert!(!migration.content.contains("[global]"));
        assert!(migration
            .content
            .contains("validators = [\"ws://quip-validator:9944\"]"));
        assert!(migration
            .content
            .contains("signer_key = \"/data/keystore.json\""));
        assert!(migration.content.contains("rest_host = \"0.0.0.0\""));
        assert!(migration.content.contains("rest_port = 80"));
        assert!(migration.content.contains("node_name = \"cpu-home\""));
        assert!(migration
            .content
            .contains("public_host = \"node.example.com\""));
        assert!(migration.content.contains("public_port = 24444"));
        assert!(migration.content.contains("[cpu]\n"));
        assert_eq!(
            migration.promoted.public_host.as_deref(),
            Some("node.example.com")
        );
        assert_eq!(migration.promoted.public_port, Some(24444));
        assert!(migration
            .warnings
            .contains(&"dropped v0.1 config key [global].peer".to_string()));
    }

    #[test]
    fn migrates_cuda_config_and_preserves_backend_tables() {
        let migration = migrate_config_content(CUDA_V01, &RunMode::Docker)
            .unwrap()
            .expect("v0.1 config should migrate");

        assert!(migration.content.contains("[gpu]\n"));
        assert!(migration.content.contains("[cuda.0]\n"));
        assert!(migration.content.contains("[cuda.1]\n"));
        assert!(migration.content.contains("utilization = 50"));
        assert!(migration.content.contains("[nvidia.0]\n"));
    }

    #[test]
    fn migrates_qpu_dwave_config() {
        let migration = migrate_config_content(QPU_V01, &RunMode::Docker)
            .unwrap()
            .expect("v0.1 config should migrate");

        assert!(migration.content.contains("[qpu]\n"));
        assert!(migration.content.contains("[dwave]\n"));
        assert!(migration.content.contains("token = \"DWAVE-TOKEN\""));
        assert!(migration.content.contains("daily_budget = \"60s\""));
    }

    #[test]
    fn native_migration_uses_host_validator_rpc() {
        let migration = migrate_config_content(CPU_V01, &RunMode::Native)
            .unwrap()
            .expect("v0.1 config should migrate");

        assert!(migration
            .content
            .contains("validators = [\"ws://127.0.0.1:20049/rpc\"]"));
        assert!(migration.content.contains("rest_host = \"127.0.0.1\""));
        assert!(migration.content.contains("rest_port = 20100"));
    }

    #[test]
    fn already_v02_config_is_idempotent() {
        let content = r#"
[miner]
validators = ["ws://quip-validator:9944"]
signer_key = "/data/keystore.json"
"#;
        assert!(migrate_config_content(content, &RunMode::Docker)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ambiguous_config_is_refused() {
        let err = migrate_config_content("[global]\n\n[miner]\n", &RunMode::Docker).unwrap_err();
        assert!(err.contains("both [global] and [miner]"));
    }

    #[test]
    fn unknown_config_is_refused() {
        let err = migrate_config_content("[cpu]\nnum_cpus = 1\n", &RunMode::Docker).unwrap_err();
        assert!(err.contains("neither [global] nor [miner]"));
    }

    #[test]
    fn migrates_env_file_and_removes_legacy_dashboard_keys() {
        let dir = unique_temp_dir("env");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        fs::write(
            &path,
            "PUID=501\nQUIP_NODE_URL=http://quip-node:20100\nQUIP_NODE_TOKEN=old\n",
        )
        .unwrap();

        let report = migrate_env_file(&path).unwrap();
        let migrated = fs::read_to_string(&path).unwrap();

        assert!(report.changed);
        assert!(!migrated.contains("QUIP_NODE_URL"));
        assert!(!migrated.contains("QUIP_NODE_TOKEN"));
        assert!(migrated.contains("# QUIP_VALIDATOR_RPC_URLS=ws://quip-validator:9944"));
        assert!(dir.join(ENV_BACKUP_FILE).exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_config_dir_with_backup() {
        let dir = unique_temp_dir("config");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), CPU_V01).unwrap();
        fs::write(dir.join("node.log"), "old log").unwrap();

        let report = migrate_config_dir(&dir, &RunMode::Docker).unwrap();

        assert!(report.changed);
        assert!(dir.join(BACKUP_DIR).join("config.toml").exists());
        assert!(dir.join(BACKUP_DIR).join("node.log").exists());
        assert!(fs::read_to_string(dir.join("config.toml"))
            .unwrap()
            .contains("[miner]\n"));

        fs::remove_dir_all(dir).unwrap();
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nonce: u64 = rand::random();
        std::env::temp_dir().join(format!("quip-migration-test-{label}-{nonce}"))
    }
}
