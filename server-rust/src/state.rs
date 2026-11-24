//! 全局共享状态（AppState）：配置 + 连接池 + 各服务单例。
//!
//! 用 `Arc` 包裹，axum 各 handler 通过 `State<Arc<AppState>>` 共享。

use std::sync::Arc;

use crate::config::Config;
use crate::connection::ConnectionManager;
use crate::db::DbPool;
use crate::event_bus::EventBus;
use crate::services::deploy_queue::DeployQueue;
use crate::services::file_transfers::TransferService;
use crate::services::gateway::GatewayStats;
use crate::services::model_tokens::DownloadTokens;
use crate::services::node_registry::NodeRegistry;
use crate::services::service_watchdog::ServiceWatchdog;

pub struct AppState {
    pub config: Config,
    pub pool: DbPool,
    pub started_at: String,
    pub bus: EventBus,
    pub manager: Arc<ConnectionManager>,
    pub registry: Arc<NodeRegistry>,
    pub deploy_queue: Arc<DeployQueue>,
    #[allow(dead_code)] // 与 Python 版状态结构对齐
    pub watchdog: Arc<ServiceWatchdog>,
    pub download_tokens: Arc<DownloadTokens>,
    pub transfers: Arc<TransferService>,
    pub gateway: Arc<GatewayStats>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        pool: DbPool,
        bus: EventBus,
        manager: Arc<ConnectionManager>,
        registry: Arc<NodeRegistry>,
        deploy_queue: Arc<DeployQueue>,
        watchdog: Arc<ServiceWatchdog>,
        download_tokens: Arc<DownloadTokens>,
        transfers: Arc<TransferService>,
        gateway: Arc<GatewayStats>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            pool,
            started_at: crate::time::now_db(),
            bus,
            manager,
            registry,
            deploy_queue,
            watchdog,
            download_tokens,
            transfers,
            gateway,
        })
    }
}
