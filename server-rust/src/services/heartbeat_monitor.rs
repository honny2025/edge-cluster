//! 心跳巡检：判定节点离线（状态机裁决者）。与 Python services/heartbeat_monitor.py 对齐。
//!
//! 判定：missed_heartbeats >= offline_confirm_times 才落库为 offline。
//! 额外职责：在线节点 uptime 刷新；按周期触发指标分层维护。

use std::time::Duration;

use chrono::NaiveDateTime;
use serde_json::json;

use crate::config;
use crate::db::DbPool;
use crate::enums::{
    aggregate_deploy_status, deploy_active_node_states, deploy_node_status,
    is_terminal_node_state, node_status,
};
use crate::event_bus::EventBus;
use crate::time;

/// 启动心跳巡检任务（每 heartbeat_interval 秒一次）。
pub fn spawn_heartbeat_monitor(pool: DbPool, bus: EventBus) {
    tokio::spawn(async move {
        let interval = config::get().server.heartbeat_interval.max(1) as u64;
        let mut last_rollup_at: Option<NaiveDateTime> = None;
        loop {
            check_once(&pool, &bus);
            maybe_cleanup(&pool, &mut last_rollup_at);
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
}

fn check_once(pool: &DbPool, bus: &EventBus) {
    let threshold = config::get().server.offline_threshold;
    let confirm = config::get().server.offline_confirm_times.max(1);
    let now = time::now();
    let cutoff = now - chrono::Duration::seconds(threshold);
    let cutoff_db = time::format_db(&cutoff);

    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    // 读取在线节点
    let mut nodes: Vec<(String, String, Option<String>, i64, Option<String>)> = Vec::new();
    {
        let mut stmt = match conn.prepare(
            "SELECT node_id, name, last_heartbeat, missed_heartbeats, online_since \
             FROM nodes WHERE status = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let iter = match stmt.query_map([node_status::ONLINE], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        }) {
            Ok(it) => it,
            Err(_) => return,
        };
        for row in iter.flatten() {
            nodes.push(row);
        }
    }

    let mut changes: Vec<serde_json::Value> = Vec::new();

    for (node_id, name, hb, missed, online_since) in nodes {
        let fresh = hb.as_deref().map(|h| h >= cutoff_db.as_str()).unwrap_or(false);
        if hb.is_none() {
            let new_missed = missed + 1;
            let _ = conn.execute(
                "UPDATE nodes SET missed_heartbeats = ?2 WHERE node_id = ?1",
                rusqlite::params![node_id, new_missed],
            );
            if new_missed >= confirm {
                mark_offline(&conn, &node_id, &name, new_missed, None, &mut changes);
            }
        } else if fresh {
            // 心跳新鲜：清空防抖计数，刷新在线时长
            let mut uptime: i64 = 0;
            if let Some(since) = &online_since {
                if let Some(elapsed) = time::db_elapsed_secs(&time::now_db(), since) {
                    uptime = if elapsed > 0.0 { elapsed as i64 } else { 0 };
                }
            }
            let _ = conn.execute(
                "UPDATE nodes SET missed_heartbeats = 0, uptime_seconds = ?2 WHERE node_id = ?1",
                rusqlite::params![node_id, uptime],
            );
        } else {
            let new_missed = missed + 1;
            let _ = conn.execute(
                "UPDATE nodes SET missed_heartbeats = ?2 WHERE node_id = ?1",
                rusqlite::params![node_id, new_missed],
            );
            if new_missed >= confirm {
                let hb_str = hb.clone();
                mark_offline(&conn, &node_id, &name, new_missed, hb_str.as_deref(), &mut changes);
            }
        }
    }

    for change in changes {
        bus.publish_threadsafe("node_offline", change);
    }
}

fn mark_offline(
    conn: &rusqlite::Connection,
    node_id: &str,
    name: &str,
    missed: i64,
    hb: Option<&str>,
    changes: &mut Vec<serde_json::Value>,
) {
    let _ = conn.execute(
        "UPDATE nodes SET status = ?2, uptime_seconds = 0 WHERE node_id = ?1",
        rusqlite::params![node_id, node_status::OFFLINE],
    );
    // 节点下线时，把它正在进行的部署判为失败 —— 否则手机下载途中一断线，
    // 部署任务就永远卡在「下载中」，用户只能看到一片挂起。
    fail_inflight_deployments(conn, node_id);
    changes.push(json!({
        "node_id": node_id,
        "name": name,
        "status": node_status::OFFLINE,
        "last_heartbeat": hb.map(time::db_to_iso),
        "missed_heartbeats": missed,
    }));
}

/// 节点下线后，把它的非终态部署全部判为失败，并重新结算受影响的部署任务。
fn fail_inflight_deployments(conn: &rusqlite::Connection, node_id: &str) {
    let mut task_ids: Vec<String> = Vec::new();
    for &st in deploy_active_node_states() {
        let ids: Vec<String> = {
            let mut stmt = match conn.prepare(
                "SELECT task_id FROM deploy_task_nodes WHERE node_id = ?1 AND status = ?2",
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut out = Vec::new();
            if let Ok(it) = stmt.query_map(rusqlite::params![node_id, st], |r| r.get::<_, String>(0))
            {
                out = it.flatten().collect();
            }
            out
        };
        if ids.is_empty() {
            continue;
        }
        let _ = conn.execute(
            "UPDATE deploy_task_nodes SET status = ?3, error = ?4, finished_at = ?5 \
             WHERE node_id = ?1 AND status = ?2",
            rusqlite::params![
                node_id,
                st,
                deploy_node_status::FAILED,
                "节点离线，部署中断（请重试）",
                time::now_db()
            ],
        );
        task_ids.extend(ids);
    }
    for task_id in task_ids {
        settle_deploy_task(conn, &task_id);
    }
}

/// 重新结算单个部署任务：所有节点都到终态后，聚合出任务级状态。
fn settle_deploy_task(conn: &rusqlite::Connection, task_id: &str) {
    let mut statuses: Vec<String> = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT status FROM deploy_task_nodes WHERE task_id = ?1") {
        if let Ok(it) = stmt.query_map([task_id], |r| r.get::<_, String>(0)) {
            statuses = it.flatten().collect();
        }
    }
    if statuses.is_empty() {
        return;
    }
    if statuses.iter().all(|s| is_terminal_node_state(s)) {
        let agg = aggregate_deploy_status(&statuses);
        let _ = conn.execute(
            "UPDATE deploy_tasks SET status = ?2, finished_at = COALESCE(finished_at, ?3) WHERE task_id = ?1",
            rusqlite::params![task_id, agg, time::now_db()],
        );
    }
}

fn maybe_cleanup(pool: &DbPool, last_rollup_at: &mut Option<NaiveDateTime>) {
    let interval = config::get().server.metric_rollup_interval_minutes.max(1);
    let now = time::now();
    if let Some(last) = last_rollup_at {
        if (now - *last).num_seconds() < interval * 60 {
            return;
        }
    }
    let stats = super::metric_rollup::run_once(pool, None);
    if stats["ok"].as_bool() == Some(true) {
        *last_rollup_at = Some(now);
    }
}
