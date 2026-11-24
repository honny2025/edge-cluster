//! Agent 配置加载。
//!
//! 优先级：命令行参数 > 环境变量（EDGE_SECTION__KEY） > config.toml > 内置默认值。
//! node_id 留空时自动生成并回写配置文件，保证重启后身份稳定（不产生重复节点）。

use std::collections::HashMap;
use std::env;
use std::fs;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const AGENT_VERSION: &str = "0.5.0-m5";

/// 运行基目录：config.toml / state / models / logs 的相对根。
/// 与 Python 版"整目录拷进 Termux 后 cd 进去运行"的部署模型一致，
/// 用当前工作目录作为基准。
pub fn base_dir() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentCfg {
    pub node_id: String,
    pub name: String,
    pub token: String,
    pub heartbeat_interval: i64,
    pub platform_url: String,
    pub platform_port: u16,
}

impl Default for AgentCfg {
    fn default() -> Self {
        Self {
            node_id: String::new(),
            name: String::new(),
            token: "edge-cluster-dev-token".to_string(),
            heartbeat_interval: 5,
            platform_url: String::new(),
            platform_port: 8000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryCfg {
    pub enabled: bool,
    pub port: u16,
    pub broadcast_addr: String,
    pub attempts: i64,
    pub interval: f64,
    pub timeout: f64,
}

impl Default for DiscoveryCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 37020,
            broadcast_addr: "255.255.255.255".to_string(),
            attempts: 3,
            interval: 1.0,
            timeout: 1.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReconnectCfg {
    pub base_delay: f64,
    pub max_delay: f64,
    pub jitter: f64,
}

impl Default for ReconnectCfg {
    fn default() -> Self {
        Self {
            base_delay: 1.0,
            max_delay: 60.0,
            jitter: 0.2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageCfg {
    pub models_dir: String,
    pub logs_dir: String,
    pub log_max_bytes: i64,
    pub log_backup_count: usize,
}

impl Default for StorageCfg {
    fn default() -> Self {
        Self {
            models_dir: "./models".to_string(),
            logs_dir: "./logs".to_string(),
            log_max_bytes: 5 * 1024 * 1024,
            log_backup_count: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceCfg {
    pub health_timeout: i64,
    pub health_fail_threshold: i64,
    pub health_interval: i64,
    pub restart_backoff: i64,
    pub stop_grace: i64,
}

impl Default for ServiceCfg {
    fn default() -> Self {
        Self {
            health_timeout: 60,
            health_fail_threshold: 3,
            health_interval: 3,
            restart_backoff: 3,
            stop_grace: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SimulateCfg {
    pub enabled: bool,
    pub model: String,
    pub soc: String,
    pub android_version: String,
    pub fail_download_once: bool,
    pub crash_service_after: i64,
    pub disconnect_after: i64,
}

impl Default for SimulateCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "Xiaomi 13".to_string(),
            soc: "Snapdragon 8 Gen 2".to_string(),
            android_version: "14".to_string(),
            fail_download_once: false,
            crash_service_after: 0,
            disconnect_after: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub agent: AgentCfg,
    pub discovery: DiscoveryCfg,
    pub reconnect: ReconnectCfg,
    pub storage: StorageCfg,
    pub service: ServiceCfg,
    pub simulate: SimulateCfg,
    #[serde(skip)]
    pub config_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent: AgentCfg::default(),
            discovery: DiscoveryCfg::default(),
            reconnect: ReconnectCfg::default(),
            storage: StorageCfg::default(),
            service: ServiceCfg::default(),
            simulate: SimulateCfg::default(),
            config_path: base_dir().join("config.toml"),
        }
    }
}

fn resolve(p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        base_dir().join(path)
    }
}

impl Config {
    pub fn models_path(&self) -> PathBuf {
        resolve(&self.storage.models_dir)
    }

    pub fn logs_path(&self) -> PathBuf {
        resolve(&self.storage.logs_dir)
    }

    pub fn ensure_dirs(&self) {
        for p in [self.models_path(), self.logs_path()] {
            let _ = fs::create_dir_all(&p);
        }
    }

    /// HTTP 基地址 → WebSocket 地址（`.../ws/agent`）。
    pub fn ws_url(&self, platform_url: Option<&str>) -> Result<String, String> {
        let mut base = platform_url
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.agent.platform_url.clone());
        while base.ends_with('/') {
            base.pop();
        }
        if base.is_empty() {
            return Err("平台地址为空".to_string());
        }
        if let Some(rest) = base.strip_prefix("http://") {
            base = format!("ws://{rest}");
        } else if let Some(rest) = base.strip_prefix("https://") {
            base = format!("wss://{rest}");
        } else if !base.starts_with("ws://") && !base.starts_with("wss://") {
            base = format!("ws://{base}");
        }
        // 主机部分无端口则补上 platform_port（与 Python 的 ":" 判断一致）
        let host = base.splitn(2, "//").nth(1).unwrap_or(&base);
        if !host.contains(':') {
            base = format!("{base}:{}", self.agent.platform_port);
        }
        Ok(format!("{base}/ws/agent"))
    }
}

/// 加载配置：TOML + 环境变量覆盖。解析失败打印并退回默认值。
pub fn load_config(config_file: Option<&str>) -> Config {
    let mut cfg = Config::default();
    let path = match config_file {
        Some(p) => PathBuf::from(p),
        None => base_dir().join("config.toml"),
    };
    let path = if path.is_absolute() {
        path
    } else {
        base_dir().join(path)
    };
    cfg.config_path = path.clone();

    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(parsed) => {
                    cfg.agent = parsed.agent;
                    cfg.discovery = parsed.discovery;
                    cfg.reconnect = parsed.reconnect;
                    cfg.storage = parsed.storage;
                    cfg.service = parsed.service;
                    cfg.simulate = parsed.simulate;
                }
                Err(e) => eprintln!("[config] 解析 {} 失败，使用默认配置: {e}", path.display()),
            },
            Err(e) => eprintln!("[config] 读取 {} 失败，使用默认配置: {e}", path.display()),
        }
    }

    apply_env(&mut cfg);
    cfg
}

fn apply_env(cfg: &mut Config) {
    for (key, val) in env::vars() {
        if !key.starts_with("EDGE_") || !key.contains("__") {
            continue;
        }
        let rest = &key[5..]; // strip "EDGE_"
        let (section, field) = match rest.split_once("__") {
            Some(x) => x,
            None => continue,
        };
        let (section, field) = (section.to_lowercase(), field.to_lowercase());
        match section.as_str() {
            "agent" => set_agent(cfg, &field, &val),
            "discovery" => set_discovery(cfg, &field, &val),
            "reconnect" => set_reconnect(cfg, &field, &val),
            "storage" => set_storage(cfg, &field, &val),
            "service" => set_service(cfg, &field, &val),
            "simulate" => set_simulate(cfg, &field, &val),
            _ => {}
        }
    }
}

fn coerce_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn coerce_i64(v: &str) -> i64 {
    v.trim().parse::<f64>().map(|f| f as i64).unwrap_or(0)
}

fn coerce_u16(v: &str) -> u16 {
    v.trim().parse::<f64>().map(|f| f as u16).unwrap_or(0)
}

fn coerce_f64(v: &str) -> f64 {
    v.trim().parse::<f64>().unwrap_or(0.0)
}

fn set_agent(cfg: &mut Config, field: &str, val: &str) {
    match field {
        "node_id" => cfg.agent.node_id = val.to_string(),
        "name" => cfg.agent.name = val.to_string(),
        "token" => cfg.agent.token = val.to_string(),
        "heartbeat_interval" => cfg.agent.heartbeat_interval = coerce_i64(val),
        "platform_url" => cfg.agent.platform_url = val.to_string(),
        "platform_port" => cfg.agent.platform_port = coerce_u16(val),
        _ => {}
    }
}

fn set_discovery(cfg: &mut Config, field: &str, val: &str) {
    match field {
        "enabled" => cfg.discovery.enabled = coerce_bool(val),
        "port" => cfg.discovery.port = coerce_u16(val),
        "broadcast_addr" => cfg.discovery.broadcast_addr = val.to_string(),
        "attempts" => cfg.discovery.attempts = coerce_i64(val),
        "interval" => cfg.discovery.interval = coerce_f64(val),
        "timeout" => cfg.discovery.timeout = coerce_f64(val),
        _ => {}
    }
}

fn set_reconnect(cfg: &mut Config, field: &str, val: &str) {
    match field {
        "base_delay" => cfg.reconnect.base_delay = coerce_f64(val),
        "max_delay" => cfg.reconnect.max_delay = coerce_f64(val),
        "jitter" => cfg.reconnect.jitter = coerce_f64(val),
        _ => {}
    }
}

fn set_storage(cfg: &mut Config, field: &str, val: &str) {
    match field {
        "models_dir" => cfg.storage.models_dir = val.to_string(),
        "logs_dir" => cfg.storage.logs_dir = val.to_string(),
        "log_max_bytes" => cfg.storage.log_max_bytes = coerce_i64(val),
        "log_backup_count" => cfg.storage.log_backup_count = coerce_i64(val) as usize,
        _ => {}
    }
}

fn set_service(cfg: &mut Config, field: &str, val: &str) {
    match field {
        "health_timeout" => cfg.service.health_timeout = coerce_i64(val),
        "health_fail_threshold" => cfg.service.health_fail_threshold = coerce_i64(val),
        "health_interval" => cfg.service.health_interval = coerce_i64(val),
        "restart_backoff" => cfg.service.restart_backoff = coerce_i64(val),
        "stop_grace" => cfg.service.stop_grace = coerce_i64(val),
        _ => {}
    }
}

fn set_simulate(cfg: &mut Config, field: &str, val: &str) {
    match field {
        "enabled" => cfg.simulate.enabled = coerce_bool(val),
        "model" => cfg.simulate.model = val.to_string(),
        "soc" => cfg.simulate.soc = val.to_string(),
        "android_version" => cfg.simulate.android_version = val.to_string(),
        "fail_download_once" => cfg.simulate.fail_download_once = coerce_bool(val),
        "crash_service_after" => cfg.simulate.crash_service_after = coerce_i64(val),
        "disconnect_after" => cfg.simulate.disconnect_after = coerce_i64(val),
        _ => {}
    }
}

/// 生成稳定可读的 node_id：形如 phone-a1b2c3d4。
pub fn default_node_id(name: &str) -> String {
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
    if !name.is_empty() {
        let slug: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let slug = slug.trim_matches('-');
        if !slug.is_empty() {
            let slug = &slug[..slug.len().min(16)];
            return format!("{slug}-{suffix}");
        }
    }
    format!("phone-{suffix}")
}

/// 状态文件路径（每实例一份）。文件名做安全过滤。
fn state_path(node_id: &str) -> PathBuf {
    let safe: String = node_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { c } else { '_' })
        .collect();
    let safe = if safe.is_empty() { "default" } else { &safe };
    base_dir().join("state").join(format!("{safe}.json"))
}

fn read_json_object(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.as_object().cloned()
}

pub fn load_device_token(node_id: &str) -> String {
    if node_id.is_empty() {
        return String::new();
    }
    let path = state_path(&format!("token-{node_id}"));
    let obj = match read_json_object(&path) {
        Some(o) => o,
        None => return String::new(),
    };
    obj.get("device_token")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .unwrap_or_default()
}

pub fn save_device_token(node_id: &str, token: &str) {
    if node_id.is_empty() || token.is_empty() {
        return;
    }
    let path = state_path(&format!("token-{node_id}"));
    let data = serde_json::json!({"node_id": node_id, "device_token": token});
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&data) {
        if fs::write(&path, text).is_err() {
            eprintln!("[config] 保存设备 token 失败（不影响运行）: {}", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
    }
}

pub fn load_instance_state(name: &str) -> serde_json::Map<String, serde_json::Value> {
    if name.is_empty() {
        return serde_json::Map::new();
    }
    let path = state_path(&format!("by-name-{name}"));
    match read_json_object(&path) {
        Some(o) if o.get("node_id").and_then(|v| v.as_str()).is_some() => o,
        _ => serde_json::Map::new(),
    }
}

pub fn save_instance_state(name: &str, node_id: &str, display_name: &str) {
    let data = serde_json::json!({"name": display_name, "node_id": node_id});
    let write = |p: &PathBuf| {
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = serde_json::to_string_pretty(&data).map(|t| fs::write(p, t));
    };
    write(&state_path(&format!("by-name-{}", if name.is_empty() { node_id } else { name })));
    write(&state_path(&format!("by-id-{node_id}")));
}

/// 把 node_id 回写到 config.toml —— 仅当配置里 node_id 为空且未指定 --name。
pub fn update_config_node_id(cfg: &Config, node_id: &str, name: &str) {
    let path = &cfg.config_path;
    if !path.exists() {
        return;
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut out = text;
    if let Some(re) = regex_node_id_line(&out) {
        out = out.replace(re, &format!("node_id = \"{node_id}\""));
    }
    if !name.is_empty() {
        if let Some(re) = regex_name_line(&out) {
            out = out.replace(re, &format!("name = \"{name}\""));
        }
    }
    let _ = fs::write(path, out);
}

// 文本替换（保持注释与排版），简化实现：逐行处理 `node_id = "..."` / `name = "..."`。
fn regex_node_id_line<'a>(text: &'a str) -> Option<&'a str> {
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("node_id") {
            if rest.trim_start().starts_with('=') && rest.contains('"') {
                return Some(line);
            }
        }
    }
    None
}

fn regex_name_line<'a>(text: &'a str) -> Option<&'a str> {
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("name") {
            if rest.trim_start().starts_with('=') && rest.contains('"') {
                return Some(line);
            }
        }
    }
    None
}

