//! WebSocket 端点。与 Python api/ws.py 对齐。
//!
//! - `/ws/agent?token=...`    Agent ↔ 平台（注册、心跳、指令下发、结果上报）
//! - `/ws/dashboard?token=...` 浏览器 ↔ 平台（实时事件推送，只读）

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};

use crate::api::auth::query_token;
use crate::connection::LocalWsTransport;
use crate::security;
use crate::state::AppState;
use crate::time;

type SharedState = Arc<AppState>;

const DASHBOARD_PING_INTERVAL: Duration = Duration::from_secs(25);

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/ws/agent", get(ws_agent))
        .route("/ws/dashboard", get(ws_dashboard))
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// WS 握手鉴权（与 Python require_token_ws 对齐）。
fn ws_authorized(state: &SharedState, token: Option<&str>) -> bool {
    if security::verify_token(token) {
        if state.config.security.enforce_device_token
            && security::any_device_token_exists(&state.pool)
        {
            return false;
        }
        return true;
    }
    security::verify_device_token(token, &state.pool).is_some()
}

// ====================================================================== Agent

async fn ws_agent(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    RawQuery(query): RawQuery,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let token = query_token(query.as_deref());
    let authorized = ws_authorized(&state, token.as_deref());
    let peer_ip = peer.ip().to_string();
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    let actor = security::identify(token.as_deref(), &state.pool)
        .unwrap_or_else(|| "unknown".to_string());
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_default();

    ws.on_upgrade(move |socket| agent_loop(socket, state, actor, peer_ip, host))
}

async fn agent_loop(
    socket: WebSocket,
    state: SharedState,
    actor: String,
    peer_ip: String,
    host: String,
) {
    let (sender, mut receiver) = socket.split();
    let mut sender = Some(sender);
    let mut transport: Option<Arc<LocalWsTransport>> = None;
    let mut node_id: Option<String> = None;

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(obj) = parsed.as_object() else { continue };
        let msg_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // 首帧必须是 register。
        if node_id.is_none() && msg_type != "register" {
            continue;
        }

        if msg_type == "register" {
            // info = payload + 顶层字段（除 type/payload）
            let mut info = obj
                .get("payload")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            let info_obj = info.as_object_mut().unwrap();
            for (k, v) in obj {
                if k == "type" || k == "payload" {
                    continue;
                }
                info_obj.insert(k.clone(), v.clone());
            }

            let reported_id = info
                .get("node_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if reported_id.is_empty() {
                let err = json!({"type": "error", "message": "register 缺少 node_id"});
                if let Some(s) = sender.as_mut() {
                    let _ = s.send(Message::Text(err.to_string().into())).await;
                }
                continue;
            }

            let sink = match sender.take() {
                Some(s) => s,
                None => break,
            };
            let t = Arc::new(LocalWsTransport::from_sink(
                reported_id.clone(),
                sink,
                peer_ip.clone(),
            ));
            state.manager.register(&reported_id, t.clone()).await;
            transport = Some(t);
            node_id = Some(reported_id.clone());

            if !host.is_empty() {
                state.deploy_queue.set_node_base(&reported_id, &host);
            }

            // 节点（重）连上后，把被打断的下发任务续上（手机端从 .part 断点续传）。
            state.deploy_queue.redrive_for_node(&reported_id);

            let is_simulated = info.get("simulate").and_then(|v| v.as_bool()).unwrap_or(false);
            let snapshot = state.registry.register(&info, &peer_ip, is_simulated);

            let device_token = if state.config.security.auto_issue_device_token {
                security::issue_device_token(&reported_id, &state.pool, false)
            } else {
                String::new()
            };

            crate::audit::record(
                &state.pool,
                "node.register",
                &actor,
                "node",
                &reported_id,
                &reported_id,
                true,
                Some(&json!({
                    "model": snapshot.get("model"),
                    "soc": snapshot.get("soc"),
                    "is_root": snapshot.get("is_root"),
                    "simulate": is_simulated,
                    "token_issued": !device_token.is_empty(),
                })),
                "节点注册",
                &peer_ip,
            );

            let mut ack = json!({
                "type": "register_ack",
                "node_id": reported_id,
                "ts": now_epoch(),
                "server_version": "0.5.0-m5",
                "heartbeat_interval": info.get("heartbeat_interval")
                    .and_then(|v| v.as_f64())
                    .filter(|f| *f > 0.0)
                    .unwrap_or(5.0),
                "status": snapshot.get("status").cloned().unwrap_or(Value::Null),
            });
            if !device_token.is_empty() {
                ack["device_token"] = json!(device_token);
            }
            if let Some(t) = &transport {
                let _ = t.send(&ack).await;
            }
        } else {
            let nid = node_id.clone().unwrap();
            let t = transport.clone().unwrap();
            dispatch(&state, &nid, &parsed, &peer_ip, &t).await;
        }
    }

    if let Some(nid) = node_id {
        state.manager.unregister(&nid, transport.as_ref()).await;
    }
}

