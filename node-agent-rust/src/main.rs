//! edge-cluster 节点 Agent（Rust 重写版）入口。
//!
//! 用法：
//!   edge-cluster-agent [--config PATH] [--simulate] [--name NAME] [--node-id ID]
//!                      [--platform-url URL] [--token TOKEN] [--heartbeat-interval N]
//!                      [--log-level LEVEL] [--log-tag TAG]
//!
//! 优先级：命令行 > 环境变量 EDGE_SECTION__KEY > config.toml > 默认值。

mod agent;
mod config;
mod discovery;
mod files;
mod metrics;
mod rollback;
mod util;

use std::env;
use std::sync::Arc;

use agent::EdgeAgent;
use config::{load_config, AGENT_VERSION};

struct CliArgs {
    config: Option<String>,
    simulate: bool,
    name: Option<String>,
    node_id: Option<String>,
    platform_url: Option<String>,
    token: Option<String>,
    heartbeat_interval: Option<i64>,
    log_level: String,
    log_tag: Option<String>,
    help: bool,
}

fn parse_args() -> CliArgs {
    let mut a = CliArgs {
        config: None,
        simulate: false,
        name: None,
        node_id: None,
        platform_url: None,
        token: None,
        heartbeat_interval: None,
        log_level: "INFO".to_string(),
        log_tag: None,
        help: false,
    };
    let mut it = env::args().skip(1);
    let next_val = |key: &str, cur: Option<String>| -> Option<String> {
        cur.or_else(|| {
            eprintln!("[cli] 参数 {key} 缺少取值");
            None
        })
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => a.help = true,
            "--simulate" => a.simulate = true,
            "--config" => a.config = next_val("--config", it.next()),
            "--name" => a.name = next_val("--name", it.next()),
            "--node-id" => a.node_id = next_val("--node-id", it.next()),
            "--platform-url" => a.platform_url = next_val("--platform-url", it.next()),
            "--token" => a.token = next_val("--token", it.next()),
            "--heartbeat-interval" => {
                a.heartbeat_interval = it.next().and_then(|v| v.parse::<i64>().ok());
            }
            "--log-level" => a.log_level = it.next().unwrap_or_else(|| "INFO".to_string()),
            "--log-tag" => a.log_tag = it.next(),
            other => {
                eprintln!("[cli] 忽略未知参数: {other}");
            }
        }
    }
    a
}

fn print_help() {
    println!(
        "edge-cluster Agent v{AGENT_VERSION}\n\n\
         用法: edge-cluster-agent [选项]\n\n\
         --config PATH          配置文件路径（默认 ./config.toml）\n\
         --simulate             启用模拟模式（PC 联调用，不读真机指标）\n\
         --name NAME            显示名（会据此解析/固化 node_id）\n\
         --node-id ID           强制指定 node_id\n\
         --platform-url URL     手工指定平台地址（如 http://192.168.1.100:8000）\n\
         --token TOKEN          覆盖握手 token\n\
         --heartbeat-interval N 心跳间隔（秒）\n\
         --log-level LEVEL      DEBUG/INFO/WARN/ERROR（默认 INFO）\n\
         --log-tag TAG          日志文件名前缀（默认 node_id）\n\
         -h, --help             显示帮助"
    );
}

/// 解析 node_id：CLI --node-id > 实例状态（按 --name）> config.toml > 自动生成。
fn resolve_node_id(cfg: &config::Config, args: &CliArgs) -> String {
    if let Some(id) = &args.node_id {
        if !id.trim().is_empty() {
            return id.clone();
        }
    }
    if let Some(name) = &args.name {
        if !name.trim().is_empty() {
            let state = config::load_instance_state(name);
            if let Some(id) = state.get("node_id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    log::info!("按名称 {name} 找到已登记的 node_id={id}");
                    return id.to_string();
                }
            }
        }
    }
    if !cfg.agent.node_id.trim().is_empty() {
        return cfg.agent.node_id.clone();
    }
    let gen_name = args.name.clone().unwrap_or_else(|| cfg.agent.name.clone());
    config::default_node_id(&gen_name)
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    if args.help {
        print_help();
        return;
    }

    let mut cfg = load_config(args.config.as_deref());

    // 命令行覆盖
    if args.simulate {
        cfg.simulate.enabled = true;
    }
    if let Some(n) = &args.name {
        cfg.agent.name = n.clone();
    }
    if let Some(u) = &args.platform_url {
        cfg.agent.platform_url = u.clone();
    }
    if let Some(t) = &args.token {
        cfg.agent.token = t.clone();
    }
    if let Some(h) = args.heartbeat_interval {
        if h > 0 {
            cfg.agent.heartbeat_interval = h;
        }
    }

    let node_id = resolve_node_id(&cfg, &args);
    cfg.agent.node_id = node_id.clone();
    let name = if cfg.agent.name.trim().is_empty() {
        node_id.clone()
    } else {
        cfg.agent.name.clone()
    };

    // 固化身份：实例状态 + 回写 config.toml（仅当原本为空且未指定 --name）
    config::save_instance_state(&name, &node_id, &name);
    if cfg.agent.node_id.is_empty() || args.name.is_some() {
        config::update_config_node_id(&cfg, &node_id, &name);
    }

    // 日志
    let log_dir = cfg.logs_path();
    let log_tag = args.log_tag.clone().unwrap_or_else(|| node_id.clone());
    let _logfile = util::setup_logging(
        &log_dir,
        &args.log_level,
        cfg.storage.log_max_bytes,
        cfg.storage.log_backup_count,
        &log_tag,
    );

    log::info!(
        "edge-cluster Agent v{AGENT_VERSION} 启动 node_id={} name={} simulate={}",
        node_id,
        name,
        cfg.simulate.enabled
    );
    if cfg.simulate.enabled {
        log::warn!("模拟模式已开启（metrics 使用假数据，不触碰真机硬件）");
    }

    let agent = Arc::new(EdgeAgent::new(cfg));
    let run_agent = agent.clone();
    let run_task = tokio::spawn(async move { run_agent.run().await });

    let sig = wait_for_signal().await;
    log::warn!("收到 {sig}，正在停机…");

    agent.request_stop().await;
    let _ = run_task.await;
    let stopped = agent.stop_all_services().await;
    agent.persist_services();
    log::info!("已停止 {} 个托管服务，退出", stopped);
}

/// 等待停止信号，返回信号名。
#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
        _ = tokio::signal::ctrl_c() => "Ctrl-C",
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "Ctrl-C"
}
