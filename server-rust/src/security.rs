//! 鉴权：共享 token + 每设备 token（需求 §8）。
//!
//! 三层令牌与 Python 版 app/core/security.py 对齐：
//! 1. 共享 token（server.token）：REST + 首次 WS 接入。
//! 2. 设备 token（nodes.device_token）：WS 握手，register_ack 下发。
//! 3. 传输/下载 token：一次性，见 model_tokens / file_transfers。

use crate::config;
use crate::db::DbPool;
use crate::time;

/// 常数时间比较，避免时序侧信道。
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 校验是否为平台共享 token。
pub fn verify_token(token: Option<&str>) -> bool {
    let cfg = config::get();
    let expected = cfg.server.token.as_str();
    match token {
        Some(t) if !t.is_empty() => constant_time_eq(t, expected),
        _ => false,
    }
}

/// 校验是否为某台已登记设备的设备 token。命中返回 node_id。
pub fn verify_device_token(token: Option<&str>, pool: &DbPool) -> Option<String> {
    let t = token?;
    if t.is_empty() {
        return None;
    }
    let conn = pool.get().ok()?;
    let mut stmt = conn
        .prepare("SELECT node_id, device_token FROM nodes WHERE device_token != ''")
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .ok()?;
    for r in rows.flatten() {
        if constant_time_eq(t, &r.1) {
            return Some(r.0);
        }
    }
    None
}

/// 把一个令牌翻译成"它是谁"，用于审计。
/// 返回 "shared-token" / "device:<node_id>" / None。
pub fn identify(token: Option<&str>, pool: &DbPool) -> Option<String> {
    if verify_token(token) {
        return Some("shared-token".to_string());
    }
    if let Some(nid) = verify_device_token(token, pool) {
        return Some(format!("device:{nid}"));
    }
    None
}

/// 是否为某台已登记设备签发过 token（用于 enforce_device_token 判定）。
pub fn any_device_token_exists(pool: &DbPool) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT 1 FROM nodes WHERE device_token != '' LIMIT 1",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// 为某台设备签发（或复用）device token。
/// 已有 token 且未强制轮换时原样返回。
pub fn issue_device_token(node_id: &str, pool: &DbPool, force: bool) -> String {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let existing: Option<String> = conn
        .query_row(
            "SELECT device_token FROM nodes WHERE node_id = ?1",
            [node_id],
            |r| r.get(0),
        )
        .ok();
    match existing {
        Some(tok) if !tok.is_empty() && !force => tok,
        _ => {
            let token = crate::util::new_urlsafe_token();
            let now = time::now_db();
            let _ = conn.execute(
                "UPDATE nodes SET device_token = ?1, device_token_issued_at = ?2 WHERE node_id = ?3",
                rusqlite::params![token, now, node_id],
            );
            token
        }
    }
}

/// 吊销某台设备的 token。返回是否确实吊销了一个。
#[allow(dead_code)] // 与 Python 版对齐，设备重置场景预留
pub fn revoke_device_token(node_id: &str, pool: &DbPool) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let had: bool = conn
        .query_row(
            "SELECT device_token != '' FROM nodes WHERE node_id = ?1",
            [node_id],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false);
    if !had {
        return false;
    }
    let _ = conn.execute(
        "UPDATE nodes SET device_token = '', device_token_issued_at = NULL WHERE node_id = ?1",
        [node_id],
    );
    true
}

/// 启动自检：仍是默认 token 就大声警告。
pub fn warn_if_default_token() {
    let cfg = config::get();
    if !cfg.security.warn_on_default_token {
        return;
    }
    if cfg.server.token == "edge-cluster-dev-token" {
        eprintln!("{}", "=".repeat(70));
        eprintln!("⚠  正在使用默认共享 token（edge-cluster-dev-token）");
        eprintln!("⚠  同一局域网内任何人都能凭它接管你的设备。");
        eprintln!("⚠  上线前请修改 server-rust/config.toml 的 [server] token，");
        eprintln!("⚠  并同步修改每台手机 node-agent-rust/config.toml 的 [agent] token。");
        eprintln!("{}", "=".repeat(70));
    }
}
