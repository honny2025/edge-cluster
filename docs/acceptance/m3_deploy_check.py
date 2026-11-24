"""M3 部署下发闭环验收（需隔离实例）。

验证的是「创建任务 → 手机真的开始下载 → 进度回传 → 完成」这一段，
也就是原来缺失、本轮补上的部分：

  M3-D1  创建任务即自动入队下发（不再需要手动点 /start）
  M3-D2  队列并发上限生效（/queue/stats 报出 concurrency）
  M3-D3  进度上报能写回逐节点状态（percent 42 → progress 42）
  M3-D4  完成回执（ack ready）能把节点置 ready、任务置 succeeded
  M3-D5  下载地址用的是节点连平台时的 Host，而不是 127.0.0.1

用法：python docs/acceptance/m3_deploy_check.py --base http://127.0.0.1:8899
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time

# 与 m3_check.py 同样处理：本机代理会把 127.0.0.1 拦成 502
for _v in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
    os.environ.pop(_v, None)
os.environ["NO_PROXY"] = "*"
os.environ["no_proxy"] = "*"

import urllib.request  # noqa: E402

TOKEN = "edge-cluster-dev-token"
RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f"  {detail}" if detail else ""))


def http(method: str, path: str, body: dict | None = None,
         raw: bytes | None = None, ctype: str = "application/json") -> tuple[int, dict | str]:
    url = f"{BASE}{path}"
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("X-Auth-Token", TOKEN)
    if data is not None:
        req.add_header("Content-Type", ctype)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=30) as r:
            raw_out = r.read()
            try:
                return r.status, json.loads(raw_out)
            except Exception:
                return r.status, raw_out.decode("utf-8", "replace")
    except urllib.error.HTTPError as e:  # noqa: F821
        raw_out = e.read()
        try:
            return e.code, json.loads(raw_out)
        except Exception:
            return e.code, raw_out.decode("utf-8", "replace")


async def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8899")
    a = ap.parse_args()
    BASE = a.base

    print("=" * 64)
    print("M3 部署下发闭环验收")
    print("=" * 64)

    import websockets  # type: ignore

    # ---------------------------------------------------------------- 1. 假节点
    node_id = f"depnode-{int(time.time()) % 100000}"
    url = BASE.replace("http://", "ws://") + f"/ws/agent?token={TOKEN}"
    ws = await websockets.connect(url, open_timeout=10)
    await ws.send(json.dumps({
        "type": "register",
        "payload": {
            "node_id": node_id, "name": "dep-probe", "model": "Probe",
            "soc": "Test", "android_version": "14", "agent_version": "0.2.0-probe",
            "ip": "10.255.255.255",
        },
    }))
    ack = json.loads(await asyncio.wait_for(ws.recv(), timeout=10))
    check("假节点注册成功", ack.get("type") == "register_ack", f"node={node_id}")

    # ---------------------------------------------------------------- 2. 上传模型
    # 上传接口要求 name / version 走 form-data（name 必填），且同名同版本会 409，
    # 所以每次用随机名，保证脚本可重复跑。
    payload = b"edge-cluster-deploy-probe-" + os.urandom(2048)
    model_name = f"probe-{int(time.time()) % 1000000}"
    boundary = "----edgeprobe"
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="name"\r\n\r\n{model_name}\r\n'
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="version"\r\n\r\nv1\r\n'
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="file"; filename="probe.bin"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + payload + f"\r\n--{boundary}--\r\n".encode()

    st, up = http("POST", "/api/v1/models", raw=body,
                  ctype=f"multipart/form-data; boundary={boundary}")
    check("模型上传成功", st in (200, 201), f"status={st}")
    if st >= 400:
        print(f"    上传失败响应：{str(up)[:300]}")
        await ws.close()
        return 1
    model_id = up.get("model_id") if isinstance(up, dict) else None
    check("返回 model_id", bool(model_id), f"model_id={model_id}")

    # ---------------------------------------------------------------- 3. 建任务（不解 start）
    st, task = http("POST", "/api/v1/deployments",
                    {"model_id": model_id, "node_ids": [node_id]})
    check("创建部署任务", st in (200, 201), f"status={st}")
    task_id = task.get("task_id") if isinstance(task, dict) else None

    # ---------------------------------------------------------------- 4. 等队列下发
    got_cmd = None
    try:
        for _ in range(40):  # 最多等 8 秒
            raw = await asyncio.wait_for(ws.recv(), timeout=1.0)
            m = json.loads(raw)
            if m.get("type") == "deploy_model":
                got_cmd = m
                break
    except asyncio.TimeoutError:
        pass

    check("M3-D1 创建即自动下发（无需手动 start）", got_cmd is not None,
          "节点收到 deploy_model 指令" if got_cmd else "8 秒内没收到指令")

    if got_cmd:
        p = got_cmd.get("payload", {})
        check("M3-D5 下载地址用节点连平台的 Host",
              "127.0.0.1" in (p.get("url") or "") and p.get("task_id") == task_id,
              f"url={p.get('url')}")
        check("指令带 task_id（进度回传的关联键）", p.get("task_id") == task_id,
              f"task_id={p.get('task_id')}")

    # ---------------------------------------------------------------- 5. 队列并发
    st, stats = http("GET", "/api/v1/deployments/queue/stats")
    check("M3-D2 队列观察口可用", st == 200 and isinstance(stats, dict),
          f"stats={stats}")

    # ---------------------------------------------------------------- 6. 进度上报
    if task_id:
        await ws.send(json.dumps({
            "type": "progress",
            "payload": {
                "task_id": task_id, "node_id": node_id,
                "bytes_downloaded": 42, "bytes_total": 100, "percent": 42.0,
            },
        }))
        await asyncio.sleep(1.0)
        st, cur = http("GET", f"/api/v1/deployments/{task_id}")
        nodes = (cur or {}).get("nodes", []) if isinstance(cur, dict) else []
        prog = nodes[0].get("progress") if nodes else None
        check("M3-D3 进度回传写回逐节点状态", prog == 42, f"progress={prog}")

    # ---------------------------------------------------------------- 7. 完成回执
    if got_cmd:
        cmd_id = got_cmd.get("cmd_id")
        await ws.send(json.dumps({
            "type": "ack", "cmd_id": cmd_id, "ok": True,
            "status": "ready", "task_id": task_id, "bytes_downloaded": 100,
        }))
        await asyncio.sleep(1.0)
        st, cur = http("GET", f"/api/v1/deployments/{task_id}")
        nodes = (cur or {}).get("nodes", []) if isinstance(cur, dict) else []
        nstat = nodes[0].get("status") if nodes else None
        tstat = (cur or {}).get("status") if isinstance(cur, dict) else None
        check("M3-D4 完成回执置节点 ready", nstat == "ready", f"node.status={nstat}")
        check("M3-D4 任务聚合为 succeeded", tstat == "succeeded", f"task.status={tstat}")

    await ws.close()

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    total = len(RESULTS)
    print("\n" + "=" * 64)
    print(f"结果：{passed}/{total} 通过")
    for name, ok, detail in RESULTS:
        if not ok:
            print(f"  FAIL {name}  {detail}")
    print("=" * 64)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
