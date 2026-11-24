//! 状态枚举与状态机常量 —— 三端共用契约，与 Python 版 models/enums.py 逐值对齐。
//!
//! 用字符串常量而不是 Rust enum 的原因：这些值大量以字符串形态进出 SQLite 与
//! JSON（wire 协议），用常量 + 判断函数最贴合原实现，避免每次边界转换。
#![allow(dead_code)] // 三端契约常量：部分值仅 Python 侧引用，此处保留对齐。

/// 节点在线状态（三态）。
pub mod node_status {
    pub const NEVER_SEEN: &str = "never_seen";
    pub const ONLINE: &str = "online";
    pub const OFFLINE: &str = "offline";
}

/// 模型服务 / Runtime 状态（七态）。
pub mod service_status {
    pub const STOPPED: &str = "stopped";
    pub const STARTING: &str = "starting";
    pub const RUNNING: &str = "running";
    pub const RESTARTING: &str = "restarting";
    pub const STOPPING: &str = "stopping";
    pub const CRASHED: &str = "crashed";
    pub const ERROR: &str = "error";
}

/// 单节点部署进度状态（九态）。
pub mod deploy_node_status {
    pub const PENDING: &str = "pending";
    pub const UPLOADING: &str = "uploading";
    pub const DOWNLOADING: &str = "downloading";
    pub const VERIFYING: &str = "verifying";
    pub const INSTALLING: &str = "installing";
    pub const TESTING: &str = "testing";
    pub const READY: &str = "ready";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
}

/// 部署任务聚合状态（六态）。
pub mod deploy_task_status {
    pub const PENDING: &str = "pending";
    pub const RUNNING: &str = "running";
    pub const PARTIAL: &str = "partial";
    pub const SUCCEEDED: &str = "succeeded";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
}

/// 模型服务对外 API 类型。
pub mod api_type {
    pub const LLM: &str = "llm";
    pub const VLM: &str = "vlm";
    pub const VISION: &str = "vision";
    pub const OCR: &str = "ocr";
    pub const OTHER: &str = "other";
}

/// 模型框架。
pub mod framework {
    pub const LLAMA_CPP: &str = "llama.cpp";
    pub const ONNX: &str = "onnx";
    pub const MNN: &str = "mnn";
    pub const QNN: &str = "qnn";
    pub const OTHER: &str = "other";
}

/// 部署终态（节点级）。
pub fn deploy_terminal_node_states() -> &'static [&'static str] {
    &[
        deploy_node_status::READY,
        deploy_node_status::FAILED,
        deploy_node_status::CANCELLED,
    ]
}

/// 部署进行中（节点级）。
pub fn deploy_active_node_states() -> &'static [&'static str] {
    &[
        deploy_node_status::PENDING,
        deploy_node_status::UPLOADING,
        deploy_node_status::DOWNLOADING,
        deploy_node_status::VERIFYING,
        deploy_node_status::INSTALLING,
        deploy_node_status::TESTING,
    ]
}

/// 服务处于"应该有一个进程在跑"或"正在变动"的状态。
pub fn service_active_states() -> &'static [&'static str] {
    &[
        service_status::STARTING,
        service_status::RUNNING,
        service_status::RESTARTING,
    ]
}

/// 服务占用资源的互斥判定状态（准入检查用）。
pub fn service_occupying_states() -> &'static [&'static str] {
    &[
        service_status::STARTING,
        service_status::RUNNING,
        service_status::RESTARTING,
    ]
}

pub fn is_terminal_node_state(s: &str) -> bool {
    deploy_terminal_node_states().contains(&s)
}

pub fn is_active_node_state(s: &str) -> bool {
    deploy_active_node_states().contains(&s)
}

/// 根据逐节点状态聚合出任务状态（与 Python `aggregate_deploy_status` 完全一致）。
pub fn aggregate_deploy_status(node_statuses: &[String]) -> String {
    use deploy_node_status as d;
    use deploy_task_status as t;
    use std::collections::HashSet;

    if node_statuses.is_empty() {
        return t::PENDING.to_string();
    }

    let states: HashSet<&str> = node_statuses.iter().map(|s| s.as_str()).collect();
    let done = states.iter().all(|s| is_terminal_node_state(s));
    if !done {
        return if states.len() == 1 && states.contains(d::PENDING) {
            t::PENDING.to_string()
        } else {
            t::RUNNING.to_string()
        };
    }

    let ready = states.contains(d::READY);
    let failed = states.contains(d::FAILED);
    let cancelled = states.contains(d::CANCELLED);
    // `others = states - {READY}` —— 等价于非 READY 终态的数量。
    let others = states.len() - if ready { 1 } else { 0 };

    if states.len() == 1 && cancelled {
        return t::CANCELLED.to_string();
    }
    if ready && (failed || cancelled) {
        return t::PARTIAL.to_string();
    }
    if ready {
        return t::SUCCEEDED.to_string();
    }
    if failed && others == 0 {
        return t::FAILED.to_string();
    }
    if failed {
        return t::FAILED.to_string();
    }
    t::CANCELLED.to_string()
}
