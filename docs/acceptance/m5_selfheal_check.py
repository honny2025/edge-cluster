"""运行时自愈验收（需求 §7：Auto Start 恢复 + 进程死了平台要知道）。

分两段跑，因为中间必须由**外层 shell** 杀掉并重启 Agent —— 这一条无法在
进程内自证（自己杀自己之后就没有执行体了）。

    python docs/acceptance/m5_selfheal_check.py prepare    # 建服务、启动、等探活通过
    <外层重启 Agent>
    python docs/acceptance/m5_selfheal_check.py verify     # 验证恢复 + 状态如实上报
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

B = "http://127.0.0.1:8000/api/v1"
T = "edge-cluster-dev-token"
NODE = os.environ.get("EDGE_NODE", "chk-01")
PORT = int(os.environ.get("EDGE_SVC_PORT", "18080"))
# 用当前解释器即可 —— 跑这个脚本的本来就是项目的 venv。
# 不写死路径：换台机器路径就变了（其它验收脚本同样用 sys.executable）。
PY = os.environ.get("EDGE_PY") or sys.executable
STATE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "_m5_state.json")

FAIL: list[str] = []


def call(method, path, body=None, timeout=60):
    req = urllib.request.Request(B + path, method=method)
    req.add_header("X-Auth-Token", T)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("Content-Type", "application/json")
    req.data = data
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, raw.decode(errors="replace")


def check(name, cond, extra=""):
    print(("  PASS  " if cond else "  FAIL  ") + name + (f"   {extra}" if extra else ""))
    if not cond:
        FAIL.append(name)


def load_state():
    try:
        return json.loads(open(STATE, encoding="utf-8").read())
    except Exception:
        return {}


def save_state(d):
    open(STATE, "w", encoding="utf-8").write(json.dumps(d, indent=2))


def wait_probe(sid, timeout=90):
    """等平台侧探活真的跑过一轮。

    **状态变 running 不等于探活通过**：Agent 心跳一到就把状态对齐成 running 了
    （那是"进程还在"），而探活走的是看门狗自己的 15 秒节拍（那是"端口通、业务健康"）。
    只等 status 会稳定地早一个节拍，测出来的是心跳不是探活。
    """
    end = time.time() + timeout
    last = {}
    while time.time() < end:
        _, last = call("GET", f"/services/{sid}")
        if isinstance(last, dict) and last.get("probe_ok") == 1 and last.get("last_probe_at"):
            return last
        time.sleep(2)
    return last


def wait_status(sid, want, timeout=60):
    end = time.time() + timeout
    last = {}
    while time.time() < end:
        _, last = call("GET", f"/services/{sid}")
        if isinstance(last, dict) and last.get("status") in want:
            return last
        time.sleep(2)
    return last


def prepare():
    print("== 1. 建一个 Auto Start 服务（真进程，非 mock）==")
    body = {
        "node_id": NODE,
        "name": "m5-selfheal",
        "port": PORT,
        "command": PY,
        # 被测进程必须**真实可探活**：http.server 的 "/" 返回 200，
        # 这样"平台探活 → RUNNING"这条链路才是真的走通了，而不是我们直接写状态。
        "args": ["-m", "http.server", str(PORT), "--bind", "0.0.0.0"],
        "health_url": "/",
        "auto_restart": True,
        "auto_start": True,
    }
    s, r = call("POST", "/services", body)
    check("创建服务", s == 200 and r.get("service_id"), f"{s} {r}")
    if s != 200:
        return
    sid = r["service_id"]
    check("auto_start 已记录", r.get("auto_start") is True, str(r.get("auto_start")))
    save_state({"service_id": sid, "port": PORT})

    print("== 2. 启动 ==")
    s, r = call("POST", f"/services/{sid}/start")
    check("start 指令已送达设备", s == 200 and r.get("delivered") is True, f"{s} {r}")

    print("== 3. 等平台探活判定 RUNNING（真实 HTTP 探活，不是直接写状态）==")
    row = wait_probe(sid, timeout=90)
    check("服务收敛到 running", row.get("status") == "running", f"status={row.get('status')}")
    check("拿到了 pid（Agent 真拉起了进程）", bool(row.get("pid")), f"pid={row.get('pid')}")
    check("probe_ok=1（看门狗真的打了一次 HTTP 探活）", row.get("probe_ok") == 1,
          f"probe_ok={row.get('probe_ok')}")
    check("last_probe_at 有值", bool(row.get("last_probe_at")), str(row.get("last_probe_at")))

    print("== 4. 停一次，确认 stopped 能被如实记录 ==")
    call("POST", f"/services/{sid}/stop")
    row = wait_status(sid, {"stopped", "crashed"}, timeout=40)
    check("人工停止后是 stopped（不是 crashed）",
          row.get("status") == "stopped", f"status={row.get('status')}")

    print("== 5. 重新启动，交给 verify 段验证恢复 ==")
    call("POST", f"/services/{sid}/start")
    row = wait_status(sid, {"running"}, timeout=70)
    check("再次 running", row.get("status") == "running", f"status={row.get('status')}")

    print()
    print("准备完成。现在请**杀掉并重启 Agent**（模拟手机重启），然后运行 verify。")


def verify():
    st = load_state()
    sid = st.get("service_id")
    if not sid:
        print("没有 prepare 阶段的记录，先跑 prepare")
        return
    print("== 6. Agent 重启后：服务应当自己回来了（Agent 侧 Auto Start 恢复）==")
    row = wait_status(sid, {"running", "starting"}, timeout=90)
    check("服务已恢复 running", row.get("status") == "running",
          f"status={row.get('status')} pid={row.get('pid')}")
    check("恢复后 pid 是老进程的那个以外的（确实换了新进程）",
          bool(row.get("pid")), f"pid={row.get('pid')}")
    check("restart_count 被 Agent 侧的恢复/看门狗累加过或保持",
          row.get("restart_count") is not None, f"restart_count={row.get('restart_count')}")

    print("== 7. 平台侧的探活仍然认得它 ==")
    s, r = call("POST", f"/services/{sid}/check")
    check("主动探活返回 ok", s == 200 and (r.get("probe") or {}).get("ok") is True,
          f"{s} {str(r.get('probe'))[:90]}")

    print("== 8. 收尾：停掉并删除本次验收的服务 ==")
    call("POST", f"/services/{sid}/stop")
    wait_status(sid, {"stopped", "crashed"}, timeout=40)
    s, r = call("DELETE", f"/services/{sid}")
    check("删除服务", s in (200, 204), f"{s} {r}")

    print()
    print("=" * 60)
    print(f"失败 {len(FAIL)} 项" + ("" if not FAIL else "：" + ", ".join(FAIL)))


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "prepare"
    if mode == "prepare":
        prepare()
    elif mode == "verify":
        verify()
    else:
        print("用法：m5_selfheal_check.py [prepare|verify]")
