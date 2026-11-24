//! 连接管理器：node_id → Transport 映射，向 Agent 下发指令。
//!
//! 指令幂等（M5）：每条指令带全局唯一 cmd_id，Agent 侧去重。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

type WsSink = SplitSink<WebSocket, Message>;

/// MVP 传输实现：Agent 主动拨出的 WebSocket（平台永不主动连手机）。
#[allow(dead_code)] // node_id/ip 为标识字段，访问器留作后续调试用
pub struct LocalWsTransport {
    node_id: String,
    ip: String,
    alive: Arc<AtomicBool>,
    sender: Arc<Mutex<WsSink>>,
}

#[allow(dead_code)] // new/node_id/ip 与 Python 版 transport 接口对齐，当前走 from_sink
impl LocalWsTransport {
    pub fn new(node_id: String, ws: WebSocket, ip: String) -> Self {
        let (sink, _stream) = ws.split();
        Self {
            node_id,
            ip,
            alive: Arc::new(AtomicBool::new(true)),
            sender: Arc::new(Mutex::new(sink)),
        }
    }

    /// 从已拆分的 sink 构造。WS handler 需要同时持有 stream 来读入站消息，
    /// 因此由 handler 先 `split()`，再把 sink 交给这里、stream 留着自己消费。
    pub fn from_sink(node_id: String, sink: WsSink, ip: String) -> Self {
        Self {
            node_id,
            ip,
            alive: Arc::new(AtomicBool::new(true)),
            sender: Arc::new(Mutex::new(sink)),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn ip(&self) -> &str {
        &self.ip
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn mark_dead(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }

    /// 发送一条 JSON 消息。失败返回 false（绝不 panic）。
    pub async fn send(&self, payload: &Value) -> bool {
        if !self.is_alive() {
            return false;
        }
        let text = payload.to_string();
        let mut sink = self.sender.lock().await;
        match sink.send(Message::Text(text.into())).await {
            Ok(()) => true,
            Err(_) => {
                self.mark_dead();
                false
            }
        }
    }

    /// 主动关闭。
    pub async fn close(&self) {
        self.mark_dead();
        let mut sink = self.sender.lock().await;
        let _ = sink.send(Message::Close(None)).await;
    }
}

pub struct ConnectionManager {
    transports: Mutex<HashMap<String, Arc<LocalWsTransport>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
}

impl ConnectionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            transports: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// 注册（或替换）一条节点连接。旧连接会被主动关闭。
    pub async fn register(&self, node_id: &str, transport: Arc<LocalWsTransport>) {
        let old = {
            let mut map = self.transports.lock().await;
            map.insert(node_id.to_string(), transport)
        };
        if let Some(old) = old {
            old.close().await;
        }
    }

    /// 注销连接。只在当前登记的正是这条 transport 时才移除。
    pub async fn unregister(&self, node_id: &str, transport: Option<&Arc<LocalWsTransport>>) {
        let removed = {
            let mut map = self.transports.lock().await;
            match transport {
                Some(t) if map.get(node_id).map(|cur| !Arc::ptr_eq(cur, t)).unwrap_or(true) => None,
                _ => map.remove(node_id),
            }
        };
        if let Some(t) = removed {
            t.close().await;
        }
    }

    /// 取节点当前 transport（若仍存活）。
    pub async fn get(&self, node_id: &str) -> Option<Arc<LocalWsTransport>> {
        let map = self.transports.lock().await;
        match map.get(node_id) {
            Some(t) if t.is_alive() => Some(t.clone()),
            _ => None,
        }
    }

    #[allow(dead_code)] // 与 Python 版对齐，当前内部以 online_node_ids 为准
    pub async fn is_connected(&self, node_id: &str) -> bool {
        self.get(node_id).await.is_some()
    }

    /// 当前持有活跃连接的节点。
    pub async fn online_node_ids(&self) -> Vec<String> {
        let map = self.transports.lock().await;
        map.iter()
            .filter(|(_, t)| t.is_alive())
            .map(|(k, _)| k.clone())
            .collect()
    }

    #[allow(dead_code)] // 与 Python 版对齐
    pub async fn connected_nodes(&self) -> Vec<String> {
        self.online_node_ids().await
    }

    pub async fn count(&self) -> usize {
        self.online_node_ids().await.len()
    }

    /// 向单个节点发消息。节点不在线返回 false。
    pub async fn send(&self, node_id: &str, payload: &Value) -> bool {
        match self.get(node_id).await {
            Some(t) => t.send(payload).await,
            None => false,
        }
    }

    /// 下发一条带 cmd_id 的指令。返回 (是否送达, cmd_id)。
    /// `wait > 0` 时等待 ack/结果，超时不算失败。
    pub async fn send_command(
        &self,
        node_id: &str,
        cmd_type: &str,
        payload: Value,
        wait: f64,
    ) -> (bool, String) {
        let cmd_id = crate::util::new_cmd_id();
        let message = serde_json::json!({
            "type": cmd_type,
            "cmd_id": cmd_id,
            "ts": epoch_secs(),
            "payload": payload,
        });
        let ok = self.send(node_id, &message).await;
        if !ok {
            return (false, cmd_id);
        }
        if wait <= 0.0 {
            return (true, cmd_id);
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(cmd_id.clone(), tx);
        }
        let _ = tokio::time::timeout(Duration::from_secs_f64(wait), rx).await;
        {
            let mut pending = self.pending.lock().await;
            pending.remove(&cmd_id);
        }
        (true, cmd_id)
    }

    /// 下发指令并取回回执内容。返回 (是否送达, cmd_id, 回执或 None)。
    pub async fn send_command_result(
        &self,
        node_id: &str,
        cmd_type: &str,
        payload: Value,
        wait: f64,
    ) -> (bool, String, Option<Value>) {
        let cmd_id = crate::util::new_cmd_id();
        let message = serde_json::json!({
            "type": cmd_type,
            "cmd_id": cmd_id,
            "ts": epoch_secs(),
            "payload": payload,
        });
        let ok = self.send(node_id, &message).await;
        if !ok {
            return (false, cmd_id, None);
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(cmd_id.clone(), tx);
        }
        let result = match tokio::time::timeout(Duration::from_secs_f64(wait), rx).await {
            Ok(Ok(v)) => Some(v),
            _ => None,
        };
        {
            let mut pending = self.pending.lock().await;
            pending.remove(&cmd_id);
        }
        (true, cmd_id, result)
    }

    /// 问一次 Agent 当前托管服务状态，返回 {service_id: {...}}。
    /// 返回 None 表示"没问到"（离线 / 超时 / Agent 不回），与空 map 严格区分。
    pub async fn fetch_service_states(&self, node_id: &str) -> Option<std::collections::HashMap<String, Value>> {
        let (_delivered, _cmd_id, result) = self
            .send_command_result(node_id, "list_services", serde_json::json!({}), 6.0)
            .await;
        let result = result?;
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return None;
        }
        let mut items = result.get("services").cloned();
        if items.is_none() {
            items = result.get("result").cloned();
        }
        if let Some(Value::Object(o)) = &items {
            if let Some(inner) = o.get("services") {
                items = Some(inner.clone());
            }
        }
        let arr = items?.as_array()?.clone();
        let mut out = std::collections::HashMap::new();
        for it in arr {
            if let Some(id) = it.get("service_id").and_then(|v| v.as_str()) {
                out.insert(id.to_string(), it);
            }
        }
        Some(out)
    }

    /// Agent 回执到达时兑现对应等待。
    pub async fn resolve_pending(&self, cmd_id: &str, result: Value) {
        let tx = {
            let mut pending = self.pending.lock().await;
            pending.remove(cmd_id)
        };
        if let Some(tx) = tx {
            let _ = tx.send(result);
        }
    }

    /// 关闭全部连接（进程退出时）。
    pub async fn close_all(&self) {
        let all: Vec<Arc<LocalWsTransport>> = {
            let map = self.transports.lock().await;
            map.values().cloned().collect()
        };
        for t in all {
            t.close().await;
        }
    }
}

fn epoch_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
