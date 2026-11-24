//! 告警判定（里程碑 2）。规则全部来自配置 [alerts]，代码不含硬编码阈值。
//!
//! 三条硬纪律（与 Python services/alerting.py 对齐）：
//! 1. 没有数据 ≠ 异常：指标为 None 一律不产生告警。
//! 2. never_seen 不报离线告警：只有曾经在线过、现在心跳超时的才告警。
//! 3. 离线节点的旧指标不参与指标类告警。

use std::collections::HashMap;

use serde_json::Value;

use crate::config;
use crate::enums::node_status;

use super::metrics_query::{percent, MetricRow};

const LEVEL_WARNING: &str = "warning";
const LEVEL_CRITICAL: &str = "critical";

const KIND_CPU: &str = "cpu";
const KIND_MEMORY: &str = "memory";
const KIND_STORAGE: &str = "storage";
const KIND_TEMPERATURE: &str = "temperature";
const KIND_NODE_OFFLINE: &str = "node_offline";

struct Alert {
    level: &'static str,
    kind: &'static str,
    node_id: String,
    node_name: String,
    message: String,
    value: Option<f64>,
    threshold: Option<f64>,
    unit: &'static str,
}

impl Alert {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "level": self.level,
            "kind": self.kind,
            "node_id": self.node_id,
            "node_name": self.node_name,
            "message": self.message,
            "value": self.value,
            "threshold": self.threshold,
            "unit": self.unit,
        })
    }
}

/// 根据节点列表 + 最新指标算出当前活跃告警。无状态。
pub fn evaluate(nodes: &[Value], latest: &HashMap<String, MetricRow>) -> Vec<Value> {
    let cfg = &config::get().alerts;
    if !cfg.enabled {
        return Vec::new();
    }

    let mut alerts: Vec<Alert> = Vec::new();
    for node in nodes {
        let node_id = node.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        let name = node
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(node_id)
            .to_string();
        let status = node.get("status").and_then(|v| v.as_str()).unwrap_or("");

        if status == node_status::OFFLINE && cfg.alarm_on_offline {
            let hb = node
                .get("last_heartbeat")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            alerts.push(Alert {
                level: LEVEL_CRITICAL,
                kind: KIND_NODE_OFFLINE,
                node_id: node_id.to_string(),
                node_name: name,
                message: if hb.is_empty() {
                    "节点离线（最后心跳 未知）".to_string()
                } else {
                    format!("节点离线（最后心跳 {}）", hb)
                },
                value: None,
                threshold: None,
                unit: "",
            });
            continue;
        }

        if status != node_status::ONLINE {
            continue;
        }

        let sample = match latest.get(node_id) {
            Some(s) => s,
            None => continue,
        };

        check_cpu(&mut alerts, node_id, &name, sample, cfg);
        check_memory(&mut alerts, node_id, &name, sample, cfg);
        check_storage(&mut alerts, node_id, &name, sample, cfg);
        check_temperature(&mut alerts, node_id, &name, sample, cfg);
    }

    // critical 在前，同级按节点名、告警类型排序，保证前端顺序稳定不跳动。
    alerts.sort_by(|a, b| {
        let a_crit = a.level == LEVEL_CRITICAL;
        let b_crit = b.level == LEVEL_CRITICAL;
        a_crit
            .cmp(&b_crit)
            .then_with(|| a.node_name.cmp(&b.node_name))
            .then_with(|| a.kind.cmp(b.kind))
    });
    alerts.into_iter().map(|a| a.to_json()).collect()
}

fn check_cpu(alerts: &mut Vec<Alert>, node_id: &str, name: &str, sample: &MetricRow, cfg: &config::AlertConfig) {
    let v = match sample.cpu_usage {
        Some(v) => v,
        None => return,
    };
    if v < cfg.cpu_usage_warn {
        return;
    }
    alerts.push(Alert {
        level: LEVEL_WARNING,
        kind: KIND_CPU,
        node_id: node_id.to_string(),
        node_name: name.to_string(),
        message: format!(
            "CPU 使用率偏高 {:.1}%（阈值 {:.0}%）",
            v, cfg.cpu_usage_warn
        ),
        value: Some(round2(v)),
        threshold: Some(cfg.cpu_usage_warn),
        unit: "%",
    });
}

fn check_memory(alerts: &mut Vec<Alert>, node_id: &str, name: &str, sample: &MetricRow, cfg: &config::AlertConfig) {
    let p = match percent(sample.mem_used.map(|v| v as f64), sample.mem_total.map(|v| v as f64)) {
        Some(p) => p,
        None => return,
    };
    if p < cfg.mem_usage_warn {
        return;
    }
    alerts.push(Alert {
        level: LEVEL_WARNING,
        kind: KIND_MEMORY,
        node_id: node_id.to_string(),
        node_name: name.to_string(),
        message: format!(
            "内存使用率 {:.1}%（阈值 {:.0}%）",
            p, cfg.mem_usage_warn
        ),
        value: Some(p),
        threshold: Some(cfg.mem_usage_warn),
        unit: "%",
    });
}

fn check_storage(alerts: &mut Vec<Alert>, node_id: &str, name: &str, sample: &MetricRow, cfg: &config::AlertConfig) {
    let p = match percent(sample.storage_used.map(|v| v as f64), sample.storage_total.map(|v| v as f64)) {
        Some(p) => p,
        None => return,
    };
    if p < cfg.storage_usage_warn {
        return;
    }
    alerts.push(Alert {
        level: LEVEL_WARNING,
        kind: KIND_STORAGE,
        node_id: node_id.to_string(),
        node_name: name.to_string(),
        message: format!(
            "存储使用率 {:.1}%（阈值 {:.0}%）",
            p, cfg.storage_usage_warn
        ),
        value: Some(p),
        threshold: Some(cfg.storage_usage_warn),
        unit: "%",
    });
}

fn check_temperature(alerts: &mut Vec<Alert>, node_id: &str, name: &str, sample: &MetricRow, cfg: &config::AlertConfig) {
    let v = match sample.temperature {
        Some(v) => v,
        None => return,
    };
    if v >= cfg.temperature_critical {
        alerts.push(Alert {
            level: LEVEL_CRITICAL,
            kind: KIND_TEMPERATURE,
            node_id: node_id.to_string(),
            node_name: name.to_string(),
            message: format!(
                "温度过高 {:.1}℃（严重阈值 {:.0}℃）",
                v, cfg.temperature_critical
            ),
            value: Some(round1(v)),
            threshold: Some(cfg.temperature_critical),
            unit: "℃",
        });
    } else if v >= cfg.temperature_warn {
        alerts.push(Alert {
            level: LEVEL_WARNING,
            kind: KIND_TEMPERATURE,
            node_id: node_id.to_string(),
            node_name: name.to_string(),
            message: format!(
                "温度偏高 {:.1}℃（阈值 {:.0}℃）",
                v, cfg.temperature_warn
            ),
            value: Some(round1(v)),
            threshold: Some(cfg.temperature_warn),
            unit: "℃",
        });
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
