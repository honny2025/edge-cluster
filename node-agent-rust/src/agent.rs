//! 节点 Agent：注册 + 心跳 + 指令处理。

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::config::{
    self, load_device_token, save_device_token, save_services, Config, SavedService, AGENT_VERSION,
};
use crate::discovery::discover_platform;
use crate::files::{FileError, FileService};
use crate::metrics::{device_info, MetricsCollector};
use crate::rollback::InstallJournal;
use crate::util::{human_bytes, now_epoch, pid};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = tokio::sync::Mutex<SplitSink<WsStream, Message>>;

const FS_COMMANDS: [&str; 8] = [
    "fs_roots", "fs_list", "fs_stat", "fs_du", "fs_mkdir", "fs_delete", "fs_copy", "fs_move",
];

/// 托管服务运行记录。
struct ServiceRecord {
    proc: Option<tokio::process::Child>,
    command: String,
    args: Vec<String>,
    started_at: f64,
    auto_restart: bool,
    auto_start: bool,
    restart_count: i64,
    exit_status: Option<i32>,
}

impl ServiceRecord {
    fn alive(&mut self) -> bool {
        if self.exit_status.is_some() {
            return false;
        }
        if let Some(proc) = self.proc.as_mut() {
            match proc.try_wait() {
                Ok(Some(status)) => {
                    self.exit_status = status.code();
                    false
                }
                Ok(None) => true,
                Err(_) => true,
            }
        } else {
            false
        }
    }
}

/// 部署错误：区分「用户取消」与「部署失败」。
enum DeployError {
    Cancelled(String),
    Failed(String),
}

pub struct EdgeAgent {
    cfg: Config,
    simulate: bool,
    node_id: String,
    name: String,
    ip: String,
    device: serde_json::Value,
    collector: Arc<Mutex<MetricsCollector>>,
    files: Arc<FileService>,
    platform_url: Mutex<Option<String>>,
    device_token: Mutex<String>,
    services: Mutex<HashMap<String, ServiceRecord>>,
    handled_cmds: Mutex<HashMap<String, f64>>,
    cancelled_tasks: Mutex<HashSet<String>>,
    stop: AtomicBool,
    current_sink: tokio::sync::Mutex<Option<Arc<WsSink>>>,
}

impl EdgeAgent {
    pub fn new(cfg: Config) -> Self {
        let simulate = cfg.simulate.enabled;
        let node_id = cfg.agent.node_id.clone();
        let name = if cfg.agent.name.is_empty() { node_id.clone() } else { cfg.agent.name.clone() };
        let ip = config::local_ip();
        let device = device_info(simulate, &cfg.simulate);
        let collector = Arc::new(Mutex::new(MetricsCollector::new(simulate)));
        let files = Arc::new(FileService::new(vec![
            ("models".to_string(), "模型目录".to_string(), cfg.models_path().to_string_lossy().to_string()),
            ("logs".to_string(), "日志目录".to_string(), cfg.logs_path().to_string_lossy().to_string()),
        ]));
        cfg.ensure_dirs();
        let device_token = load_device_token(&node_id);
        Self {
            cfg,
            simulate,
            node_id,
            name,
            ip,
            device,
            collector,
            files,
            platform_url: Mutex::new(None),
            device_token: Mutex::new(device_token),
            services: Mutex::new(HashMap::new()),
            handled_cmds: Mutex::new(HashMap::new()),
            cancelled_tasks: Mutex::new(HashSet::new()),
            stop: AtomicBool::new(false),
            current_sink: tokio::sync::Mutex::new(None),
        }
    }

    // ================================================================== 生命周期

    pub async fn run(self: Arc<Self>) {
        // 先恢复托管服务（连平台之前）
        let n = self.restore_services().await;
        if n > 0 {
            log::warn!("Auto Start 恢复完成：{} 个服务已拉起", n);
        }

        let mut attempt = 0u32;
        while !self.stop.load(Ordering::SeqCst) {
            let ok = self.clone().connect_and_serve().await;
            if ok {
                attempt = 0;
            } else {
                attempt += 1;
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let delay = self.backoff_delay(attempt);
            log::warn!("将在 {:.1}s 后重连（第 {} 次重试）", delay, attempt);
            // 分段休眠，随时响应停止信号。
            let mut remaining = delay;
            while remaining > 0.0 {
                if self.stop.load(Ordering::SeqCst) {
                    break;
                }
                let chunk = remaining.min(0.5);
                tokio::time::sleep(Duration::from_secs_f64(chunk)).await;
                remaining -= chunk;
            }
        }
    }

    pub async fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let mut slot = self.current_sink.lock().await;
        if let Some(sink) = slot.take() {
            let mut s = sink.lock().await;
            let _ = s.close().await;
        }
    }