/// 本机局域网 IP。
pub fn local_ip() -> String {
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".to_string()
}

// --------------------------------------------------------------- 托管服务状态

fn services_path(node_id: &str) -> PathBuf {
    let safe: String = node_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { c } else { '_' })
        .collect();
    let safe = if safe.is_empty() { "default" } else { &safe };
    base_dir().join("state").join(format!("services-{safe}.json"))
}

/// 托管服务记录。command/args/auto_restart/auto_start/restart_count/pid。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedService {
    pub command: String,
    pub args: Vec<String>,
    pub auto_restart: bool,
    pub auto_start: bool,
    pub restart_count: i64,
    pub pid: i64,
}

pub fn load_services(node_id: &str) -> HashMap<String, SavedService> {
    if node_id.is_empty() {
        return HashMap::new();
    }
    let path = services_path(node_id);
    let obj = match read_json_object(&path) {
        Some(o) => o,
        None => return HashMap::new(),
    };
    let items = match obj.get("services").and_then(|s| s.as_object()) {
        Some(m) => m,
        None => return HashMap::new(),
    };
    let mut out = HashMap::new();
    for (sid, rec) in items {
        if let Some(rec) = rec.as_object() {
            let command = rec
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if command.is_empty() {
                continue;
            }
            out.insert(
                sid.clone(),
                SavedService {
                    command,
                    args: rec
                        .get("args")
                        .and_then(|a| a.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                    auto_restart: rec
                        .get("auto_restart")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    auto_start: rec
                        .get("auto_start")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    restart_count: rec
                        .get("restart_count")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    pid: rec.get("pid").and_then(|v| v.as_i64()).unwrap_or(0),
                },
            );
        }
    }
    out
}

/// 落盘托管服务记录（原子替换）。
pub fn save_services(node_id: &str, records: &HashMap<String, SavedService>) {
    if node_id.is_empty() {
        return;
    }
    let mut safe_records = serde_json::Map::new();
    for (sid, rec) in records {
        if rec.command.is_empty() {
            continue;
        }
        safe_records.insert(
            sid.clone(),
            serde_json::json!({
                "command": rec.command,
                "args": rec.args,
                "auto_restart": rec.auto_restart,
                "auto_start": rec.auto_start,
                "restart_count": rec.restart_count,
                "pid": rec.pid,
            }),
        );
    }
    let data = serde_json::json!({"node_id": node_id, "services": safe_records});
    let path = services_path(node_id);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&data) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}
