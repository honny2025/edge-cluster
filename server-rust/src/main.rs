//! edge-cluster 平台服务端入口（Rust 重写版）。
//!
//! 与 Python app/main.py 的生命周期逐项对齐：
//! 载入配置 → 建表 → 装配各服务单例 → 安全自检 → 归零残留在线态 →
//! 启动 UDP 发现 / 心跳巡检 / 部署队列 / 服务看门狗 → 挂载路由 → 监听 8000。

mod api;
mod audit;
mod config;
mod connection;
mod db;
mod enums;
mod error;
mod event_bus;
mod security;
mod services;
mod state;
mod time;
mod util;

use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 配置必须在任何后台任务之前就绪：discovery / heartbeat / watchdog 都会读 config::get()。
    let config = config::load_config(None);
    config.ensure_dirs()?;
    config::init_global(config.clone());

    let pool = db::open_pool(&config.db_file())?;
    {
        let conn = pool.get()?;
        db::init_db(&conn)?;
    }

    // ---- 服务单例（顺序对应 Python main.py 的 import）----
    let bus = event_bus::EventBus::new();
    let manager = connection::ConnectionManager::new();
    let registry = Arc::new(services::node_registry::NodeRegistry::new(
        pool.clone(),
        bus.clone(),
    ));
    let download_tokens = Arc::new(services::model_tokens::DownloadTokens::new());
    let deploy_queue = services::deploy_queue::DeployQueue::new(
        manager.clone(),
        pool.clone(),
        download_tokens.clone(),
    );
    let watchdog = services::service_watchdog::ServiceWatchdog::new(manager.clone(), pool.clone());
    let transfers = Arc::new(services::file_transfers::TransferService::new(pool.clone()));
    let gateway = Arc::new(services::gateway::GatewayStats::new());

    let state = state::AppState::new(
        config.clone(),
        pool.clone(),
        bus.clone(),
        manager.clone(),
        registry.clone(),
        deploy_queue.clone(),
        watchdog.clone(),
        download_tokens,
        transfers,
        gateway,
    );

    // 安全自检：仍是默认 token 就大声警告（放在 init_db 之后，不被数据库问题连带吞掉）。
    security::warn_if_default_token();

    // 平台刚启动不可能持有 Agent 连接：把库里残留的 online 归零，避免"假在线"。
    let _ = registry.reset_online_on_boot();

    // ---- 启动后台任务 ----
    services::discovery::spawn_discovery(bus.clone());
    services::heartbeat_monitor::spawn_heartbeat_monitor(pool.clone(), bus.clone());
    deploy_queue.start(); // 部署下发队列（并发上限 2）
    watchdog.start(); // 服务看门狗：周期探活 + 崩溃收敛 + 重连恢复

    let app = api::build_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "edge-cluster-server v0.5.0-m5 listening on http://{addr} (db={}, discovery={})",
        config.db_file().display(),
        config.server.discovery_port
    );

    // 优雅退出：收到 Ctrl+C / 终止信号时先关闭全部 Agent 连接（与 Python 版
    // lifespan 的 `manager.close_all()` 对齐），再由 axum 正常收尾。
    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        println!("收到退出信号，正在关闭所有 Agent 连接...");
        manager.close_all().await;
    };

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}
