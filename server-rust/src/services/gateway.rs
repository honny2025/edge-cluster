//! §7 最简 API 网关：转发 + 计时 + 计数。计数器存内存，进程重启即清零。
//! 与 Python services/gateway.py 对齐。

use std::collections::HashMap;
use std::sync::Mutex;

struct GatewayStat {
    requests: i64,
    errors: i64,
    total_ms: f64,
    last_ms: f64,
    last_status: Option<i64>,
}

impl GatewayStat {
    fn new() -> Self {
        Self { requests: 0, errors: 0, total_ms: 0.0, last_ms: 0.0, last_status: None }
    }

    fn avg_ms(&self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            (self.total_ms / self.requests as f64 * 100.0).round() / 100.0
        }
    }
}

pub struct GatewayStats {
    data: Mutex<HashMap<String, GatewayStat>>,
}

impl GatewayStats {
    pub fn new() -> Self {
        Self { data: Mutex::new(HashMap::new()) }
    }

    pub fn record(&self, service_id: &str, ok: bool, ms: f64, status: Option<i64>) {
        let mut data = self.data.lock().unwrap();
        let stat = data.entry(service_id.to_string()).or_insert_with(GatewayStat::new);
        stat.requests += 1;
        if !ok {
            stat.errors += 1;
        }
        stat.total_ms += ms;
        stat.last_ms = ms;
        stat.last_status = status;
    }

    pub fn get(&self, service_id: &str) -> serde_json::Value {
        let data = self.data.lock().unwrap();
        match data.get(service_id) {
            None => serde_json::json!({
                "service_id": service_id,
                "requests": 0,
                "errors": 0,
                "last_ms": 0.0,
                "avg_ms": 0.0,
                "last_status": null,
            }),
            Some(s) => serde_json::json!({
                "service_id": service_id,
                "requests": s.requests,
                "errors": s.errors,
                "last_ms": (s.last_ms * 100.0).round() / 100.0,
                "avg_ms": s.avg_ms(),
                "last_status": s.last_status,
            }),
        }
    }
}

impl Default for GatewayStats {
    fn default() -> Self {
        Self::new()
    }
}
