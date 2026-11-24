//! 审计记录写入（需求 §8）。写失败不拖垮业务，但必须留下日志。

use crate::db::DbPool;
use crate::time;

/// 写一条审计记录，永不 panic。
#[allow(clippy::too_many_arguments)]
pub fn record(
    pool: &DbPool,
    action: &str,
    actor: &str,
    target_type: &str,
    target_id: &str,
    node_id: &str,
    ok: bool,
    detail: Option<&serde_json::Value>,
    message: &str,
    client_ip: &str,
) {
    let now = time::now_db();
    let detail_json: Option<String> = detail.map(|d| d.to_string());
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let conn = pool.get()?;
        conn.execute(
            "INSERT INTO audit_logs (ts, actor, client_ip, action, target_type, target_id, node_id, ok, detail, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                now,
                actor,
                client_ip,
                action,
                target_type,
                target_id,
                node_id,
                if ok { 1i64 } else { 0i64 },
                detail_json,
                message
            ],
        )?;
        Ok(())
    })();

    if let Err(e) = result {
        eprintln!(
            "审计写入失败 action={action} target={target_type}/{target_id} actor={actor} err={e}"
        );
    }
}