    pub async fn stop_all_services(&self) -> i64 {
        let mut n = 0;
        let mut map = self.services.lock().unwrap();
        for (_sid, rec) in map.iter_mut() {
            if let Some(mut proc) = rec.proc.take() {
                if rec.exit_status.is_none() {
                    if self.terminate_child(&mut proc).await {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    /// 优雅停掉一个子进程：先 SIGTERM 等 stop_grace 秒，再 SIGKILL。
    async fn terminate_child(&self, proc: &mut tokio::process::Child) -> bool {
        #[cfg(unix)]
        {
            let grace = self.cfg.service.stop_grace.max(1) as u64;
            if let Some(pid) = proc.id() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            match tokio::time::timeout(Duration::from_secs(grace), proc.wait()).await {
                Ok(_) => true,
                Err(_) => {
                    let _ = proc.start_kill();
                    true
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = proc.start_kill();
            let _ = proc.wait().await;
            true
        }
    }

    fn backoff_delay(&self, attempt: u32) -> f64 {
        let base = self.cfg.reconnect.base_delay;
        let cap = self.cfg.reconnect.max_delay;
        let jitter = self.cfg.reconnect.jitter;
        let raw = cap.min(base * 2f64.powi(attempt.max(1) as i32 - 1));
        let factor = 1.0 + (rand::random::<f64>() * 2.0 - 1.0) * jitter;
        (raw * factor).max(0.2)
    }

    fn ws_token(&self) -> String {
        let device_token = self.device_token.lock().unwrap().clone();
        if !device_token.is_empty() {
            device_token
        } else {
            self.cfg.agent.token.clone()
        }
    }

    async fn connect_and_serve(self: Arc<Self>) -> bool {
        // 1) 定位平台（force=true：即使配置里填过 platform_url 也重新广播，拿到平台当前 IP）
        if self.platform_url.lock().unwrap().is_none() {
            let cfg = self.cfg.clone();
            let token = self.cfg.agent.token.clone();
            let name = self.name.clone();
            let found = tokio::task::spawn_blocking(move || {
                discover_platform(&cfg, &token, AGENT_VERSION, &name, true)
            })
            .await
            .unwrap_or(None);
            match found {
                Some(url) => *self.platform_url.lock().unwrap() = Some(url),
                None => {
                    if !self.cfg.agent.platform_url.is_empty() {
                        *self.platform_url.lock().unwrap() =
                            Some(self.cfg.agent.platform_url.trim_end_matches('/').to_string());
                    } else {
                        return false;
                    }
                }
            }
        }
        let platform = self.platform_url.lock().unwrap().clone().unwrap();
        let url = match self.cfg.ws_url(Some(&platform)) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let full_url = format!("{url}?token={}", self.ws_token());
        log::info!("正在连接平台 {}（node_id={}）", url, self.node_id);

        let (ws, _) = match connect_async(&full_url).await {
            Ok(x) => x,
            Err(e) => {
                log::warn!("连接中断: {e}");
                return false;
            }
        };
        let (sink, stream) = ws.split();
        let sink = Arc::new(tokio::sync::Mutex::new(sink));
        *self.current_sink.lock().await = Some(sink.clone());

        self.register(&sink).await;
        let session_started = Instant::now();

        let mut hb = tokio::spawn(self.clone().heartbeat_loop(sink.clone(), session_started));
        let mut recv = tokio::spawn(self.clone().recv_loop(stream, sink.clone()));
        let mut sim = tokio::spawn(self.clone().simulate_loop(sink.clone()));
        let mut wd = tokio::spawn(self.clone().service_watchdog());

        let ok = tokio::select! {
            _ = &mut hb => false,
            r = &mut recv => r.unwrap_or(false),
            _ = &mut sim => true,
            _ = &mut wd => false,
        };
        hb.abort();
        recv.abort();
        sim.abort();
        wd.abort();

        *self.current_sink.lock().await = None;
        // 平台地址可能变了（换 IP / 换端口）。无条件清空缓存，下次循环重新广播发现。
        *self.platform_url.lock().unwrap() = None;
        log::info!("与平台的连接已正常结束");
        ok
    }

    // ================================================================== 注册

    async fn register(&self, sink: &Arc<WsSink>) {
        let info = serde_json::json!({
            "node_id": self.node_id,
            "name": self.name,
            "model": self.device.get("model").and_then(|v| v.as_str()).unwrap_or(""),
            "soc": self.device.get("soc").and_then(|v| v.as_str()).unwrap_or(""),
            "android_version": self.device.get("android_version").and_then(|v| v.as_str()).unwrap_or(""),
            "cpu": self.device.get("cpu").and_then(|v| v.as_str()).unwrap_or(""),
            "gpu": self.device.get("gpu").and_then(|v| v.as_str()).unwrap_or(""),
            "npu": self.device.get("npu").and_then(|v| v.as_str()).unwrap_or(""),
            "qnn_version": self.device.get("qnn_version").and_then(|v| v.as_str()).unwrap_or(""),
            "is_root": self.device.get("is_root").and_then(|v| v.as_i64()).unwrap_or(-1),
            "capabilities": self.device.get("capabilities").cloned().unwrap_or(serde_json::json!({})),
            "agent_version": AGENT_VERSION,
            "ip": self.ip,
            "simulate": self.simulate,
            "pid": pid(),
        });
        let mut payload = info.clone();
        payload["type"] = serde_json::json!("register");
        let mut p = info.clone();
        p["heartbeat_interval"] = serde_json::json!(self.cfg.agent.heartbeat_interval);
        payload["payload"] = p;
        let _ = sink.lock().await.send(Message::text(payload.to_string())).await;
        log::info!(
            "已发送注册信息 node_id={} name={} model={} soc={} android={} ip={}",
            self.node_id,
            self.name,
            info["model"].as_str().unwrap_or(""),
            info["soc"].as_str().unwrap_or(""),
            info["android_version"].as_str().unwrap_or(""),
            self.ip
        );
    }

    fn accept_device_token(&self, msg: &serde_json::Value) {
        let token = match msg.get("device_token").and_then(|t| t.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => return,
        };
        let ack_node = msg.get("node_id").and_then(|n| n.as_str()).unwrap_or("").to_string();
        if !ack_node.is_empty() && ack_node != self.node_id {
            log::warn!(
                "register_ack 的 node_id 与本地不一致，拒绝保存设备 token ack={} local={}",
                ack_node,
                self.node_id
            );
            return;
        }
        let mut cur = self.device_token.lock().unwrap();
        if token == *cur {
            return;
        }
        *cur = token.clone();
        drop(cur);
        save_device_token(&self.node_id, &token);
        log::info!("已接收平台签发的设备 token（此后握手将改用它）node_id={}", self.node_id);
    }

    // ================================================================== 心跳

    async fn heartbeat_loop(self: Arc<Self>, sink: Arc<WsSink>, session_started: Instant) {
        let interval = self.cfg.agent.heartbeat_interval.max(1);
        loop {
            let t0 = Instant::now();
            let collector = self.collector.clone();
            let metrics = tokio::task::spawn_blocking(move || {
                let mut c = collector.lock().unwrap();
                c.collect()
            })
            .await
            .unwrap_or_else(|_| serde_json::json!({}));

            let services = self.service_states();
            let msg = serde_json::json!({
                "type": "heartbeat",
                "node_id": self.node_id,
                "ts": now_epoch(),
                "payload": {
                    "node_id": self.node_id,
                    "ip": self.ip,
                    "name": self.name,
                    "uptime_seconds": session_started.elapsed().as_secs() as i64,
                    "metrics": metrics,
                    "services": services,
                },
            });
            if sink.lock().await.send(Message::text(msg.to_string())).await.is_err() {
                return;
            }
            log::debug!(
                "心跳已发送 cpu={}% mem={}/{} temp={}",
                metrics.get("cpu_usage").and_then(|v| v.as_f64()).unwrap_or(0.0),
                human_bytes(metrics.get("mem_used").and_then(|v| v.as_i64())),
                human_bytes(metrics.get("mem_total").and_then(|v| v.as_i64())),
                metrics.get("temperature").and_then(|v| v.as_f64()).unwrap_or(0.0),
            );
            let elapsed = t0.elapsed().as_secs_f64();
            tokio::time::sleep(Duration::from_secs_f64((interval as f64 - elapsed).max(0.2))).await;
        }
    }

    // ================================================================== 收消息

    async fn recv_loop(self: Arc<Self>, mut stream: futures_util::stream::SplitStream<WsStream>, sink: Arc<WsSink>) -> bool {
        while let Some(item) = stream.next().await {
            let msg = match item {
                Ok(m) => m,
                Err(_) => return false,
            };
            if let Message::Text(text) = msg {
                let parsed: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        log::warn!("收到非法 JSON，已忽略");
                        continue;
                    }
                };
                if !parsed.is_object() {
                    continue;
                }
                self.handle_message(&sink, parsed).await;
            }
        }
        true
    }

    async fn handle_message(&self, sink: &Arc<WsSink>, msg: serde_json::Value) {
        let mtype = msg.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let cmd_id = msg.get("cmd_id").and_then(|c| c.as_str()).unwrap_or("").to_string();

        if !cmd_id.is_empty() {
            let duplicate = {
                let mut handled = self.handled_cmds.lock().unwrap();
                if handled.contains_key(&cmd_id) {
                    true
                } else {
                    handled.insert(cmd_id.clone(), now_epoch());
                    if handled.len() > 512 {
                        let mut keys: Vec<String> = handled.keys().cloned().collect();
                        keys.sort_by(|a, b| handled[a].partial_cmp(&handled[b]).unwrap());
                        for k in keys.into_iter().take(256) {
                            handled.remove(&k);
                        }
                    }
                    false
                }
            };
            if duplicate {
                log::info!("收到重复指令（已忽略副作用）type={} cmd_id={}", mtype, cmd_id);
                self.reply(sink, &cmd_id, true, "", Some(serde_json::json!({"duplicate": true}))).await;
                return;
            }
        }

        match mtype.as_str() {
            "register_ack" => {
                log::info!(
                    "平台确认注册成功 node_id={} server_version={}",
                    msg.get("node_id").and_then(|n| n.as_str()).unwrap_or(""),
                    msg.get("server_version").and_then(|s| s.as_str()).unwrap_or("")
                );
                self.accept_device_token(&msg);
            }
            "heartbeat_ack" => {}
            "ping" => {
                let _ = sink
                    .lock()
                    .await
                    .send(Message::text(serde_json::json!({"type": "pong", "ts": now_epoch()}).to_string()))
                    .await;
            }
            "collect_metrics" => {
                let collector = self.collector.clone();
                let metrics = tokio::task::spawn_blocking(move || {
                    let mut c = collector.lock().unwrap();
                    c.collect()
                })
                .await
                .unwrap_or_else(|_| serde_json::json!({}));
                self.reply(sink, &cmd_id, true, "", Some(serde_json::json!({"metrics": metrics}))).await;
            }
            "simulate" => {
                self.handle_simulate(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({}))).await;
            }
            "deploy_model" => {
                self.deploy_model(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({}))).await;
            }
            "cancel_deploy" => {
                let tid = msg
                    .get("payload")
                    .and_then(|p| p.get("task_id"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if !tid.is_empty() {
                    let mut set = self.cancelled_tasks.lock().unwrap();
                    set.insert(tid.clone());
                    if set.len() > 256 {
                        let all: Vec<String> = set.iter().cloned().collect();
                        *set = all.into_iter().skip(128).collect();
                    }
                    log::warn!("收到取消指令 task={}（将在下一个阶段边界中止）", tid);
                }
                self.reply(sink, &cmd_id, true, "", Some(serde_json::json!({"task_id": tid, "cancelled": true}))).await;
            }
            "start_service" => {
                self.service_command(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({})), true).await;
            }
            "stop_service" | "restart_service" => {
                let start = mtype == "restart_service";
                self.service_command(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({})), start).await;
            }
            "service_logs" => {
                self.service_logs(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({}))).await;
            }
            "remove_service" => {
                let sid = msg.get("payload").and_then(|p| p.get("service_id")).and_then(|s| s.as_str()).unwrap_or("").to_string();
                let rec = self.services.lock().unwrap().remove(&sid);
                if let Some(mut rec) = rec {
                    if rec.exit_status.is_none() {
                        if let Some(mut proc) = rec.proc.take() {
                            let _ = self.terminate_child(&mut proc).await;
                        }
                    }
                }
                self.persist_services();
                self.reply(sink, &cmd_id, true, "", Some(serde_json::json!({"service_id": sid, "removed": true}))).await;
            }
            "list_services" => {
                let services = self.service_states();
                self.reply(sink, &cmd_id, true, "", Some(serde_json::json!({"services": services}))).await;
            }
            t if FS_COMMANDS.contains(&t) => {
                self.fs_op(sink, &cmd_id, t, msg.get("payload").cloned().unwrap_or(serde_json::json!({}))).await;
            }
            "fs_fetch" => {
                self.fs_fetch(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({}))).await;
            }
            "fs_push" => {
                self.fs_push(sink, &cmd_id, msg.get("payload").cloned().unwrap_or(serde_json::json!({}))).await;
            }
            other => {
                log::warn!("收到暂不支持的指令 type={}", other);
                if !cmd_id.is_empty() {
                    self.reply(sink, &cmd_id, false, &format!("不支持的指令类型: {other}"), None).await;
                }
            }
        }
    }

    async fn reply(&self, sink: &Arc<WsSink>, cmd_id: &str, ok: bool, error: &str, extra: Option<serde_json::Value>) {
        let mut payload = serde_json::json!({"type": "ack", "cmd_id": cmd_id, "ok": ok});
        if !error.is_empty() {
            payload["error"] = serde_json::json!(error);
        }
        if let Some(extra) = extra {
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    payload[k] = v.clone();
                }
            }
        }
        let _ = sink.lock().await.send(Message::text(payload.to_string())).await;
    }

    async fn send_progress(&self, sink: &Arc<WsSink>, task_id: Option<&str>, state: &Arc<Mutex<(i64, i64)>>, phase: &str) {
        let task_id = match task_id {
            Some(t) => t,
            None => return,
        };
        let (done, size) = *state.lock().unwrap();
        let msg = serde_json::json!({
            "type": "progress",
            "ts": now_epoch(),
            "payload": {
                "task_id": task_id,
                "node_id": self.node_id,
                "bytes_downloaded": done,
                "bytes_total": if size > 0 { serde_json::json!(size) } else { serde_json::json!(null) },
                "percent": if size > 0 { serde_json::json!(((done as f64 * 100.0 / size as f64) * 10.0).round() / 10.0) } else { serde_json::json!(null) },
                "phase": if phase.is_empty() { "downloading" } else { phase },
            },
        });
        let _ = sink.lock().await.send(Message::text(msg.to_string())).await;
    }

    async fn send_transfer_progress(&self, sink: &Arc<WsSink>, transfer_id: &str, state: &Arc<Mutex<(i64, i64)>>) {
        if transfer_id.is_empty() {
            return;
        }
        let (done, size) = *state.lock().unwrap();
        let msg = serde_json::json!({
            "type": "transfer_progress",
            "ts": now_epoch(),
            "payload": {
                "transfer_id": transfer_id,
                "node_id": self.node_id,
                "bytes_done": done,
                "bytes_total": if size > 0 { serde_json::json!(size) } else { serde_json::json!(null) },
                "percent": if size > 0 { serde_json::json!(((done as f64 * 100.0 / size as f64) * 10.0).round() / 10.0) } else { serde_json::json!(null) },
            },
        });
        let _ = sink.lock().await.send(Message::text(msg.to_string())).await;
    }

    // ================================================================== 服务管理

    fn service_log_path(&self, sid: &str) -> PathBuf {
        let d = self.cfg.logs_path().join("services");
        let _ = fs::create_dir_all(&d);
        d.join(format!("{sid}.log"))
    }

    fn spawn_service(&self, sid: &str, command: &str, args: &[String]) -> std::io::Result<tokio::process::Child> {
        let log_path = self.service_log_path(sid);
        let file = OpenOptions::new().create(true).append(true).open(&log_path)?;
        let stdout = Stdio::from(file.try_clone()?);
        let stderr = Stdio::from(file);
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args).stdout(stdout).stderr(stderr);
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.spawn()
    }

    pub(crate) fn persist_services(&self) {
        let map = self.services.lock().unwrap();
        let mut saved = HashMap::new();
        for (sid, rec) in map.iter() {
            saved.insert(
                sid.clone(),
                SavedService {
                    command: rec.command.clone(),
                    args: rec.args.clone(),
                    auto_restart: rec.auto_restart,
                    auto_start: rec.auto_start,
                    restart_count: rec.restart_count,
                    pid: rec.proc.as_ref().and_then(|p| p.id()).map(|i| i as i64).unwrap_or(0),
                },
            );
        }
        drop(map);
        save_services(&self.node_id, &saved);
    }

    fn service_states(&self) -> Vec<serde_json::Value> {
        let mut map = self.services.lock().unwrap();
        let mut out = Vec::new();
        for (sid, rec) in map.iter_mut() {
            let alive = rec.alive();
            let pid = if alive { rec.proc.as_ref().and_then(|p| p.id()).map(|i| i as i64) } else { None };
            let exit_code = if alive { None } else { rec.exit_status.map(|c| c as i64) };
            out.push(serde_json::json!({
                "service_id": sid,
                "status": if alive { "running" } else { "stopped" },
                "pid": pid,
                "exit_code": exit_code,
                "started_at": rec.started_at,
                "auto_restart": rec.auto_restart,
                "restart_count": rec.restart_count,
            }));
        }
        out
    }

    async fn restore_services(&self) -> i64 {
        let saved = config::load_services(&self.node_id);
        if saved.is_empty() {
            return 0;
        }
        let mut restored = 0;
        for (sid, rec) in saved {
            if !rec.auto_start {
                continue;
            }
            if rec.command.is_empty() {
                continue;
            }
            if rec.pid > 0 && self.kill_leftover(rec.pid, &rec.command, &rec.args).await {
                log::warn!("已清理服务 {} 上次遗留的进程 pid={}", sid, rec.pid);
            }
            let proc = match self.spawn_service(&sid, &rec.command, &rec.args) {
                Ok(p) => p,
                Err(e) => {
                    log::error!("服务 {} 开机恢复失败 err={}", sid, e);
                    continue;
                }
            };
            self.services.lock().unwrap().insert(
                sid.clone(),
                ServiceRecord {
                    proc: Some(proc),
                    command: rec.command.clone(),
                    args: rec.args.clone(),
                    started_at: now_epoch(),
                    auto_restart: rec.auto_restart,
                    auto_start: true,
                    restart_count: rec.restart_count,
                    exit_status: None,
                },
            );
            restored += 1;
            log::warn!("服务 {} 已按 Auto Start 恢复 pid={}", sid, 0);
        }
        if restored > 0 {
            self.persist_services();
        }
        restored
    }

    async fn service_watchdog(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let mut to_respawn: Vec<(String, String, Vec<String>, bool, i64)> = Vec::new();
            {
                let mut map = self.services.lock().unwrap();
                for (sid, rec) in map.iter_mut() {
                    if !rec.auto_restart {
                        continue;
                    }
                    if rec.alive() {
                        continue;
                    }
                    if rec.command.is_empty() {
                        continue;
                    }
                    log::warn!("服务 {} 已退出，自动拉起", sid);
                    rec.restart_count += 1;
                    to_respawn.push((sid.clone(), rec.command.clone(), rec.args.clone(), rec.auto_start, rec.restart_count));
                }
            }
            let had_respawn = !to_respawn.is_empty();
            for (sid, command, args, auto_start, restart_count) in to_respawn {
                match self.spawn_service(&sid, &command, &args) {
                    Ok(proc) => {
                        self.services.lock().unwrap().insert(
                            sid.clone(),
                            ServiceRecord {
                                proc: Some(proc),
                                command,
                                args,
                                started_at: now_epoch(),
                                auto_restart: true,
                                auto_start,
                                restart_count,
                                exit_status: None,
                            },
                        );
                        log::warn!("服务 {} 已重启（第 {} 次）", sid, restart_count);
                    }
                    Err(e) => log::error!("服务 {} 自动拉起失败 err={}", sid, e),
                }
            }
            if had_respawn {
                self.persist_services();
            }
        }
    }

    async fn kill_leftover(&self, pid: i64, command: &str, args: &[String]) -> bool {
        if pid <= 0 {
            return false;
        }
        let live = match proc_cmdline(pid) {
            Some(v) => v,
            None => return false,
        };
        let mut want = vec![command.to_string()];
        want.extend(args.iter().cloned());
        let live_name = Path::new(&live[0]).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let want_name = Path::new(&want[0]).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if live_name != want_name || live[1..] != want[1..] {
            return false;
        }
        if !kill_pid_sigterm(pid as i32) {
            return false;
        }
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if proc_cmdline(pid).is_none() {
                return true;
            }
        }
        kill_pid_sigkill(pid as i32);
        true
    }

    async fn service_command(&self, sink: &Arc<WsSink>, cmd_id: &str, payload: serde_json::Value, start: bool) {
        let sid = payload.get("service_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if sid.is_empty() {
            self.reply(sink, cmd_id, false, "缺少 service_id", None).await;
            return;
        }
        // 先停（stop / restart 都会走到这里）
        let rec = self.services.lock().unwrap().remove(&sid);
        if let Some(mut rec) = rec {
            if rec.exit_status.is_none() {
                if let Some(mut proc) = rec.proc.take() {
                    let _ = self.terminate_child(&mut proc).await;
                }
            }
        }
        if !start {
            self.persist_services();
            self.reply(sink, cmd_id, true, "", Some(serde_json::json!({"service_id": sid, "status": "stopped"}))).await;
            return;
        }
        let command = payload.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let args: Vec<String> = payload
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if command.is_empty() {
            self.reply(sink, cmd_id, false, "缺少 command", None).await;
            return;
        }
        let proc = match self.spawn_service(&sid, &command, &args) {
            Ok(p) => p,
            Err(e) => {
                self.reply(sink, cmd_id, false, &format!("启动失败: {e}"), Some(serde_json::json!({"service_id": sid, "status": "error"}))).await;
                return;
            }
        };
        let log_path = self.service_log_path(&sid);
        let auto_restart = payload.get("auto_restart").and_then(|v| v.as_bool()).unwrap_or(false);
        let auto_start = payload.get("auto_start").and_then(|v| v.as_bool()).unwrap_or(false);
        self.services.lock().unwrap().insert(
            sid.clone(),
            ServiceRecord {
                proc: Some(proc),
                command,
                args,
                started_at: now_epoch(),
                auto_restart,
                auto_start,
                restart_count: 0,
                exit_status: None,
            },
        );
        self.persist_services();
        self.reply(
            sink,
            cmd_id,
            true,
            "",
            Some(serde_json::json!({"service_id": sid, "status": "running", "log_path": log_path.to_string_lossy()})),
        )
        .await;
    }

    async fn service_logs(&self, sink: &Arc<WsSink>, cmd_id: &str, payload: serde_json::Value) {
        let sid = payload.get("service_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if sid.is_empty() {
            self.reply(sink, cmd_id, false, "缺少 service_id", None).await;
            return;
        }
        let lines = payload.get("lines").and_then(|l| l.as_i64()).unwrap_or(200).clamp(1, 5000) as usize;
        let path = self.service_log_path(&sid);
        if !path.exists() {
            self.reply(
                sink,
                cmd_id,
                true,
                "",
                Some(serde_json::json!({"service_id": sid, "lines": [], "total_bytes": 0, "note": "尚无日志（服务可能还没启动过）"})),
            )
            .await;
            return;
        }
        match fs::read(&path) {
            Ok(raw) => {
                let text = String::from_utf8_lossy(&raw);
                let tail: Vec<&str> = text.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect();
                let status = {
                    let mut map = self.services.lock().unwrap();
                    map.get_mut(&sid).map(|r| if r.alive() { "running" } else { "stopped" }).unwrap_or("stopped")
                };
                self.reply(
                    sink,
                    cmd_id,
                    true,
                    "",
                    Some(serde_json::json!({"service_id": sid, "lines": tail, "total_bytes": raw.len(), "status": status})),
                )
                .await;
            }
            Err(e) => {
                self.reply(sink, cmd_id, false, &format!("读取日志失败: {e}"), Some(serde_json::json!({"service_id": sid}))).await;
            }
        }
    }

    // ================================================================== 文件管理

    async fn fs_op(&self, sink: &Arc<WsSink>, cmd_id: &str, mtype: &str, payload: serde_json::Value) {
        let files = self.files.clone();
        let mtype = mtype.to_string();
        let payload = payload.clone();
        let result: Result<serde_json::Value, FileError> = tokio::task::spawn_blocking(move || {
            run_fs_op(&files, &mtype, &payload)
        })
        .await
        .unwrap_or_else(|e| Err(FileError(format!("操作失败：{e}"))));
        match result {
            Ok(result) => self.reply(sink, cmd_id, true, "", Some(serde_json::json!({"result": result}))).await,
            Err(e) => self.reply(sink, cmd_id, false, &e.0, None).await,
        }
    }

    async fn fs_fetch(&self, sink: &Arc<WsSink>, cmd_id: &str, payload: serde_json::Value) {
        let transfer_id = payload.get("transfer_id").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let url = payload.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        let token = payload.get("token").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if transfer_id.is_empty() || url.is_empty() {
            self.reply(sink, cmd_id, false, "缺少传输参数", None).await;
            return;
        }
        let path = payload.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
        let files = self.files.clone();
        let (target, size) = match tokio::task::spawn_blocking(move || files.open_read(&path)).await.unwrap_or_else(|e| Err(FileError(format!("操作失败：{e}")))) {
            Ok(x) => x,
            Err(e) => {
                self.reply(sink, cmd_id, false, &e.0, None).await;
                return;
            }
        };

        let state = Arc::new(Mutex::new((0i64, size)));
        let transfer_id2 = transfer_id.clone();
        let url2 = url.clone();
        let token2 = token.clone();
        let target2 = target.clone();
        let state2 = state.clone();
        let handle = tokio::task::spawn_blocking(move || upload_blocking(&url2, &token2, &target2, size, &state2));

        self.await_transfer_progress(sink, &transfer_id, &state, &handle).await;
        match handle.await {
            Ok(Ok((total, digest))) => {
                self.reply(
                    sink,
                    cmd_id,
                    true,
                    "",
                    Some(serde_json::json!({"transfer_id": transfer_id, "bytes": total, "sha256": digest, "direction": "pull", "path": target.to_string_lossy()})),
                )
                .await;
            }
            _ => {
                log::warn!("文件上传失败 transfer={} path={}", transfer_id2, target.to_string_lossy());
                self.reply(sink, cmd_id, false, "上传失败", Some(serde_json::json!({"transfer_id": transfer_id2, "direction": "pull"}))).await;
            }
        }
    }

    async fn fs_push(&self, sink: &Arc<WsSink>, cmd_id: &str, payload: serde_json::Value) {
        let transfer_id = payload.get("transfer_id").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let url = payload.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        let token = payload.get("token").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let expected = payload.get("sha256").and_then(|s| s.as_str()).unwrap_or("").to_lowercase();
        let size = payload.get("size").and_then(|s| s.as_i64()).unwrap_or(0);
        if transfer_id.is_empty() || url.is_empty() {
            self.reply(sink, cmd_id, false, "缺少传输参数", None).await;
            return;
        }
        let path = payload.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
        let files = self.files.clone();
        let target = match tokio::task::spawn_blocking(move || files.prepare_write(&path, size)).await.unwrap_or_else(|e| Err(FileError(format!("操作失败：{e}")))) {
            Ok(t) => t,
            Err(e) => {
                self.reply(sink, cmd_id, false, &e.0, Some(serde_json::json!({"transfer_id": transfer_id, "direction": "push"}))).await;
                return;
            }
        };

        let temp = target.with_extension(format!("{}part", target.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()));
        let state = Arc::new(Mutex::new((0i64, size)));
        let transfer_id2 = transfer_id.clone();
        let url2 = url.clone();
        let token2 = token.clone();
        let temp2 = temp.clone();
        let state2 = state.clone();
        let handle = tokio::task::spawn_blocking(move || download_blocking(&url2, &token2, &temp2, &state2, false));

        self.await_transfer_progress(sink, &transfer_id, &state, &handle).await;
        let result = handle.await;
        match result {
            Ok(Ok((total, digest))) => {
                if !expected.is_empty() && digest != expected {
                    let _ = fs::remove_file(&temp);
                    self.reply(sink, cmd_id, false, "SHA-256 校验失败，文件已丢弃", Some(serde_json::json!({"transfer_id": transfer_id2, "direction": "push"}))).await;
                    return;
                }
                if let Err(e) = fs::rename(&temp, &target) {
                    let _ = fs::remove_file(&temp);
                    self.reply(sink, cmd_id, false, &format!("下载失败：{e}"), Some(serde_json::json!({"transfer_id": transfer_id2, "direction": "push"}))).await;
                    return;
                }
                self.reply(
                    sink,
                    cmd_id,
                    true,
                    "",
                    Some(serde_json::json!({"transfer_id": transfer_id2, "bytes": total, "sha256": digest, "direction": "push", "path": target.to_string_lossy()})),
                )
                .await;
            }
            _ => {
                let _ = fs::remove_file(&temp);
                self.reply(sink, cmd_id, false, "下载失败", Some(serde_json::json!({"transfer_id": transfer_id2, "direction": "push"}))).await;
            }
        }
    }

    async fn await_transfer_progress(
        &self,
        sink: &Arc<WsSink>,
        transfer_id: &str,
        state: &Arc<Mutex<(i64, i64)>>,
        handle: &tokio::task::JoinHandle<Result<(i64, String), String>>,
    ) {
        let mut last = 0.0f64;
        while !handle.is_finished() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if now_epoch() - last >= 1.0 {
                last = now_epoch();
                self.send_transfer_progress(sink, transfer_id, state).await;
            }
        }
        self.send_transfer_progress(sink, transfer_id, state).await;
    }

    // ================================================================== 部署

    async fn deploy_model(&self, sink: &Arc<WsSink>, cmd_id: &str, payload: serde_json::Value) {
        let url = payload.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        let name = Path::new(payload.get("target").and_then(|t| t.as_str()).unwrap_or("model.bin"))
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "model.bin".to_string());
        let expected = payload.get("sha256").and_then(|s| s.as_str()).unwrap_or("").to_lowercase();
        let size_hint = payload.get("size").and_then(|s| s.as_i64()).unwrap_or(0);
        let task_id = payload.get("task_id").and_then(|t| t.as_str()).map(String::from);
        let dl_token = payload.get("download_token").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if url.is_empty() {
            self.reply(sink, cmd_id, false, "缺少下载地址", None).await;
            return;
        }
        let manifest = payload
            .get("manifest")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        let self_test = payload
            .get("self_test")
            .and_then(|s| s.as_object())
            .filter(|o| o.get("command").and_then(|c| c.as_str()).is_some())
            .cloned();

        let models_dir = self.cfg.models_path();
        let _ = fs::create_dir_all(&models_dir);
        let target = models_dir.join(&name);
        let temp = models_dir.join(format!("{name}.part"));

        let mut journal = InstallJournal::new(&models_dir.join(".rollback"), task_id.as_deref());
        let stale_note = match journal.prepare() {
            Ok(n) => n,
            Err(e) => {
                log::error!("无法建立回滚点，部署中止 task={:?} err={}", task_id, e);
                self.reply(
                    sink,
                    cmd_id,
                    false,
                    &format!("无法建立回滚点：{e}"),
                    Some(serde_json::json!({"status": "failed", "task_id": task_id, "rolled_back": false, "rollback_note": ""})),
                )
                .await;
                return;
            }
        };
        if !stale_note.is_empty() {
            log::warn!("部署前先收拾了上一轮的遗留 task={:?}：{}", task_id, stale_note);
        }
        if let Some(tid) = &task_id {
            self.cancelled_tasks.lock().unwrap().remove(tid);
        }

        let state = Arc::new(Mutex::new((0i64, size_hint)));
        let result: Result<(i64, String, Vec<serde_json::Value>, String), DeployError> =
            self.run_deploy(sink, task_id.as_deref(), &url, &dl_token, &temp, &state, size_hint, &expected, &target, &name, &manifest, self_test.as_ref(), &models_dir, &mut journal).await;

        match result {
            Ok((total, digest, assets, test_output)) => {
                journal.commit();
                self.reply(
                    sink,
                    cmd_id,
                    true,
                    "",
                    Some(serde_json::json!({
                        "status": "ready",
                        "bytes_downloaded": total,
                        "sha256": digest,
                        "target": target.to_string_lossy(),
                        "assets": assets,
                        "self_test_output": test_output,
                        "task_id": task_id,
                        "rolled_back": false,
                        "rollback_note": "",
                    })),
                )
                .await;
            }
            Err(DeployError::Cancelled(msg)) => {
                let _ = fs::remove_file(&temp);
                let (rb, note) = journal.rollback();
                self.reply(
                    sink,
                    cmd_id,
                    false,
                    &msg,
                    Some(serde_json::json!({"status": "cancelled", "task_id": task_id, "rolled_back": rb, "rollback_note": note})),
                )
                .await;
            }
            Err(DeployError::Failed(msg)) => {
                let _ = fs::remove_file(&temp);
                let (rb, note) = journal.rollback();
                self.reply(
                    sink,
                    cmd_id,
                    false,
                    &msg,
                    Some(serde_json::json!({"status": "failed", "task_id": task_id, "rolled_back": rb, "rollback_note": note})),
                )
                .await;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_deploy(
        &self,
        sink: &Arc<WsSink>,
        task_id: Option<&str>,
        url: &str,
        dl_token: &str,
        temp: &Path,
        state: &Arc<Mutex<(i64, i64)>>,
        size_hint: i64,
        expected: &str,
        target: &Path,
        name: &str,
        manifest: &[serde_json::Value],
        self_test: Option<&serde_json::Map<String, serde_json::Value>>,
        models_dir: &Path,
        journal: &mut InstallJournal,
    ) -> Result<(i64, String, Vec<serde_json::Value>, String), DeployError> {
        // 下载
        let (total, digest) = self
            .download_model_file(sink, task_id, url, temp, state, dl_token)
            .await
            .map_err(DeployError::Failed)?;
        self.raise_if_cancelled(task_id)?;

        // 校验
        self.send_progress(sink, task_id, state, "verifying").await;
        if !expected.is_empty() && digest != expected {
            let _ = fs::remove_file(temp);
            return Err(DeployError::Failed(format!(
                "SHA-256 校验失败（期望 {}… 实际 {}…）",
                &expected[..expected.len().min(12)],
                &digest[..digest.len().min(12)]
            )));
        }
        if size_hint > 0 && total != size_hint {
            log::warn!("模型大小与登记不符 task={:?} 登记={} 实际={}", task_id, size_hint, total);
        }

        // 落位
        self.send_progress(sink, task_id, state, "installing").await;
        if let Err(e) = journal.capture(target) {
            return Err(DeployError::Failed(format!("{e}")));
        }
        if let Err(e) = fs::rename(temp, target) {
            return Err(DeployError::Failed(format!("落位失败：{e}")));
        }
        self.raise_if_cancelled(task_id)?;

        // 附属物
        let assets = self.fetch_manifest(sink, task_id, manifest, models_dir, dl_token, Some(journal)).await;
        self.raise_if_cancelled(task_id)?;

        // Self Test
        let mut test_output = String::new();
        if let Some(spec) = self_test {
            self.send_progress(sink, task_id, state, "testing").await;
            let command = spec.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let (ok, out) = self.run_self_test(spec, target).await;
            if !ok {
                return Err(DeployError::Failed(format!(
                    "Self Test 未通过（{command}）：{}",
                    &out[..out.len().min(300)]
                )));
            }
            test_output = out;
        }

        let _ = name;
        Ok((total, digest, assets, test_output))
    }

    fn raise_if_cancelled(&self, task_id: Option<&str>) -> Result<(), DeployError> {
        if let Some(tid) = task_id {
            if self.cancelled_tasks.lock().unwrap().contains(tid) {
                return Err(DeployError::Cancelled(format!("部署已被用户取消（task={tid}）")));
            }
        }
        Ok(())
    }

    async fn download_model_file(
        &self,
        sink: &Arc<WsSink>,
        task_id: Option<&str>,
        url: &str,
        temp: &Path,
        state: &Arc<Mutex<(i64, i64)>>,
        dl_token: &str,
    ) -> Result<(i64, String), String> {
        let url2 = url.to_string();
        let dl2 = dl_token.to_string();
        let temp2 = temp.to_path_buf();
        let state2 = state.clone();
        let handle = tokio::task::spawn_blocking(move || download_blocking(&url2, &dl2, &temp2, &state2, true));
        let mut last = 0.0f64;
        while !handle.is_finished() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if now_epoch() - last >= 1.0 {
                last = now_epoch();
                self.send_progress(sink, task_id, state, "").await;
            }
        }
        self.send_progress(sink, task_id, state, "").await;
        match handle.await {
            Ok(r) => r,
            Err(_) => Err("下载失败".to_string()),
        }
    }

    async fn fetch_manifest(
        &self,
        sink: &Arc<WsSink>,
        task_id: Option<&str>,
        manifest: &[serde_json::Value],
        models_dir: &Path,
        dl_token: &str,
        mut journal: Option<&mut InstallJournal>,
    ) -> Vec<serde_json::Value> {
        let _ = sink;
        let mut out = Vec::new();
        for item in manifest {
            let item = match item.as_object() {
                Some(o) => o,
                None => continue,
            };
            let aname = item.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let aname = Path::new(&aname).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let aurl = item.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
            if aname.is_empty() || aurl.is_empty() {
                continue;
            }
            let dest = models_dir.join(&aname);
            let tmp = models_dir.join(format!("{aname}.part"));
            let want = item.get("sha256").and_then(|s| s.as_str()).unwrap_or("").to_lowercase();
            let dl2 = dl_token.to_string();
            let aurl2 = aurl.clone();
            let tmp2 = tmp.clone();
            let state = Arc::new(Mutex::new((0i64, 0i64)));
            let handle = tokio::task::spawn_blocking(move || download_blocking(&aurl2, &dl2, &tmp2, &state, false));
            let result = handle.await.unwrap_or_else(|_| Err("拉取失败".to_string()));
            match result {
                Ok((size, got)) => {
                    if !want.is_empty() && got != want {
                        let _ = fs::remove_file(&tmp);
                        out.push(serde_json::json!({"name": aname.clone(), "ok": false, "error": "SHA-256 校验失败", "kind": item.get("kind").and_then(|k| k.as_str()).unwrap_or("other")}));
                        log::warn!("附属物拉取失败（不阻断部署）task={:?} name={} err=SHA-256 校验失败", task_id, aname);
                        continue;
                    }
                    if let Some(j) = journal.as_deref_mut() {
                        if let Err(e) = j.capture(&dest) {
                            let _ = fs::remove_file(&tmp);
                            out.push(serde_json::json!({"name": aname.clone(), "ok": false, "error": e.to_string(), "kind": item.get("kind").and_then(|k| k.as_str()).unwrap_or("other")}));
                            continue;
                        }
                    }
                    if let Err(e) = fs::rename(&tmp, &dest) {
                        let _ = fs::remove_file(&tmp);
                        out.push(serde_json::json!({"name": aname.clone(), "ok": false, "error": e.to_string(), "kind": item.get("kind").and_then(|k| k.as_str()).unwrap_or("other")}));
                        continue;
                    }
                    out.push(serde_json::json!({"name": aname.clone(), "ok": true, "size": size, "kind": item.get("kind").and_then(|k| k.as_str()).unwrap_or("other")}));
                    log::info!("附属物已落位 task={:?} name={} size={}", task_id, aname, size);
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    out.push(serde_json::json!({"name": aname.clone(), "ok": false, "error": e, "kind": item.get("kind").and_then(|k| k.as_str()).unwrap_or("other")}));
                    log::warn!("附属物拉取失败（不阻断部署）task={:?} name={} err={}", task_id, aname, "");
                }
            }
        }
        out
    }

    async fn run_self_test(&self, spec: &serde_json::Map<String, serde_json::Value>, model_path: &Path) -> (bool, String) {
        let command = spec
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .replace("{model_path}", &model_path.to_string_lossy());
        let timeout = spec.get("timeout").and_then(|t| t.as_i64()).unwrap_or(60).max(1);
        let mut cmd = shell_command(&command);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return (false, format!("Self Test 无法执行：{e}")),
        };
        let pid = child.id();
        match tokio::time::timeout(Duration::from_secs(timeout as u64), child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let combined = if stderr.is_empty() { stdout } else { format!("{stdout}\n{stderr}") }.trim().to_string();
                if output.status.success() {
                    (true, if combined.is_empty() { "OK".to_string() } else { combined })
                } else {
                    (false, format!("退出码 {}：{combined}", output.status.code().unwrap_or(-1)))
                }
            }
            Ok(Err(e)) => (false, format!("Self Test 无法执行：{e}")),
            Err(_) => {
                #[cfg(unix)]
                if let Some(pid) = pid {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
                #[cfg(not(unix))]
                let _ = pid;
                (false, format!("Self Test 超时（{timeout}s）"))
            }
        }
    }

    // ================================================================== 模拟

    async fn handle_simulate(&self, sink: &Arc<WsSink>, cmd_id: &str, payload: serde_json::Value) {
        let action = payload.get("action").and_then(|a| a.as_str()).unwrap_or("").to_string();
        log::warn!("收到模拟指令 action={}", action);
        match action.as_str() {
            "disconnect" => {
                self.reply(sink, cmd_id, true, "", Some(serde_json::json!({"action": action}))).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
                log::warn!("模拟断线：主动关闭 WebSocket");
                let _ = sink.lock().await.close().await;
            }
            "crash" => {
                log::warn!("模拟进程崩溃：3 秒后退出进程（由外部脚本重启）");
                self.reply(sink, cmd_id, true, "", Some(serde_json::json!({"action": action}))).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
                std::process::exit(9);
            }
            other => {
                self.reply(sink, cmd_id, false, &format!("未知模拟动作: {other}"), None).await;
            }
        }
    }

    async fn simulate_loop(self: Arc<Self>, sink: Arc<WsSink>) {
        let seconds = self.cfg.simulate.disconnect_after;
        if !self.simulate || seconds <= 0 {
            std::future::pending::<()>().await;
            return;
        }
        tokio::time::sleep(Duration::from_secs(seconds as u64)).await;
        log::warn!("触发配置的模拟断线（disconnect_after={}s）", seconds);
        let _ = sink.lock().await.close().await;
    }
}

