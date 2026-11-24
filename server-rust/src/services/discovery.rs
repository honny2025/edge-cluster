//! UDP 广播自动发现（任务书 §3）。与 Python services/discovery.py 对齐。
//!
//! Agent 广播 EDGE_DISCOVER{token} → 平台回单播 EDGE_DISCOVER_ACK。
//! 运行在独立线程，阻塞 socket + 超时轮询。

use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::config;
use crate::event_bus::EventBus;
use crate::security;

pub const DISCOVER_REQ: &str = "EDGE_DISCOVER";
pub const DISCOVER_ACK: &str = "EDGE_DISCOVER_ACK";
pub const PROTOCOL_VERSION: i64 = 1;

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 本机默认出口 IP（UDP connect 拿地址，不真正发包）。127.* 兜底。
fn local_ip() -> String {
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = s.local_addr() {
                let ip = addr.ip().to_string();
                if !ip.starts_with("127.") {
                    return ip;
                }
            }
        }
    }
    "127.0.0.1".to_string()
}

/// 本机「对某台手机可达」的网卡 IP。
///
/// 关键：多网卡机器（WiFi + 有线 + VPN + 虚拟机网卡）下，`local_ip()` 连公网
/// 8.8.8.8 拿到的是「默认出口」地址，可能跟手机根本不在同一网段 —— 手机按它
/// 回连必然失败。正确做法是把 UDP socket 连到「发来发现包的那台手机」的地址，
/// 让内核帮我们选「能路由到手机」的那块网卡，再取它的 local_addr。
/// 失败时退回 `local_ip()` 兜底。
fn local_ip_for(peer: &str) -> String {
    if let Ok(peer_ip) = peer.parse::<std::net::IpAddr>() {
        if !peer_ip.is_unspecified() && !peer_ip.is_loopback() {
            if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
                if s.connect((peer_ip, 80)).is_ok() {
                    if let Ok(addr) = s.local_addr() {
                        let ip = addr.ip().to_string();
                        if !ip.starts_with("127.") {
                            return ip;
                        }
                    }
                }
            }
        }
    }
    local_ip()
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// 返回给 Agent 的平台 HTTP 地址。优先 server_ip_hint，其次「对这台手机可达」的网卡 IP。
fn server_http(req: &Value, peer: &str) -> String {
    let hint = req
        .get("server_ip_hint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let host = if hint.is_empty() {
        local_ip_for(peer)
    } else {
        hint
    };
    format!("http://{}:{}", host, config::get().server.port)
}

fn handle(data: &[u8], peer: &str) -> Option<(String, Value)> {
    let msg: Value = serde_json::from_slice(data).ok()?;
    if !msg.is_object() || msg.get("type").and_then(|v| v.as_str()) != Some(DISCOVER_REQ) {
        return None;
    }
    let token = msg.get("token").and_then(|v| v.as_str());
    if !security::verify_token(token) {
        return None;
    }
    let server_http = server_http(&msg, peer);
    let server_ws = server_http
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let ack = serde_json::json!({
        "type": DISCOVER_ACK,
        "version": PROTOCOL_VERSION,
        "token": config::get().server.token,
        "server_name": hostname(),
        "server_http": server_http,
        "server_ws": server_ws,
        "heartbeat_interval": config::get().server.heartbeat_interval,
        "ts": now_epoch(),
    });
    Some((
        ack.to_string(),
        serde_json::json!({
            "from_ip": "",
            "from_port": 0,
            "agent_version": msg.get("agent_version").unwrap_or(&Value::String(String::new())),
            "agent_name": msg.get("name").unwrap_or(&Value::String(String::new())),
            "server_http": server_http,
        }),
    ))
}

/// 启动 UDP 发现监听线程（阻塞在独立线程，进程退出即结束）。
pub fn spawn_discovery(bus: EventBus) {
    let port = config::get().server.discovery_port;
    let _ = std::thread::Builder::new()
        .name("udp-discovery".to_string())
        .spawn(move || run(port, bus));
}

fn run(port: u16, bus: EventBus) {
    let sock = match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("UDP 发现监听启动失败（自动发现不可用，可手工填 IP）port={port} err={e}");
            return;
        }
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
    eprintln!("UDP 发现监听已启动 port={port} 本机 IP={}", local_ip());

    let mut buf = [0u8; 4096];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => {
                let peer = addr.ip().to_string();
                if let Some((ack, event)) = handle(&buf[..n], &peer) {
                    let mut ev = event;
                    ev["from_ip"] = serde_json::json!(addr.ip().to_string());
                    ev["from_port"] = serde_json::json!(addr.port());
                    let _ = sock.send_to(ack.as_bytes(), addr);
                    bus.publish_threadsafe("discovery_ack", ev);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    eprintln!("UDP 发现监听已停止");
}
