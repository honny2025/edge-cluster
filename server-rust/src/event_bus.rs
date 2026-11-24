//! 进程内事件总线：业务服务 → Dashboard 实时推送。
//!
//! 用 tokio broadcast 实现广播。容量 200，慢客户端自动丢最旧（与 Python 版一致）。
//! 消息信封与 Python 版对齐：{"type":"event","event":...,"data":...}。

use serde_json::Value;
use tokio::sync::broadcast;

pub const QUEUE_MAX: usize = 200;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Value>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(QUEUE_MAX);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    /// 发布事件（`send` 本身是 `&self`，可从任意线程调用）。
    pub fn publish(&self, event: &str, data: Value) {
        let msg = serde_json::json!({"type": "event", "event": event, "data": data});
        let _ = self.tx.send(msg);
    }

    /// 从非 async 线程（如 UDP 监听线程）安全投递。
    pub fn publish_threadsafe(&self, event: &str, data: Value) {
        self.publish(event, data);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