async fn dispatch(
    state: &SharedState,
    node_id: &str,
    msg: &Value,
    peer_ip: &str,
    transport: &Arc<LocalWsTransport>,
) {
    let Some(obj) = msg.as_object() else { return };
    let msg_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let payload = obj
        .get("payload")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));

    match msg_type {
        "heartbeat" => {
            let snapshot = state.registry.heartbeat(node_id, &payload, peer_ip);
            if let Some(services) = payload.get("services").and_then(|v| v.as_array()) {
                reconcile_services(state, node_id, services);
            }
            let ack = json!({
                "type": "heartbeat_ack",
                "ts": now_epoch(),
                "server_status": snapshot.as_ref()
                    .and_then(|s| s.get("status").and_then(|v| v.as_str()))
                    .unwrap_or("unknown"),
            });
            let _ = transport.send(&ack).await;
        }
        "pong" => {}
        "progress" => {
            let task_id = payload.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !task_id.is_empty() {
                handle_progress(state, node_id, &task_id, &payload);
            }
        }
        "transfer_progress" => {
            let tid = payload.get("transfer_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let bd = payload.get("bytes_done").and_then(|v| v.as_i64()).unwrap_or(0);
            if !tid.is_empty() {
                // 落库，前端轮询 listTransfers 就能看到实时进度。
                state.transfers.update_progress(&tid, bd);
                state.bus.publish_threadsafe(
                    "transfer_progress",
                    json!({
                        "type": "transfer_progress",
                        "transfer_id": tid,
                        "node_id": node_id,
                        "bytes_done": bd,
                        "bytes_total": payload.get("bytes_total"),
                        "percent": payload.get("percent"),
                        "ts": now_epoch(),
                    }),
                );
            }
        }
        "ack" => {
            handle_ack(state, node_id, obj, &payload).await;
        }
        _ => {}
    }
}

