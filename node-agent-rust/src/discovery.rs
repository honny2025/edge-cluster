//! UDP 广播发现客户端。
//!
//! 1. 配置里已填 platform_url → 直接用（手工填 IP 兜底）；
//! 2. 否则向 broadcast_addr:port 广播 EDGE_DISCOVER，等待 EDGE_DISCOVER_ACK；
//! 3. 广播 attempts 次（间隔 interval），拿到响应即返回；
//! 4. 全部失败返回 None，由主流程走重连退避。

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use crate::config::Config;
use log::{info, warn};

pub const DISCOVER_REQ: &str = "EDGE_DISCOVER";
pub const DISCOVER_ACK: &str = "EDGE_DISCOVER_ACK";

/// 广播发现平台，返回平台 HTTP 基地址（如 http://192.168.1.100:8000），失败返回 None。
///
/// `force=true` 时忽略配置里的 platform_url，强制走一次 UDP 广播。
pub fn discover_platform(
    cfg: &Config,
    token: &str,
    agent_version: &str,
    name: &str,
    force: bool,
) -> Option<String> {
    if !cfg.agent.platform_url.is_empty() && !force {
        info!("配置中已指定平台地址，跳过 UDP 发现: {}", cfg.agent.platform_url);
        return Some(cfg.agent.platform_url.trim_end_matches('/').to_string());
    }

    if !cfg.discovery.enabled {
        warn!("UDP 发现已禁用且未配置 platform_url，无法定位平台");
        return None;
    }

    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            log::error!("UDP socket 创建失败: {e}");
            return None;
        }
    };
    let _ = sock.set_broadcast(true);
    if sock
        .set_read_timeout(Some(Duration::from_secs_f64(cfg.discovery.timeout)))
        .is_err()
    {
        return None;
    }
    let local = sock
        .local_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();

    let attempts = cfg.discovery.attempts.max(1);
    let mut found: Option<String> = None;

    for i in 1..=attempts {
        let req = serde_json::json!({
            "type": DISCOVER_REQ,
            "version": 1,
            "token": token,
            "agent_version": agent_version,
            "name": name,
            "local_ip": local,
            "server_ip_hint": "",
        });
        let dest = format!("{}:{}", cfg.discovery.broadcast_addr, cfg.discovery.port);
        match sock.send_to(req.to_string().as_bytes(), &dest) {
            Ok(_) => info!(
                "已广播发现请求 {}/{} -> {}:{}",
                i, cfg.discovery.attempts, cfg.discovery.broadcast_addr, cfg.discovery.port
            ),
            Err(e) => warn!("广播发送失败（第 {} 次）: {}", i, e),
        }

        let mut buf = [0u8; 4096];
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                let msg: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if msg.get("type").and_then(|t| t.as_str()) != Some(DISCOVER_ACK) {
                    continue;
                }
                if msg.get("token").and_then(|t| t.as_str()).unwrap_or("") != token {
                    log::debug!("收到 token 不匹配的发现响应，忽略来自 {}", addr.ip());
                    continue;
                }
                let url = msg
                    .get("server_http")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .to_string();
                if url.is_empty() {
                    continue;
                }
                info!(
                    "发现平台成功: {}（平台主机名={}，来自 {}:{}）",
                    url,
                    msg.get("server_name").and_then(|s| s.as_str()).unwrap_or("?"),
                    addr.ip(),
                    addr.port()
                );
                found = Some(url);
                break;
            }
            Err(_) => {
                log::debug!("等待发现响应超时（第 {} 次）", i);
            }
        }

        if i < attempts {
            std::thread::sleep(Duration::from_secs_f64(cfg.discovery.interval));
        }
    }

    if found.is_none() {
        warn!(
            "UDP 发现失败（共尝试 {} 次）。可在 config.toml 填写 platform_url 手工指定平台地址。",
            cfg.discovery.attempts
        );
    }
    found
}

// 供 `SocketAddr` 可能的将来使用，避免未用导入告警。
#[allow(dead_code)]
fn _unused_socket_addr(_a: SocketAddr) {}
