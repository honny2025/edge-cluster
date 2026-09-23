"""UDP 广播自动发现（任务书 §3：UDP 广播或 mDNS 二选一，配手工 IP 兜底）。

流程：
  Agent 广播 EDGE_DISCOVER{token}  →  平台 UDP 监听器收到
  平台回单播 EDGE_DISCOVER_ACK{token, server_http, server_ws, server_name}
  Agent 拿到地址后建立 WebSocket 长连接。

为什么用单播回包：广播回的包在部分 Android/Termux 环境下不可靠，
且单播能让多平台实例场景下 Agent 准确地跟"响应它的那个平台"配对。

健壮性：
- 运行在独立线程里，用阻塞 socket + 超时轮询，异常自动续跑；
- 任何解析失败只记 debug 日志，不中断监听。
"""

from __future__ import annotations

import json
import socket
import threading
import time
from typing import Any

from ..config import settings
from ..core.logging_conf import get_logger
from ..core.security import verify_token
from .event_bus import bus

logger = get_logger("services.discovery")

DISCOVER_REQ = "EDGE_DISCOVER"
DISCOVER_ACK = "EDGE_DISCOVER_ACK"
PROTOCOL_VERSION = 1


def _local_ips() -> list[str]:
    """尽力枚举本机 IPv4 地址（用于日志与多网卡提示）。"""
    ips: set[str] = set()
    try:
        hostname = socket.gethostname()
        for info in socket.getaddrinfo(hostname, None, socket.AF_INET):
            ips.add(info[4][0])
    except Exception:
        pass
    # 用 UDP 连接外网地址的方式拿到默认出口 IP（不真正发包）
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout(0.2)
        try:
            s.connect(("8.8.8.8", 80))
            ips.add(s.getsockname()[0])
        finally:
            s.close()
    except Exception:
        pass
    return sorted(i for i in ips if not i.startswith("127."))


class DiscoveryService:
    """UDP 广播发现监听器。"""

    def __init__(self) -> None:
        self._thread: threading.Thread | None = None
        self._sock: socket.socket | None = None
        self._stop = threading.Event()
        self._counter = 0

    # ------------------------------------------------------------------ 生命周期

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._stop.clear()
        self._thread = threading.Thread(target=self._run, name="udp-discovery", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._sock is not None:
            try:
                self._sock.close()
            except Exception:
                pass
        if self._thread:
            self._thread.join(timeout=3)

    # ------------------------------------------------------------------ 主循环

    def _run(self) -> None:
        port = int(settings.server.discovery_port)
        sock: socket.socket | None = None
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
            except Exception:
                pass
            sock.bind(("", port))
            sock.settimeout(1.0)
            self._sock = sock
        except Exception as exc:
            logger.error("UDP 发现监听启动失败（自动发现不可用，可用手工填 IP 兜底）port=%s err=%s",
                         port, exc)
            return

        logger.info("UDP 发现监听已启动 port=%s 本机 IP=%s", port, ",".join(_local_ips()) or "未知")

        while not self._stop.is_set():
            try:
                data, addr = sock.recvfrom(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            except Exception as exc:
                logger.debug("UDP 收包异常（忽略）err=%s", exc)
                continue
            try:
                self._handle(data, addr, sock)
            except Exception as exc:
                logger.debug("UDP 报文处理失败（忽略）from=%s err=%s", addr, exc)

        try:
            sock.close()
        except Exception:
            pass
        logger.info("UDP 发现监听已停止")

    # ------------------------------------------------------------------ 报文处理

    def _handle(self, data: bytes, addr: tuple, sock: socket.socket) -> None:
        try:
            msg: dict[str, Any] = json.loads(data.decode("utf-8", errors="ignore"))
        except Exception:
            return
        if not isinstance(msg, dict) or msg.get("type") != DISCOVER_REQ:
            return

        if not verify_token(msg.get("token")):
            # token 不对：静默丢弃 + debug 日志（不回包，避免暴露平台存在）
            logger.debug("发现请求 token 校验失败，已忽略 from=%s", addr[0])
            return

        self._counter += 1
        server_http = self._server_http(msg)
        ack = {
            "type": DISCOVER_ACK,
            "version": PROTOCOL_VERSION,
            "token": settings.server.token,
            "server_name": socket.gethostname(),
            "server_http": server_http,
            "server_ws": server_http.replace("http://", "ws://").replace("https://", "wss://"),
            "heartbeat_interval": settings.server.heartbeat_interval,
            "ts": time.time(),
        }
        try:
            sock.sendto(json.dumps(ack).encode("utf-8"), addr)
        except Exception as exc:
            logger.warning("发现响应发送失败 to=%s err=%s", addr, exc)
            return

        logger.info(
            "UDP_DISCOVER_ACK 已响应 from=%s:%s agent_version=%s 累计响应=%d",
            addr[0], addr[1], msg.get("agent_version", "?"), self._counter,
        )
        bus.publish_threadsafe(
            "discovery_ack",
            {
                "from_ip": addr[0],
                "from_port": addr[1],
                "agent_version": msg.get("agent_version", ""),
                "agent_name": msg.get("name", ""),
                "server_http": server_http,
            },
        )

    def _server_http(self, req: dict[str, Any]) -> str:
        """返回给 Agent 的平台 HTTP 地址。

        优先使用请求里带的 `server_ip_hint`（Agent 看到的平台源 IP，最准确），
        其次使用配置中的对外地址，最后用本机默认出口 IP 兜底。
        """
        host = str(req.get("server_ip_hint") or "").strip()
        if not host:
            host = str(getattr(settings.server, "advertise_host", "") or "").strip()
        if not host:
            ips = _local_ips()
            host = ips[0] if ips else "127.0.0.1"
        return f"http://{host}:{settings.server.port}"


discovery = DiscoveryService()