// ====================================================================== 阻塞辅助

fn run_fs_op(files: &Arc<FileService>, mtype: &str, payload: &serde_json::Value) -> Result<serde_json::Value, FileError> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match mtype {
        "fs_roots" => Ok(serde_json::json!({"roots": files.roots_json()})),
        "fs_list" => files.list_dir(
            &s("path"),
            payload.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as usize,
            payload.get("limit").and_then(|v| v.as_i64()).unwrap_or(500) as usize,
        ),
        "fs_stat" => files.stat(&s("path")),
        "fs_du" => files.du(&s("path")),
        "fs_mkdir" => files.mkdir(&s("path"), payload.get("parents").and_then(|v| v.as_bool()).unwrap_or(true)),
        "fs_delete" => files.delete(&s("path"), payload.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false)),
        "fs_copy" | "fs_move" => files.transfer(
            &s("src"),
            &s("dst"),
            mtype == "fs_move",
            payload.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false),
        ),
        other => Err(FileError(format!("未知的文件操作 {other}"))),
    }
}

/// 下载到文件（含续传、sha256）。resume 表示是否支持断点续传。
fn download_blocking(
    url: &str,
    token: &str,
    temp: &Path,
    state: &Arc<Mutex<(i64, i64)>>,
    resume: bool,
) -> Result<(i64, String), String> {
    let mut digest = Sha256::new();
    let offset = fs::metadata(temp).map(|m| m.len()).unwrap_or(0) as i64;
    let mut req = ureq::get(url);
    if !token.is_empty() {
        req = req.set("X-Download-Token", token).set("X-Transfer-Token", token);
    }
    if resume && offset > 0 {
        req = req.set("Range", &format!("bytes={offset}-"));
    }
    let resp = req.call().map_err(|e| format!("下载失败：{e}"))?;
    // ureq 对 4xx/5xx 不会抛错，必须显式检查，否则 404 的错误 body 会被当成文件内容
    // 写进临时文件，任务从此永远卡在「下载中」。这里把 HTTP 状态码与后端给的详情一起报出去。
    let status = resp.status();
    if !(200..300).contains(&status) {
        let mut body = resp.into_string().unwrap_or_default();
        if body.len() > 200 {
            body.truncate(200);
        }
        return Err(format!("下载失败：HTTP {status}：{body}"));
    }
    let cl = resp.header("Content-Length").and_then(|s| s.parse::<i64>().ok());

    let mut total = 0i64;
    if resume && offset > 0 {
        let mut old = File::open(temp).map_err(|e| format!("打开临时文件失败：{e}"))?;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = old.read(&mut buf).map_err(|e| format!("读取失败：{e}"))?;
            if n == 0 {
                break;
            }
            digest.update(&buf[..n]);
        }
        total = offset;
        let mut st = state.lock().unwrap();
        st.1 = cl.map(|c| c + offset).unwrap_or(st.1);
        st.0 = offset;
    } else if let Some(c) = cl {
        state.lock().unwrap().1 = c;
    }

    let mut file = if resume && offset > 0 {
        OpenOptions::new().append(true).open(temp)
    } else {
        File::create(temp)
    }
    .map_err(|e| format!("打开临时文件失败：{e}"))?;

    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("写入失败：{e}"))?;
        digest.update(&buf[..n]);
        total += n as i64;
        state.lock().unwrap().0 = total;
    }
    Ok((total, hex::encode(digest.finalize())))
}

