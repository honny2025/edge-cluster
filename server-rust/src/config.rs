//! 配置加载：环境变量（EDGE_SECTION__KEY） > config.toml > 内置默认值。
//!
//! 与 Python 版 app/config.py 逐项对齐。所有可调项都有默认值，开箱即用。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

/// 测试专用全局锁：把依赖 `init_global` 全局配置的用例串行化，避免 `cargo test`
/// 并行时多个用例互相覆盖这份进程级全局配置（`backup_roundtrip` 与 `metric_tiers`
/// 各自 init_global 不同配置）。生产路径只 init 一次，不受影响。
#[cfg(test)]
pub static CONFIG_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 进程内全局配置。启动时由 `init_global` 设置一次，之后只读。
/// 用 `Mutex<Option<..>>` 而非 `OnceLock`，是为了单元测试里能重复设置以隔离用例
/// （生产路径只调用一次，行为不变）。
pub fn init_global(cfg: Config) {
    *CONFIG.lock().expect("config lock poisoned") = Some(cfg);
}

/// 读取全局配置的一份副本。必须在 `init_global` 之后调用。
pub fn get() -> Config {
    CONFIG
        .lock()
        .expect("config lock poisoned")
        .clone()
        .expect("config 尚未初始化")
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub token: String,
    pub discovery_port: u16,
    pub offline_threshold: i64,
    pub offline_confirm_times: i64,
    pub heartbeat_interval: i64,
    pub service_probe_interval: i64,
    pub metric_raw_retention_days: i64,
    pub metric_minute_retention_days: i64,
    pub metric_hour_retention_days: i64,
    pub metric_rollup_interval_minutes: i64,
    pub metric_retention_days: i64,
    pub metric_min_interval: i64,
    pub log_level: String,
    pub gateway_timeout: f64,
    pub service_watchdog_enabled: bool,
    pub deploy_concurrency: i64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 8000,
            token: "edge-cluster-dev-token".into(),
            discovery_port: 37020,
            offline_threshold: 15,
            offline_confirm_times: 3,
            heartbeat_interval: 5,
            service_probe_interval: 15,
            metric_raw_retention_days: 2,
            metric_minute_retention_days: 30,
            metric_hour_retention_days: 365,
            metric_rollup_interval_minutes: 60,
            metric_retention_days: 0,
            metric_min_interval: 2,
            log_level: "INFO".into(),
            gateway_timeout: 60.0,
            service_watchdog_enabled: true,
            deploy_concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct DataConfig {
    pub db_path: String,
    pub models_dir: String,
    pub logs_dir: String,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            db_path: "data/edge_cluster.db".into(),
            models_dir: "data/models".into(),
            logs_dir: "data/logs".into(),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct WebConfig {
    pub serve_static: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { serve_static: true }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct AlertConfig {
    pub enabled: bool,
    pub cpu_usage_warn: f64,
    pub mem_usage_warn: f64,
    pub storage_usage_warn: f64,
    pub temperature_warn: f64,
    pub temperature_critical: f64,
    pub alarm_on_offline: bool,
    pub probe_timeout_warn: i64,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cpu_usage_warn: 85.0,
            mem_usage_warn: 85.0,
            storage_usage_warn: 90.0,
            temperature_warn: 45.0,
            temperature_critical: 55.0,
            alarm_on_offline: true,
            probe_timeout_warn: 3,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub enforce_device_token: bool,
    pub auto_issue_device_token: bool,
    pub warn_on_default_token: bool,
    pub remote_exec_enabled: bool,
    pub remote_exec_allowlist: Vec<String>,
    pub remote_exec_timeout: i64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enforce_device_token: false,
            auto_issue_device_token: true,
            warn_on_default_token: true,
            remote_exec_enabled: false,
            remote_exec_allowlist: vec![
                "ls".into(),
                "cat".into(),
                "df".into(),
                "free".into(),
                "uptime".into(),
                "uname".into(),
                "getprop".into(),
                "ps".into(),
                "id".into(),
                "date".into(),
            ],
            remote_exec_timeout: 15,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub staging_dir: String,
    pub max_transfer_mb: i64,
    pub transfer_ttl_minutes: i64,
    pub max_list_entries: i64,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            staging_dir: "data/staging".into(),
            max_transfer_mb: 4096,
            transfer_ttl_minutes: 60,
            max_list_entries: 2000,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub data: DataConfig,
    pub web: WebConfig,
    pub alerts: AlertConfig,
    pub security: SecurityConfig,
    pub files: FileConfig,
    /// 相对路径解析基准目录（config.toml 所在目录）。加载时注入，不参与序列化。
    #[serde(skip)]
    pub base_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            data: DataConfig::default(),
            web: WebConfig::default(),
            alerts: AlertConfig::default(),
            security: SecurityConfig::default(),
            files: FileConfig::default(),
            base_dir: PathBuf::from("."),
        }
    }
}

impl Config {
    /// 相对路径解析基准目录（config.toml 所在目录）。
    pub fn base_dir(&self) -> PathBuf {
        self.base_dir.clone()
    }

    fn resolve(&self, p: &str) -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            self.base_dir().join(path)
        }
    }

    pub fn db_file(&self) -> PathBuf {
        self.resolve(&self.data.db_path)
    }
    pub fn models_path(&self) -> PathBuf {
        self.resolve(&self.data.models_dir)
    }
    pub fn logs_path(&self) -> PathBuf {
        self.resolve(&self.data.logs_dir)
    }
    pub fn staging_path(&self) -> PathBuf {
        self.resolve(&self.files.staging_dir)
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for p in [
            self.db_file().parent().map(|x| x.to_path_buf()),
            Some(self.models_path()),
            Some(self.logs_path()),
            Some(self.staging_path()),
        ]
        .into_iter()
        .flatten()
        {
            std::fs::create_dir_all(&p)?;
        }
        Ok(())
    }
}