fn handle_progress(state: &SharedState, node_id: &str, task_id: &str, payload: &Value) {
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let row: Option<(String, i64, i64, i64)> = conn
        .query_row(
            "SELECT status, progress, bytes_downloaded, bytes_total FROM deploy_task_nodes \
             WHERE task_id = ?1 AND node_id = ?2",
            rusqlite::params![task_id, node_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok();
    let Some((status, old_progress, _old_bd, old_bt)) = row else { return };

    let phase = payload
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let stage_phase = matches!(phase.as_str(), "verifying" | "installing" | "testing");

    let new_status = if stage_phase && enums_active(&status) {
        phase.clone()
    } else {
        status.clone()
    };

    let bd = payload.get("bytes_downloaded").and_then(|v| v.as_i64()).unwrap_or(0);
    let bt = payload
        .get("bytes_total")
        .and_then(|v| v.as_i64())
        .filter(|&x| x > 0)
        .unwrap_or(old_bt);
    let pct = payload.get("percent").and_then(|v| v.as_f64());

    let progress = if stage_phase {
        old_progress.max(99)
    } else if let Some(p) = pct {
        (p as i64).clamp(0, 100)
    } else if bt > 0 {
        (bd * 100 / bt).clamp(0, 100)
    } else {
        old_progress
    };

    let _ = conn.execute(
        "UPDATE deploy_task_nodes SET status = ?1, progress = ?2, bytes_downloaded = ?3, bytes_total = ?4 \
         WHERE task_id = ?5 AND node_id = ?6",
        rusqlite::params![new_status, progress, bd, bt, task_id, node_id],
    );

    state.bus.publish_threadsafe(
        "deploy_progress",
        json!({
            "type": "deploy_progress",
            "task_id": task_id,
            "node_id": node_id,
            "progress": progress,
            "bytes_downloaded": bd,
            "bytes_total": bt,
            "ts": now_epoch(),
        }),
    );
}

fn enums_active(s: &str) -> bool {
    crate::enums::is_active_node_state(s)
}

async fn handle_ack(state: &SharedState, node_id: &str, obj: &serde_json::Map<String, Value>, payload: &Value) {
    let cmd_id = obj.get("cmd_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // 合并 payload + 顶层字段（顶层覆盖 payload 同名键，排除 type/payload/cmd_id/null）。
    let mut ack = payload.clone();
    let ack_obj = ack.as_object_mut().unwrap();
    for (k, v) in obj {
        if k == "type" || k == "payload" || k == "cmd_id" || v.is_null() {
            continue;
        }
        ack_obj.insert(k.clone(), v.clone());
    }

    if !cmd_id.is_empty() {
        state.manager.resolve_pending(&cmd_id, ack.clone()).await;
    }

    // 文件传输回执：推进会话状态。
    let tid = ack.get("transfer_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !tid.is_empty() {
        if ack.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            let size = ack.get("bytes").and_then(|v| v.as_i64()).filter(|&x| x > 0);
            let sha = ack.get("sha256").and_then(|v| v.as_str()).unwrap_or("").to_string();
            state.transfers.mark(&tid, "done", "", size, &sha);
        } else {
            let err = ack.get("error").and_then(|v| v.as_str()).unwrap_or("传输失败").to_string();
            state.transfers.mark(&tid, "failed", &err, None, "");
        }
    }

    // 部署回执：把逐节点状态收敛到终态。
    let ack_status = ack.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let ack_task_id = ack.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if matches!(ack_status, "ready" | "failed" | "cancelled") && !ack_task_id.is_empty() {
        settle_deploy_ack(state, node_id, ack_task_id, ack_status, &ack);
    }

    // 服务生命周期回执（排除日志回执——日志带 lines）。
    let sid = ack.get("service_id").and_then(|v| v.as_str()).unwrap_or("");
    let st = ack.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if !sid.is_empty() && !st.is_empty() && ack.get("lines").is_none() {
        if matches!(st, "running" | "stopped" | "error" | "starting") {
            let conn = match state.pool.get() {
                Ok(c) => c,
                Err(_) => return,
            };
            if st == "running" {
                match ack.get("pid").and_then(|v| v.as_i64()) {
                    Some(p) => {
                        let _ = conn.execute(
                            "UPDATE services SET status = 'running', pid = ?1 WHERE service_id = ?2",
                            rusqlite::params![p, sid],
                        );
                    }
                    None => {
                        let _ = conn.execute(
                            "UPDATE services SET status = 'running' WHERE service_id = ?1",
                            [sid],
                        );
                    }
                }
            } else if st == "stopped" {
                let _ = conn.execute(
                    "UPDATE services SET status = 'stopped', pid = NULL WHERE service_id = ?1",
                    [sid],
                );
            } else {
                let _ = conn.execute(
                    "UPDATE services SET status = ?1 WHERE service_id = ?2",
                    rusqlite::params![st, sid],
                );
            }
        }
    }
}

fn settle_deploy_ack(state: &SharedState, node_id: &str, task_id: &str, ack_status: &str, ack: &Value) {
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let status_map = match ack_status {
        "ready" => "ready",
        "failed" => "failed",
        _ => "cancelled",
    };

    let row: Option<(String, i64, i64, String, String, Option<String>)> = conn
        .query_row(
            "SELECT status, progress, bytes_downloaded, error, rollback_note, finished_at \
             FROM deploy_task_nodes WHERE task_id = ?1 AND node_id = ?2",
            rusqlite::params![task_id, node_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .ok();
    let task_exists: bool = conn
        .query_row("SELECT 1 FROM deploy_tasks WHERE task_id = ?1", [task_id], |_| Ok(()))
        .is_ok();
    if !task_exists {
        return;
    }
    let Some((status, progress, bd_old, err_old, _rollback_old, finished_old)) = row else { return };

    let is_cancelled = status == "cancelled";
    let new_status = if is_cancelled { status.clone() } else { status_map.to_string() };
    let new_progress = if new_status == "ready" { 100 } else { progress };
    let new_bd = if is_cancelled {
        bd_old
    } else {
        ack.get("bytes_downloaded").and_then(|v| v.as_i64()).unwrap_or(bd_old)
    };
    let new_err = if is_cancelled {
        err_old.clone()
    } else {
        ack.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let new_rollback = ack
        .get("rollback_note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let new_finished = if crate::enums::is_terminal_node_state(&new_status) {
        Some(time::now_db())
    } else {
        finished_old
    };

    let _ = conn.execute(
        "UPDATE deploy_task_nodes SET status = ?1, progress = ?2, bytes_downloaded = ?3, \
         error = ?4, rollback_note = ?5, finished_at = ?6 WHERE task_id = ?7 AND node_id = ?8",
        rusqlite::params![new_status, new_progress, new_bd, new_err, new_rollback, new_finished, task_id, node_id],
    );

    if ack.get("rolled_back").and_then(|v| v.as_bool()) == Some(false) && !new_rollback.is_empty() {
        eprintln!("节点回滚未完整完成 task={task_id} node={node_id}：{new_rollback}");
    }

    // 聚合任务状态。
    let mut statuses: Vec<String> = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT status FROM deploy_task_nodes WHERE task_id = ?1") {
        if let Ok(iter) = stmt.query_map([task_id], |r| r.get::<_, String>(0)) {
            for s in iter.flatten() {
                statuses.push(s);
            }
        }
    }
    let agg = crate::enums::aggregate_deploy_status(&statuses);
    let task_terminal = matches!(agg.as_str(), "succeeded" | "failed" | "partial" | "cancelled");
    if task_terminal {
        let _ = conn.execute(
            "UPDATE deploy_tasks SET status = ?1, finished_at = COALESCE(finished_at, ?2) WHERE task_id = ?3",
            rusqlite::params![agg, time::now_db(), task_id],
        );
    } else {
        let _ = conn.execute(
            "UPDATE deploy_tasks SET status = ?1 WHERE task_id = ?2",
            rusqlite::params![agg, task_id],
        );
    }
}

fn reconcile_services(state: &SharedState, node_id: &str, reported: &[Value]) {
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    for item in reported {
        let Some(obj) = item.as_object() else { continue };
        let sid = obj.get("service_id").and_then(|v| v.as_str()).unwrap_or("");
        let st = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if sid.is_empty() || !matches!(st, "running" | "stopped") {
            continue;
        }
        let row: Option<(String, String, Option<i64>, Option<i64>, i64, String)> = conn
            .query_row(
                "SELECT node_id, status, pid, last_exit_code, restart_count, last_error \
                 FROM services WHERE service_id = ?1",
                [sid],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        let Some((svc_node, cur_status, cur_pid, cur_exit, cur_restarts, cur_err)) = row else { continue };
        if svc_node != node_id {
            continue;
        }

        let pid_int = obj.get("pid").and_then(|v| v.as_i64());
        let exit_int = obj.get("exit_code").and_then(|v| v.as_i64());
        let restarts = obj.get("restart_count").and_then(|v| v.as_i64()).unwrap_or(0);

        let want = if st == "running" {
            "running".to_string()
        } else if exit_int.is_some() && exit_int.unwrap() != 0 {
            "crashed".to_string()
        } else {
            "stopped".to_string()
        };

        let mut new_status = cur_status.clone();
        let mut new_pid = cur_pid;
        let mut new_exit = cur_exit;
        let mut new_restarts = cur_restarts;
        let mut new_err = cur_err.clone();
        let mut status_changed = false;

        if cur_status != want {
            new_status = want.clone();
            status_changed = true;
        }
        if st == "running" {
            if let Some(p) = pid_int {
                if cur_pid != Some(p) {
                    new_pid = Some(p);
                }
            }
        }
        if st == "stopped" {
            if cur_pid.is_some() {
                new_pid = None;
            }
        }
        if cur_exit != exit_int {
            new_exit = exit_int;
        }
        if restarts != 0 && cur_restarts != restarts {
            new_restarts = restarts;
        }
        if want == "running" && new_status == "running" && cur_err.starts_with("进程已退出") {
            new_err = String::new();
        }

        let changed = status_changed
            || new_pid != cur_pid
            || new_exit != cur_exit
            || new_restarts != cur_restarts
            || new_err != cur_err;
        if changed {
            let state_changed_val: Option<String> = if status_changed { Some(time::now_db()) } else { None };
            let _ = conn.execute(
                "UPDATE services SET status = ?1, pid = ?2, last_exit_code = ?3, restart_count = ?4, \
                 last_error = ?5, state_changed_at = COALESCE(?6, state_changed_at) WHERE service_id = ?7",
                rusqlite::params![new_status, new_pid, new_exit, new_restarts, new_err, state_changed_val, sid],
            );
        }
    }
}

// ================================================================= Dashboard

async fn ws_dashboard(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    RawQuery(query): RawQuery,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    let token = query_token(query.as_deref());
    if !ws_authorized(&state, token.as_deref()) {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    let _peer_ip = peer.ip().to_string();
    ws.on_upgrade(move |socket| dashboard_loop(socket, state))
}

async fn dashboard_loop(mut socket: WebSocket, state: SharedState) {
    let mut rx = state.bus.subscribe();
    let mut ping = tokio::time::interval(DASHBOARD_PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // 吞掉首拍，首个 ping 在 25 秒后

    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(v) => {
                        if socket.send(Message::Text(v.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            _ = ping.tick() => {
                let p = json!({"type": "ping", "ts": now_epoch()});
                if socket.send(Message::Text(p.to_string().into())).await.is_err() {
                    break;
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(_)) => {} // 只读，仅保活
                    _ => break,
                }
            }
        }
    }
}
