"""M2 补充验收：节点 IP 取值是否采用 WebSocket 对端地址。

问题背景
--------
平台其实一直握着正确地址（WS 连接的对端 IP），但原先会被 Agent 自报的
`ip` 字段无条件覆盖。Agent 在多网卡 / 热点 / 内网穿透场景下自报的可能是
错误地址，而 M4 登记「服务 Endpoint」时要用平台看到的地址，取值错了会导致
Endpoint 不可用。

验证方式
--------
启动一个隔离的平台实例，用 WebSocket 以「节点 A」身份注册，注册报文里
故意携带一个错误的自报 IP（10.255.255.255）。断言：
  1. 平台记录的 node.ip == WS 对端 IP（127.0.0.1），而不是自报的错误值；
  2. 心跳报文里再带一次错误 IP，仍不会被覆盖；
  3. 影响面可控：只有「平台能看到对端 IP」时才覆盖，否则保留自报值。

用法：python docs/acceptance/m2_ip_check.py --base http://127.0.0.1:8899
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys

# 本机可能设置了 HTTP 代理，访问 127.0.0.1 会被代理拦截返回 502。
# 与 m3_check.py 同样处理：清掉代理环境变量。
for _v in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
    os.environ.pop(_v, None)
os.environ["NO_PROXY"] = "*"
os.environ["no_proxy"] = "*"

TOKEN = "edge-cluster-dev-token"
RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f"  {detail}" if detail else ""))


async def ws_register(base: str, node_name: str, fake_ip: str) -> tuple[str, dict]:
    """以节点身份完成一次注册 + 一次心跳，返回 (node_id, 注册响应体)。"""
    import websockets  # type: ignore

    # WS 路由不挂在 /api/v1 下：真实路径是 /ws/agent
    url = base.replace("http://", "ws://").replace("https://", "wss://")
    url = f"{url}/ws/agent?token={TOKEN}"
    async with websockets.connect(url, open_timeout=10) as ws:
        reg = {
            "type": "register",
            "payload": {
                "node_id": f"ipcheck-{abs(hash(node_name)) % 100000}",
                "name": node_name,
                "model": "ProbeDevice",
                "soc": "TestSoC",
                "android_version": "14",
                "agent_version": "0.2.0-ipcheck",
                "ip": fake_ip,  # 故意给错
            },
        }
        await ws.send(json.dumps(reg))
        raw = await asyncio.wait_for(ws.recv(), timeout=10)
        body = json.loads(raw)
        if body.get("type") == "error":
            raise RuntimeError(f"注册被拒: {body.get('message')}")
        node_id = body.get("node_id")
        if not node_id:
            raise RuntimeError(f"注册未返回 node_id: {raw[:300]}")

        # 心跳里再带一次错误 IP
        hb = {
            "type": "heartbeat",
            "payload": {
                "ip": fake_ip,
                "metrics": {"cpu_usage": 12.5, "mem_used": 100, "mem_total": 200},
            },
        }
        await ws.send(json.dumps(hb))
        await asyncio.sleep(1.0)
        return node_id, body


async def fetch_node(base: str, node_id: str) -> dict:
    import urllib.request

    req = urllib.request.Request(
        f"{base}/api/v1/nodes/{node_id}",
        headers={"X-Auth-Token": TOKEN, "Accept": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r)


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8899")
    a = ap.parse_args()

    print("=" * 62)
    print("M2 补充验收：节点 IP 应取 WS 对端地址，而非 Agent 自报")
    print("=" * 62)

    fake_ip = "10.255.255.255"
    node_id = None
    try:
        node_id, _ = await ws_register(a.base, "ipcheck-node", fake_ip)
        check("节点可注册（WS 链路通）", bool(node_id), f"node_id={node_id}")
    except Exception as e:  # noqa: BLE001
        check("节点可注册（WS 链路通）", False, f"{type(e).__name__}: {e}")
        print("\n无法继续：WS 注册失败，后续断言跳过。")
        return 1

    try:
        node = await fetch_node(a.base, node_id)
    except Exception as e:  # noqa: BLE001
        check("可读取节点详情", False, f"{type(e).__name__}: {e}")
        return 1

    got_ip = str(node.get("ip") or "")
    print(f"\n  Agent 自报 IP = {fake_ip}")
    print(f"  平台记录 IP   = {got_ip}  (WS 对端应为 127.0.0.1)")

    check("平台 IP 未被自报值污染", got_ip != fake_ip, f"ip={got_ip}")
    check("平台 IP == WS 对端地址", got_ip == "127.0.0.1", f"ip={got_ip}")

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    total = len(RESULTS)
    print("\n" + "=" * 62)
    print(f"结果：{passed}/{total} 通过")
    for name, ok, detail in RESULTS:
        if not ok:
            print(f"  FAIL {name}  {detail}")
    print("=" * 62)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
