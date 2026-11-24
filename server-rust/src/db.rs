//! SQLite 连接池 + 建表 + 迁移。
//!
//! 表结构与 Python 版 app/database.py + 9 个 ORM 模型逐列对齐（硬约束）。
//! PRAGMA：WAL / synchronous=NORMAL / foreign_keys=ON / busy_timeout=15000。

use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

pub type DbPool = r2d2::Pool<SqliteConnectionManager>;
pub type PooledConn = r2d2::PooledConnection<SqliteConnectionManager>;

/// 打开连接池。每条新连接都会应用 PRAGMA。
pub fn open_pool(db_file: &Path) -> Result<DbPool, r2d2::Error> {
    let manager = SqliteConnectionManager::file(db_file)
        .with_flags(
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_init(|conn: &mut Connection| {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.pragma_update(None, "busy_timeout", "15000")?;
            Ok(())
        });
    Pool::builder().max_size(10).build(manager)
}

/// 建表（幂等）。含全部索引与唯一约束。
pub fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)
}

/// 老库平滑迁移：给已存在的表补齐后来新增的列（只补真正缺失的）。
/// 与 Python `_MIGRATIONS` 一致：指标类新列一律 DEFAULT NULL（"没采到"，不是 0）。
pub fn migrate_existing_tables(conn: &Connection) -> rusqlite::Result<()> {
    // (table, column, sql_type, default)
    const MIGRATIONS: &[(&str, &[(&str, &str, &str)])] = &[
        (
            "services",
            &[
                ("mem_required_mb", "INTEGER", "0"),
                ("mutex_group", "VARCHAR(64)", "''"),
                ("model_path", "VARCHAR(512)", "''"),
                ("auto_start", "INTEGER", "0"),
                ("restart_count", "INTEGER", "0"),
                ("last_exit_code", "INTEGER", "NULL"),
                ("state_changed_at", "DATETIME", "NULL"),
            ],
        ),
        (
            "nodes",
            &[
                ("cpu", "VARCHAR(160)", "''"),
                ("gpu", "VARCHAR(160)", "''"),
                ("npu", "VARCHAR(160)", "''"),
                ("qnn_version", "VARCHAR(64)", "''"),
                ("is_root", "INTEGER", "-1"),
                ("capabilities", "JSON", "NULL"),
                ("device_token", "VARCHAR(64)", "''"),
                ("device_token_issued_at", "DATETIME", "NULL"),
            ],
        ),
        ("deploy_task_nodes", &[("rollback_note", "TEXT", "''")]),
        ("file_transfers", &[("bytes_done", "BIGINT", "0")]),
        (
            "metric_samples",
            &[
                ("cpu_temp", "FLOAT", "NULL"),
                ("gpu_temp", "FLOAT", "NULL"),
                ("cpu_freq_mhz", "FLOAT", "NULL"),
                ("gpu_freq_mhz", "FLOAT", "NULL"),
                ("battery_percent", "FLOAT", "NULL"),
                ("battery_temp", "FLOAT", "NULL"),
                ("battery_status", "VARCHAR(24)", "NULL"),
            ],
        ),
        (
            "models",
            &[
                ("quantization", "VARCHAR(32)", "''"),
                ("target_soc", "VARCHAR(128)", "''"),
                ("qnn_version", "VARCHAR(64)", "''"),
                ("backends", "JSON", "NULL"),
                ("deps", "JSON", "NULL"),
                ("run_params", "JSON", "NULL"),
                ("manifest", "JSON", "NULL"),
                ("self_test_command", "VARCHAR(512)", "''"),
                ("self_test_timeout", "INTEGER", "60"),
            ],
        ),
    ];

    for (table, columns) in MIGRATIONS {
        if !table_exists(conn, table)? {
            continue;
        }
        let existing = table_columns(conn, table)?;
        for (col, col_type, default) in *columns {
            if existing.iter().any(|c| c == col) {
                continue;
            }
            let sql = format!(
                "ALTER TABLE {table} ADD COLUMN {col} {col_type} DEFAULT {default}"
            );
            conn.execute(&sql, [])?;
        }
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
    )?;
    let exists = stmt.exists([table])?;
    Ok(exists)
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 初始化数据库（建表 + 迁移），幂等。
pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    create_schema(conn)?;
    migrate_existing_tables(conn)?;
    Ok(())
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS nodes (
    node_id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL DEFAULT '',
    model VARCHAR(128) NOT NULL DEFAULT '',
    soc VARCHAR(128) NOT NULL DEFAULT '',
    android_version VARCHAR(64) NOT NULL DEFAULT '',
    agent_version VARCHAR(32) NOT NULL DEFAULT '',
    ip VARCHAR(64) NOT NULL DEFAULT '',
    cpu VARCHAR(160) NOT NULL DEFAULT '',
    gpu VARCHAR(160) NOT NULL DEFAULT '',
    npu VARCHAR(160) NOT NULL DEFAULT '',
    qnn_version VARCHAR(64) NOT NULL DEFAULT '',
    is_root INTEGER NOT NULL DEFAULT -1,
    capabilities JSON,
    device_token VARCHAR(64) NOT NULL DEFAULT '',
    device_token_issued_at DATETIME,
    status VARCHAR(16) NOT NULL DEFAULT 'never_seen',
    last_heartbeat DATETIME,
    online_since DATETIME,
    uptime_seconds INTEGER NOT NULL DEFAULT 0,
    missed_heartbeats INTEGER NOT NULL DEFAULT 0,
    is_simulated INTEGER NOT NULL DEFAULT 0,
    note TEXT NOT NULL DEFAULT '',
    created_at DATETIME NOT NULL,
    updated_at DATETIME NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_nodes_device_token ON nodes(device_token);
CREATE INDEX IF NOT EXISTS ix_nodes_status ON nodes(status);
CREATE INDEX IF NOT EXISTS ix_nodes_last_heartbeat ON nodes(last_heartbeat);
CREATE INDEX IF NOT EXISTS ix_nodes_status_hb ON nodes(status, last_heartbeat);

CREATE TABLE IF NOT EXISTS metric_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id VARCHAR(64) NOT NULL,
    ts DATETIME NOT NULL,
    cpu_usage FLOAT,
    gpu_usage FLOAT,
    npu_usage FLOAT,
    mem_used BIGINT,
    mem_total BIGINT,
    storage_used BIGINT,
    storage_total BIGINT,
    temperature FLOAT,
    cpu_temp FLOAT,
    gpu_temp FLOAT,
    cpu_freq_mhz FLOAT,
    gpu_freq_mhz FLOAT,
    battery_percent FLOAT,
    battery_temp FLOAT,
    battery_status VARCHAR(24),
    power_watts FLOAT,
    net_rx_bytes BIGINT,
    net_tx_bytes BIGINT,
    load_avg FLOAT,
    extra JSON,
    FOREIGN KEY(node_id) REFERENCES nodes(node_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS ix_metric_samples_node_id ON metric_samples(node_id);
CREATE INDEX IF NOT EXISTS ix_metric_samples_ts ON metric_samples(ts);
CREATE INDEX IF NOT EXISTS ix_metric_node_ts ON metric_samples(node_id, ts);

CREATE TABLE IF NOT EXISTS metric_rollups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id VARCHAR(64) NOT NULL,
    kind VARCHAR(16) NOT NULL,
    ts DATETIME NOT NULL,
    samples INTEGER NOT NULL DEFAULT 0,
    first_ts DATETIME,
    last_ts DATETIME,
    cpu_usage FLOAT,
    gpu_usage FLOAT,
    npu_usage FLOAT,
    mem_used BIGINT,
    mem_total BIGINT,
    storage_used BIGINT,
    storage_total BIGINT,
    temperature FLOAT,
    cpu_temp FLOAT,
    gpu_temp FLOAT,
    cpu_freq_mhz FLOAT,
    gpu_freq_mhz FLOAT,
    battery_percent FLOAT,
    battery_temp FLOAT,
    battery_status VARCHAR(24),
    power_watts FLOAT,
    net_rx_bytes BIGINT,
    net_tx_bytes BIGINT,
    load_avg FLOAT,
    extra JSON,
    FOREIGN KEY(node_id) REFERENCES nodes(node_id) ON DELETE CASCADE,
    CONSTRAINT uq_rollup_node_kind_ts UNIQUE(node_id, kind, ts)
);
CREATE INDEX IF NOT EXISTS ix_metric_rollups_node_id ON metric_rollups(node_id);
CREATE INDEX IF NOT EXISTS ix_metric_rollups_kind ON metric_rollups(kind);
CREATE INDEX IF NOT EXISTS ix_metric_rollups_ts ON metric_rollups(ts);
CREATE INDEX IF NOT EXISTS ix_rollup_kind_ts ON metric_rollups(kind, ts);

CREATE TABLE IF NOT EXISTS models (
    model_id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(200) NOT NULL,
    version VARCHAR(64) NOT NULL DEFAULT 'v1',
    file_name VARCHAR(255) NOT NULL DEFAULT '',
    file_size BIGINT NOT NULL DEFAULT 0,
    framework VARCHAR(32) NOT NULL DEFAULT 'other',
    format VARCHAR(32) NOT NULL DEFAULT '',
    sha256 VARCHAR(64) NOT NULL DEFAULT '',
    quantization VARCHAR(32) NOT NULL DEFAULT '',
    target_soc VARCHAR(128) NOT NULL DEFAULT '',
    qnn_version VARCHAR(64) NOT NULL DEFAULT '',
    backends JSON,
    deps JSON,
    run_params JSON,
    manifest JSON,
    self_test_command VARCHAR(512) NOT NULL DEFAULT '',
    self_test_timeout INTEGER NOT NULL DEFAULT 60,
    description TEXT NOT NULL DEFAULT '',
    uploaded_at DATETIME NOT NULL,
    storage_path VARCHAR(512) NOT NULL DEFAULT '',
    download_count INTEGER NOT NULL DEFAULT 0,
    CONSTRAINT uq_model_name_version UNIQUE(name, version)
);
CREATE INDEX IF NOT EXISTS ix_models_name ON models(name);
CREATE INDEX IF NOT EXISTS ix_models_sha256 ON models(sha256);

CREATE TABLE IF NOT EXISTS services (
    service_id VARCHAR(64) PRIMARY KEY,
    node_id VARCHAR(64) NOT NULL,
    model_id VARCHAR(64),
    name VARCHAR(128) NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL DEFAULT 'stopped',
    port INTEGER NOT NULL DEFAULT 0,
    endpoint VARCHAR(512) NOT NULL DEFAULT '',
    api_type VARCHAR(16) NOT NULL DEFAULT 'other',
    pid INTEGER,
    started_at DATETIME,
    last_error TEXT NOT NULL DEFAULT '',
    last_probe_at DATETIME,
    probe_ok INTEGER NOT NULL DEFAULT 0,
    command VARCHAR(512) NOT NULL DEFAULT '',
    args JSON,
    model_path VARCHAR(512) NOT NULL DEFAULT '',
    health_url VARCHAR(512) NOT NULL DEFAULT '',
    log_path VARCHAR(512) NOT NULL DEFAULT '',
    auto_restart INTEGER NOT NULL DEFAULT 1,
    auto_start INTEGER NOT NULL DEFAULT 0,
    restart_count INTEGER NOT NULL DEFAULT 0,
    last_exit_code INTEGER,
    state_changed_at DATETIME,
    mem_required_mb INTEGER NOT NULL DEFAULT 0,
    mutex_group VARCHAR(64) NOT NULL DEFAULT '',
    gateway_meta JSON,
    created_at DATETIME NOT NULL,
    updated_at DATETIME NOT NULL,
    FOREIGN KEY(node_id) REFERENCES nodes(node_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS ix_services_node_id ON services(node_id);
CREATE INDEX IF NOT EXISTS ix_services_model_id ON services(model_id);
CREATE INDEX IF NOT EXISTS ix_services_status ON services(status);

CREATE TABLE IF NOT EXISTS deploy_tasks (
    task_id VARCHAR(64) PRIMARY KEY,
    model_id VARCHAR(64) NOT NULL,
    model_name VARCHAR(200) NOT NULL DEFAULT '',
    target_count INTEGER NOT NULL DEFAULT 0,
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    note TEXT NOT NULL DEFAULT '',
    created_by VARCHAR(64) NOT NULL DEFAULT 'platform',
    created_at DATETIME NOT NULL,
    finished_at DATETIME
);
CREATE INDEX IF NOT EXISTS ix_deploy_tasks_model_id ON deploy_tasks(model_id);
CREATE INDEX IF NOT EXISTS ix_deploy_tasks_status ON deploy_tasks(status);
CREATE INDEX IF NOT EXISTS ix_deploy_tasks_created_at ON deploy_tasks(created_at);

CREATE TABLE IF NOT EXISTS deploy_task_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id VARCHAR(64) NOT NULL,
    node_id VARCHAR(64) NOT NULL,
    node_name VARCHAR(128) NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    progress INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    retry_count INTEGER NOT NULL DEFAULT 0,
    bytes_downloaded BIGINT NOT NULL DEFAULT 0,
    bytes_total BIGINT NOT NULL DEFAULT 0,
    rollback_note TEXT NOT NULL DEFAULT '',
    started_at DATETIME,
    finished_at DATETIME,
    updated_at DATETIME NOT NULL,
    FOREIGN KEY(task_id) REFERENCES deploy_tasks(task_id) ON DELETE CASCADE,
    CONSTRAINT uq_task_node UNIQUE(task_id, node_id)
);
CREATE INDEX IF NOT EXISTS ix_deploy_task_nodes_task_id ON deploy_task_nodes(task_id);
CREATE INDEX IF NOT EXISTS ix_deploy_task_nodes_node_id ON deploy_task_nodes(node_id);
CREATE INDEX IF NOT EXISTS ix_deploy_task_nodes_status ON deploy_task_nodes(status);
CREATE INDEX IF NOT EXISTS ix_dtn_status ON deploy_task_nodes(task_id, status);

CREATE TABLE IF NOT EXISTS audit_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts DATETIME NOT NULL,
    actor VARCHAR(96) NOT NULL DEFAULT '',
    client_ip VARCHAR(64) NOT NULL DEFAULT '',
    action VARCHAR(64) NOT NULL,
    target_type VARCHAR(32) NOT NULL DEFAULT '',
    target_id VARCHAR(96) NOT NULL DEFAULT '',
    node_id VARCHAR(64) NOT NULL DEFAULT '',
    ok INTEGER NOT NULL DEFAULT 1,
    detail JSON,
    message TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS ix_audit_logs_ts ON audit_logs(ts);
CREATE INDEX IF NOT EXISTS ix_audit_logs_actor ON audit_logs(actor);
CREATE INDEX IF NOT EXISTS ix_audit_logs_action ON audit_logs(action);
CREATE INDEX IF NOT EXISTS ix_audit_logs_target_id ON audit_logs(target_id);
CREATE INDEX IF NOT EXISTS ix_audit_logs_node_id ON audit_logs(node_id);
CREATE INDEX IF NOT EXISTS ix_audit_logs_ok ON audit_logs(ok);
CREATE INDEX IF NOT EXISTS ix_audit_ts_action ON audit_logs(ts, action);

CREATE TABLE IF NOT EXISTS file_transfers (
    transfer_id VARCHAR(32) PRIMARY KEY,
    node_id VARCHAR(64) NOT NULL,
    direction VARCHAR(8) NOT NULL,
    path VARCHAR(1024) NOT NULL DEFAULT '',
    file_name VARCHAR(255) NOT NULL DEFAULT '',
    staging_path VARCHAR(1024) NOT NULL DEFAULT '',
    size BIGINT NOT NULL DEFAULT 0,
    sha256 VARCHAR(64) NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    error TEXT NOT NULL DEFAULT '',
    token VARCHAR(64) NOT NULL DEFAULT '',
    created_at DATETIME NOT NULL,
    finished_at DATETIME,
    bytes_done BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS ix_file_transfers_node_id ON file_transfers(node_id);
CREATE INDEX IF NOT EXISTS ix_file_transfers_direction ON file_transfers(direction);
CREATE INDEX IF NOT EXISTS ix_file_transfers_status ON file_transfers(status);
CREATE INDEX IF NOT EXISTS ix_file_transfers_token ON file_transfers(token);
CREATE INDEX IF NOT EXISTS ix_file_transfers_created_at ON file_transfers(created_at);
CREATE INDEX IF NOT EXISTS ix_transfer_node_status ON file_transfers(node_id, status);
"#;