/// 上传文件（边读边算 sha256、边记进度）。
fn upload_blocking(
    url: &str,
    token: &str,
    path: &Path,
    size: i64,
    state: &Arc<Mutex<(i64, i64)>>,
) -> Result<(i64, String), String> {
    let file = File::open(path).map_err(|e| format!("打开文件失败：{e}"))?;
    let digest = Arc::new(Mutex::new(Sha256::new()));
    let body = UploadBody {
        file,
        digest: digest.clone(),
        state: state.clone(),
    };
    let req = ureq::post(url)
        .set("X-Transfer-Token", token)
        .set("Content-Type", "application/octet-stream")
        .set("Content-Length", &size.to_string())
        .set("X-File-Size", &size.to_string());
    let resp = req.send(body).map_err(|e| format!("上传失败：{e}"))?;
    // 与下载同理：后端 4xx/5xx（413 超限、401 鉴权失败等）不会让 ureq 抛错，
    // 必须显式检查，否则失败被当成成功、transfer 被误标为 done。
    let status = resp.status();
    if !(200..300).contains(&status) {
        let mut body = resp.into_string().unwrap_or_default();
        if body.len() > 200 {
            body.truncate(200);
        }
        return Err(format!("上传失败：HTTP {status}：{body}"));
    }
    // 读完响应体，确保请求真正完成
    let _ = resp.into_string();
    let done = state.lock().unwrap().0;
    let digest_hex = hex::encode(digest.lock().unwrap().finalize_reset());
    Ok((done, digest_hex))
}

struct UploadBody {
    file: File,
    digest: Arc<Mutex<Sha256>>,
    state: Arc<Mutex<(i64, i64)>>,
}

impl Read for UploadBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buf)?;
        if n > 0 {
            if let Ok(mut d) = self.digest.lock() {
                d.update(&buf[..n]);
            }
            if let Ok(mut s) = self.state.lock() {
                s.0 += n as i64;
            }
        }
        Ok(n)
    }
}

#[cfg(windows)]
fn shell_command(cmd: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("cmd");
    c.arg("/C").arg(cmd);
    c
}

#[cfg(not(windows))]
fn shell_command(cmd: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

fn proc_cmdline(pid: i64) -> Option<Vec<String>> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

#[cfg(unix)]
fn kill_pid_sigterm(pid: i32) -> bool {
    unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
}
#[cfg(not(unix))]
fn kill_pid_sigterm(_pid: i32) -> bool {
    false
}

#[cfg(unix)]
fn kill_pid_sigkill(pid: i32) -> bool {
    unsafe { libc::kill(pid, libc::SIGKILL) == 0 }
}
#[cfg(not(unix))]
fn kill_pid_sigkill(_pid: i32) -> bool {
    false
}
