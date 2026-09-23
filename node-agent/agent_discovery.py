"""UDP 广播发现客户端。

策略：
1. 若配置里已填 `platform_url`，直接用（手工填 IP 兜底路径）；
2. 否则向 `broadcast_addr:port` 广播 `EDGE_DISCOVER`，等待平台的 `EDGE_DISCOVER_ACK`；
3. 广播 attempts 次（间隔 interval），拿到响应即返回；
4. 全部失败返回 None，由主流程走重连退避。

`server_ip_hint` 用途：平台可能在多网卡机器上跑，单纯靠 socket 源地址推断
自己的对外地址并不可靠；把"Agent 观察到的平台源 IP"回传，平台据此生成
对 Agent 可达的地址（这是最准确的信息来源）。
"""

from __future__ import annotations

import json
import socket
from typing import Any

from agent_utils import get_logger

log = get_logger("agent.discovery")

DISCOVER_REQ = "EDGE_DISCOVER"
DISCOVER_ACK = "EDGE_DISCOVER_ACK"


def discover_platform(cfg: Any, token: str, agent_version: str, name: str = "") -> str | None:
    """广播发现平台，返回平台 HTTP 基地址（如 http://192.168.1.100:8000），失败返回 None。"""
    if cfg.agent.platform_url:
        log.info("配置中已指定平台地址，跳过 UDP 发现: %s", cfg.agent.platform_url)
        return cfg.agent.platform_url.rstrip("/")

    if not cfg.discovery.enabled:
        log.warning("UDP 发现已禁用且未配置 platform_url，无法定位平台")
        return None

    sock = None
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        sock.settimeout(float(cfg.discovery.timeout))
        sock.bind(("", 0))  # 平台只要求能看到源 IP，不需要固定端口
        local = sock.getsockname()[0]
    except Exception as exc:
        log.error("UDP socket 创建失败: %s", exc)
        if sock:
            sock.close()
        return None

    try:
        for i in range(1, int(cfg.discovery.attempts) + 1):
            req = {
                "type": DISCOVER_REQ,
                "version": 1,
                "token": token,
                "agent_version": agent_version,
                "name": name,
                "local_ip": local,
                # 让平台用它观察到的源 IP 生成回连地址（最准确）
                "server_ip_hint": "",
            }
            try:
                sock.sendto(
                    json.dumps(req).encode("utf-8"),
                    (cfg.discovery.broadcast_addr, int(cfg.discovery.port)),
                )
                log.info(
                    "已广播发现请求 %d/%d -> %s:%s",
                    i, cfg.discovery.attempts, cfg.discovery.broadcast_addr,
                    cfg.discovery.port,
                )
            except Exception as exc:
                log.warning("广播发送失败（第 %d 次）: %s", i, exc)

            try:
                data, addr = sock.recvfrom(4096)
                msg: dict[str, Any] = json.loads(data.decode("utf-8", errors="ignore"))
                if msg.get("type") != DISCOVER_ACK:
                    continue
                if str(msg.get("token") or "") != str(token):
                    log.debug("收到 token 不匹配的发现响应，忽略来自 %s", addr[0])
                    continue
                url = str(msg.get("server_http") or "").rstrip("/")
                if not url:
                    continue
                log.info(
                    "发现平台成功: %s（平台主机名=%s，来自 %s:%s）",
                    url, msg.get("server_name", "?"), addr[0], addr[1],
                )
                return url
            except socket.timeout:
                log.debug("等待发现响应超时（第 %d 次）", i)
            except Exception as exc:
                log.debug("发现响应解析失败（继续尝试）: %s", exc)

            # 两次尝试之间短暂等待
            if i < int(cfg.discovery.attempts):
                try:
                    import time

                    time.sleep(float(cfg.discovery.interval))
                except Exception:
                    pass
    finally:
        try:
            sock.close()
        except Exception:
            pass

    log.warning(
        "UDP 发现失败（共尝试 %d 次）。可在 config.toml 填写 platform_url 手工指定平台地址。",
        cfg.discovery.attempts,
    )
    return None