fn as_bool(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// 载入配置。`config_file` 为空时依次看 EDGE_CONFIG_FILE、config.toml。
pub fn load_config(config_file: Option<&Path>) -> Config {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    load_config_with_env(config_file, &env)
}

/// 环境变量注入入口（单元测试用）：`env` 提供 EDGE_* 与 EDGE_CONFIG_FILE，
/// 其余逻辑与 `load_config` 完全一致。单独拆出来是因为 `std::env::vars()` 是进程级
/// 全局，测试无法在并行的 `#[test]` 里隔离；注入 map 后配置层可被逐用例钉死。
pub fn load_config_with_env(
    config_file: Option<&Path>,
    env: &std::collections::HashMap<String, String>,
) -> Config {
    let mut cfg = Config::default();

    // 配置路径优先级：显式参数 > EDGE_CONFIG_FILE > ./config.toml > ./server/config.toml
    let mut path: Option<PathBuf> = config_file.map(|p| p.to_path_buf());
    if path.is_none() {
        if let Some(p) = env.get("EDGE_CONFIG_FILE") {
            path = Some(PathBuf::from(p));
        }
    }
    let mut base_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolved_path = if let Some(p) = &path {
        let p2 = p.clone();
        if let Some(parent) = p2.parent() {
            if !parent.as_os_str().is_empty() {
                base_dir = parent.to_path_buf();
            }
        }
        p2
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let candidate = cwd.join("config.toml");
        if candidate.exists() {
            candidate
        } else {
            let server_candidate = cwd.join("server").join("config.toml");
            if server_candidate.exists() {
                base_dir = cwd.join("server");
                server_candidate
            } else {
                // 无配置文件，用默认值；base_dir 保持 cwd
                candidate
            }
        }
    };

    // 解析 toml
    let mut raw: toml::Value = toml::Value::Table(Default::default());
    if resolved_path.exists() {
        if let Ok(text) = std::fs::read_to_string(&resolved_path) {
            match toml::from_str::<toml::Value>(&text) {
                Ok(v) => raw = v,
                Err(e) => eprintln!("[config] 解析 {:?} 失败，使用默认配置: {}", resolved_path, e),
            }
        }
    }

    // toml → 结构体（保留默认值兜底）
    if let Ok(parsed) = toml::from_str::<Config>(&toml::to_string(&raw).unwrap_or_default()) {
        cfg = parsed;
    } else if let Ok(text) = std::fs::read_to_string(&resolved_path) {
        if let Ok(parsed) = toml::from_str::<Config>(&text) {
            cfg = parsed;
        }
    }

    // 环境变量覆盖：EDGE_SERVER__PORT=9000 / EDGE_WEB__SERVE_STATIC=false
    let prefix = "EDGE_";
    for (key, value) in env {
        if !key.starts_with(prefix) || !key.contains("__") {
            continue;
        }
        let rest = &key[prefix.len()..];
        let mut parts = rest.splitn(2, "__");
        let section = parts.next().unwrap_or("").to_ascii_lowercase();
        let field = parts.next().unwrap_or("").to_ascii_lowercase();
        apply_env(&mut cfg, &section, &field, value);
    }

    cfg.base_dir = base_dir;
    apply_legacy_retention(&mut cfg, &raw, env);
    cfg
}

fn apply_env(cfg: &mut Config, section: &str, field: &str, value: &str) {
    use std::str::FromStr;
    macro_rules! set_int {
        ($target:expr) => {
            if let Ok(v) = i64::from_str(value) {
                *$target = v;
            }
        };
    }
    match section {
        "server" => match field {
            "host" => cfg.server.host = value.to_string(),
            "port" => {
                if let Ok(v) = value.parse::<u16>() {
                    cfg.server.port = v;
                }
            }
            "token" => cfg.server.token = value.to_string(),
            "discovery_port" => {
                if let Ok(v) = value.parse::<u16>() {
                    cfg.server.discovery_port = v;
                }
            }
            "offline_threshold" => set_int!(&mut cfg.server.offline_threshold),
            "offline_confirm_times" => set_int!(&mut cfg.server.offline_confirm_times),
            "heartbeat_interval" => set_int!(&mut cfg.server.heartbeat_interval),
            "service_probe_interval" => set_int!(&mut cfg.server.service_probe_interval),
            "metric_raw_retention_days" => set_int!(&mut cfg.server.metric_raw_retention_days),
            "metric_minute_retention_days" => set_int!(&mut cfg.server.metric_minute_retention_days),
            "metric_hour_retention_days" => set_int!(&mut cfg.server.metric_hour_retention_days),
            "metric_rollup_interval_minutes" => {
                set_int!(&mut cfg.server.metric_rollup_interval_minutes)
            }
            "metric_retention_days" => set_int!(&mut cfg.server.metric_retention_days),
            "metric_min_interval" => set_int!(&mut cfg.server.metric_min_interval),
            "log_level" => cfg.server.log_level = value.to_string(),
            "gateway_timeout" => {
                if let Ok(v) = f64::from_str(value) {
                    cfg.server.gateway_timeout = v;
                }
            }
            "service_watchdog_enabled" => cfg.server.service_watchdog_enabled = as_bool(value),
            "deploy_concurrency" => set_int!(&mut cfg.server.deploy_concurrency),
            _ => {}
        },
        "data" => match field {
            "db_path" => cfg.data.db_path = value.to_string(),
            "models_dir" => cfg.data.models_dir = value.to_string(),
            "logs_dir" => cfg.data.logs_dir = value.to_string(),
            _ => {}
        },
        "web" => match field {
            "serve_static" => cfg.web.serve_static = as_bool(value),
            _ => {}
        },
        "alerts" => match field {
            "enabled" => cfg.alerts.enabled = as_bool(value),
            "cpu_usage_warn" => {
                if let Ok(v) = f64::from_str(value) {
                    cfg.alerts.cpu_usage_warn = v;
                }
            }
            "mem_usage_warn" => {
                if let Ok(v) = f64::from_str(value) {
                    cfg.alerts.mem_usage_warn = v;
                }
            }
            "storage_usage_warn" => {
                if let Ok(v) = f64::from_str(value) {
                    cfg.alerts.storage_usage_warn = v;
                }
            }
            "temperature_warn" => {
                if let Ok(v) = f64::from_str(value) {
                    cfg.alerts.temperature_warn = v;
                }
            }
            "temperature_critical" => {
                if let Ok(v) = f64::from_str(value) {
                    cfg.alerts.temperature_critical = v;
                }
            }
            "alarm_on_offline" => cfg.alerts.alarm_on_offline = as_bool(value),
            "probe_timeout_warn" => set_int!(&mut cfg.alerts.probe_timeout_warn),
            _ => {}
        },
        "security" => match field {
            "enforce_device_token" => cfg.security.enforce_device_token = as_bool(value),
            "auto_issue_device_token" => cfg.security.auto_issue_device_token = as_bool(value),
            "warn_on_default_token" => cfg.security.warn_on_default_token = as_bool(value),
            "remote_exec_enabled" => cfg.security.remote_exec_enabled = as_bool(value),
            "remote_exec_timeout" => set_int!(&mut cfg.security.remote_exec_timeout),
            _ => {}
        },
        "files" => match field {
            "staging_dir" => cfg.files.staging_dir = value.to_string(),
            "max_transfer_mb" => set_int!(&mut cfg.files.max_transfer_mb),
            "transfer_ttl_minutes" => set_int!(&mut cfg.files.transfer_ttl_minutes),
            "max_list_entries" => set_int!(&mut cfg.files.max_list_entries),
            _ => {}
        },
        _ => {}
    }
}

/// 兼容废弃的 `metric_retention_days`（原始层的旧名字）。
/// 优先级：环境变量新键 > 环境变量旧键 > toml 新键 > toml 旧键 > 默认值。
fn apply_legacy_retention(
    cfg: &mut Config,
    raw: &toml::Value,
    env: &std::collections::HashMap<String, String>,
) {
    let env_new = env.get("EDGE_SERVER__METRIC_RAW_RETENTION_DAYS").cloned();
    let env_legacy = env.get("EDGE_SERVER__METRIC_RETENTION_DAYS").cloned();

    let toml_new = raw
        .get("server")
        .and_then(|s| s.get("metric_raw_retention_days"))
        .and_then(|v| v.as_integer());
    let toml_legacy = raw
        .get("server")
        .and_then(|s| s.get("metric_retention_days"))
        .and_then(|v| v.as_integer());

    let legacy: Option<i64> = if env_new.is_some() {
        return;
    } else if let Some(v) = env_legacy {
        v.parse::<i64>().ok()
    } else if toml_new.is_none() {
        toml_legacy
    } else {
        return;
    };

    if let Some(days) = legacy {
        if days > 0 {
            cfg.server.metric_raw_retention_days = days;
            eprintln!(
                "[config] metric_retention_days={} 已废弃，按原始层保留天数处理（metric_raw_retention_days={}）。",
                days, days
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// 在临时目录里加载一次配置：显式给 config.toml 路径，避免读到仓库自带的
    /// `server-rust/config.toml`；`toml_content` 为 None 时不留配置文件（走默认值）。
    fn load_with(env: &HashMap<String, String>, toml_content: Option<&str>) -> Config {
        let dir = std::env::temp_dir().join("edge-cluster-config-check");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        match toml_content {
            Some(text) => std::fs::write(&cfg_path, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&cfg_path);
            }
        }
        load_config_with_env(Some(&cfg_path), env)
    }

    /// 对应 Python `docs/acceptance/config_check.py` 的 13 条用例，逐条钉死覆盖优先级：
    /// 尤其钉死（历史 bug）前缀截断导致的覆盖静默失效，以及 `metric_retention_days` 旧键兼容。
    #[test]
    fn env_override_precedence() {
        // 1. 不加覆盖时读默认值
        let c = load_with(&env_of(&[]), None);
        assert_eq!(c.server.port, 8000, "默认端口应为 8000");
        assert_eq!(c.server.host, "0.0.0.0");

        // 2. EDGE_SERVER__PORT 覆盖端口
        let c = load_with(&env_of(&[("EDGE_SERVER__PORT", "9000")]), None);
        assert_eq!(c.server.port, 9000);

        // 3. EDGE_SERVER__HOST 覆盖地址
        let c = load_with(&env_of(&[("EDGE_SERVER__HOST", "127.0.0.1")]), None);
        assert_eq!(c.server.host, "127.0.0.1");

        // 4. EDGE_SERVER__TOKEN 覆盖 token
        let c = load_with(&env_of(&[("EDGE_SERVER__TOKEN", "zzz-test")]), None);
        assert_eq!(c.server.token, "zzz-test");

        // 5. EDGE_SERVER__LOG_LEVEL 覆盖日志级别
        let c = load_with(&env_of(&[("EDGE_SERVER__LOG_LEVEL", "DEBUG")]), None);
        assert_eq!(c.server.log_level, "DEBUG");

        // 6. EDGE_WEB__SERVE_STATIC=false 转 bool
        let c = load_with(&env_of(&[("EDGE_WEB__SERVE_STATIC", "false")]), None);
        assert!(!c.web.serve_static, "serve_static=false 应转成 false");

        // 7. EDGE_ALERTS__CPU_USAGE_WARN=55.5 转 float
        let c = load_with(&env_of(&[("EDGE_ALERTS__CPU_USAGE_WARN", "55.5")]), None);
        assert_eq!(c.alerts.cpu_usage_warn, 55.5);

        // 8. 多个变量同时生效
        let c = load_with(
            &env_of(&[("EDGE_SERVER__PORT", "9100"), ("EDGE_SERVER__HOST", "1.2.3.4")]),
            None,
        );
        assert_eq!(c.server.port, 9100);
        assert_eq!(c.server.host, "1.2.3.4");

        // 9. 新键 EDGE_SERVER__METRIC_RAW_RETENTION_DAYS 生效
        let c = load_with(&env_of(&[("EDGE_SERVER__METRIC_RAW_RETENTION_DAYS", "5")]), None);
        assert_eq!(c.server.metric_raw_retention_days, 5);

        // 10. 旧键 EDGE_SERVER__METRIC_RETENTION_DAYS 单独出现 → 按原始层处理
        let c = load_with(&env_of(&[("EDGE_SERVER__METRIC_RETENTION_DAYS", "9")]), None);
        assert_eq!(c.server.metric_raw_retention_days, 9);

        // 11. 旧键 + 新键同时出现 → 新键赢（旧键不得覆盖它）
        let c = load_with(
            &env_of(&[
                ("EDGE_SERVER__METRIC_RETENTION_DAYS", "9"),
                ("EDGE_SERVER__METRIC_RAW_RETENTION_DAYS", "3"),
            ]),
            None,
        );
        assert_eq!(c.server.metric_raw_retention_days, 3);

        // 12. config.toml 里写了旧键 → 按原始层处理
        let c = load_with(&env_of(&[]), Some("[server]\nmetric_retention_days = 11\n"));
        assert_eq!(c.server.metric_raw_retention_days, 11);

        // 13. config.toml 旧键 + 环境变量新键 → 新键赢
        let c = load_with(
            &env_of(&[("EDGE_SERVER__METRIC_RAW_RETENTION_DAYS", "4")]),
            Some("[server]\nmetric_retention_days = 11\n"),
        );
        assert_eq!(c.server.metric_raw_retention_days, 4);
    }
}
