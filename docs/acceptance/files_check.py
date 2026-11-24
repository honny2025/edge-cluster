"""文件管理端到端验收（需求 §4）：真打 HTTP，不 mock。

前置：平台已启动，且至少一个模拟 Agent 在线（默认 chk-01）。
运行：python docs/acceptance/files_check.py


覆盖需求 §4 的全部动作：浏览 / 建目录 / 上传 / 下载 / 复制 / 移动 / 删除 / 大小统计 / 剩余空间。
"""
from __future__ import annotations
import json, os, time, urllib.error, urllib.parse, urllib.request, uuid, hashlib

B = "http://127.0.0.1:8000/api/v1"
T = "edge-cluster-dev-token"
NODE = "chk-01"
_HERE = os.path.dirname(os.path.abspath(__file__))          # server/data
_SERVER = os.path.dirname(_HERE)                            # server
_REPO = os.path.dirname(_SERVER)                            # 仓库根
ROOT = os.path.join(_REPO, "node-agent", "models")
D = os.path.join(ROOT, "e2e")

FAIL: list[str] = []

def call(method, path, body=None, timeout=90, raw=False):
    req = urllib.request.Request(B + path, method=method)
    req.add_header("X-Auth-Token", T)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("Content-Type", "application/json")
    req.data = data
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            b = r.read()
            return r.status, (b if raw else (json.loads(b) if b else None))
    except urllib.error.HTTPError as e:
        b = e.read()
        try:
            return e.code, json.loads(b)
        except Exception:
            return e.code, b.decode(errors="replace")

def q(s: str) -> str:
    return urllib.parse.quote(s)

def check(name, cond, extra=""):
    print(("  PASS  " if cond else "  FAIL  ") + name + (f"   {extra}" if extra else ""))
    if not cond:
        FAIL.append(name)

def multipart(fields, filename, content):
    b = "----e2e" + uuid.uuid4().hex
    parts = []
    for k, v in fields.items():
        parts.append(f'--{b}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode())
    parts.append(
        f'--{b}\r\nContent-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        f"Content-Type: application/octet-stream\r\n\r\n".encode() + content + b"\r\n"
    )
    parts.append(f"--{b}--\r\n".encode())
    return b, b"".join(parts)

def wait_transfer(tid, timeout=120):
    end = time.time() + timeout
    st = {}
    while time.time() < end:
        time.sleep(1.5)
        _, st = call("GET", f"/files/transfers/{tid}")
        if st.get("status") in ("done", "failed", "canceled"):
            return st
    return st

print("== 0. 剩余空间 / 根目录 ==")
s, r = call("GET", f"/nodes/{NODE}/files/roots")
roots = (r or {}).get("roots") or []
check("roots 返回根清单", s == 200 and len(roots) >= 1, f"{s} n={len(roots)}")
with_space = [x for x in roots if x.get("total")]
check("至少一个根报出容量（剩余空间）", bool(with_space),
      "总量均缺失" if not with_space else f"{with_space[0]['path']} free={with_space[0]['free']}")

print("== 1. mkdir ==")
s, r = call("POST", f"/nodes/{NODE}/files/mkdir", {"path": D})
check("mkdir 成功", s == 200, f"{s} {r}")

print("== 2. stat ==")
s, r = call("GET", f"/nodes/{NODE}/files/stat?path={q(D)}")
check("stat 认出目录", s == 200 and r.get("is_dir") is True, f"{s} {r}")

print("== 3. push：平台 -> 设备 ==")
CONTENT = b"edge-cluster-e2e-" + os.urandom(48).hex().encode()
target = D + r"\payload.bin"
b, body = multipart({"path": target, "overwrite": "true"}, "payload.bin", CONTENT)
req = urllib.request.Request(B + f"/nodes/{NODE}/files/push", data=body, method="POST")
req.add_header("X-Auth-Token", T)
req.add_header("Content-Type", f"multipart/form-data; boundary={b}")
with urllib.request.urlopen(req, timeout=120) as resp:
    push = json.loads(resp.read())
push_st = wait_transfer(push["transfer_id"])
check("push 落到设备", push_st.get("status") == "done", f"{push_st.get('status')} {push_st.get('error') or ''}")
check("push sha256 与源文件一致",
      push_st.get("sha256") == hashlib.sha256(CONTENT).hexdigest(),
      str(push_st.get("sha256"))[:16])

print("== 4. 设备侧文件确实存在且大小一致 ==")
s, r = call("GET", f"/nodes/{NODE}/files/stat?path={q(target)}")
check("设备上文件大小 == 上传大小", s == 200 and r.get("size") == len(CONTENT),
      f"{s} size={r.get('size') if isinstance(r, dict) else r} want={len(CONTENT)}")

print("== 5. 列目录 ==")
s, r = call("GET", f"/nodes/{NODE}/files?path={q(D)}")
entries = (r or {}).get("entries") or []
names = {e.get("name") for e in entries}
check("目录里能看到刚推上去的文件", "payload.bin" in names, f"{s} {sorted(names)}")

print("== 6. du：大小统计 ==")
s, r = call("GET", f"/nodes/{NODE}/files/du?path={q(D)}")
check("du 有 total_size/file_count/dir_count",
      s == 200 and isinstance(r, dict) and "total_size" in r and "file_count" in r and "dir_count" in r,
      f"{s} {r}")
check("du 统计到的字节数 >= 上传大小",
      isinstance(r, dict) and (r.get("total_size") or 0) >= len(CONTENT),
      f"du={r.get('total_size') if isinstance(r, dict) else r}")
check("du 带上所在文件系统的剩余空间", isinstance(r, dict) and (r.get("space") or {}).get("free"),
      str((r.get("space") if isinstance(r, dict) else None)))

print("== 7. copy ==")
s, r = call("POST", f"/nodes/{NODE}/files/copy", {"src": target, "dst": D + r"\payload.copy.bin"})
check("copy 成功", s == 200, f"{s} {r}")
s, r2 = call("GET", f"/nodes/{NODE}/files/stat?path={q(D + chr(92) + 'payload.copy.bin')}")
check("副本存在", s == 200 and r2.get("size") == len(CONTENT), f"{s} {r2}")

print("== 8. move（改名）==")
s, r = call("POST", f"/nodes/{NODE}/files/move",
            {"src": D + r"\payload.copy.bin", "dst": D + r"\payload.moved.bin"})
check("move 成功", s == 200, f"{s} {r}")
s, r2 = call("GET", f"/nodes/{NODE}/files/stat?path={q(D + chr(92) + 'payload.copy.bin')}")
check("原路径已不存在（404 且说明原因）",
      s == 404 and "不存在" in str(r2), f"{s} {r2}")

print("== 9. pull：设备 -> 平台 ==")
s, r = call("POST", f"/nodes/{NODE}/files/pull", {"path": D + r"\payload.moved.bin"})
check("pull 建立会话", s == 200 and r.get("transfer_id"), f"{s} {r}")
if s == 200 and r.get("transfer_id"):
    pull_st = wait_transfer(r["transfer_id"])
    check("pull 完成", pull_st.get("status") == "done", f"{pull_st.get('status')} {pull_st.get('error') or ''}")
    check("pull sha256 与源一致",
          pull_st.get("sha256") == hashlib.sha256(CONTENT).hexdigest(), str(pull_st.get("sha256"))[:16])
    print("== 10. 平台侧下载已取回的文件 ==")
    s, blob = call("GET", f"/files/transfers/{r['transfer_id']}/download", raw=True)
    check("download 内容字节完全一致", s == 200 and blob == CONTENT,
          f"{s} len={len(blob) if isinstance(blob, (bytes, bytearray)) else '?'} want={len(CONTENT)}")

print("== 11. 删除（非空目录必须显式 recursive）==")
s, r = call("POST", f"/nodes/{NODE}/files/delete", {"path": D, "recursive": False})
check("非空目录不给 recursive 时被拒绝", s == 400, f"{s} {r}")
s, r = call("POST", f"/nodes/{NODE}/files/delete", {"path": D, "recursive": True})
check("显式 recursive 后删除成功", s == 200, f"{s} {r}")
s, r = call("GET", f"/nodes/{NODE}/files/stat?path={q(D)}")
check("删除后目录确实没了（404 且说明原因）",
      s == 404 and "不存在" in str(r), f"{s} {r}")


BS = chr(92)

print("== 12. 沙箱边界（越界必须被拒）==")
for bad, why in [
    ("C:" + BS + "Windows" + BS + "System32", "系统目录"),
    ("E:" + BS, "盘根"),
    (ROOT + BS + ".." + BS + ".." + BS + "..", "用 .. 爬出根"),
    (ROOT + "/../../../../Windows", "混用分隔符爬出"),
    ("", "空路径"),
]:
    s2, r2 = call("GET", f"/nodes/{NODE}/files/stat?path={q(bad)}")
    ok = s2 in (400, 404) or (s2 == 200 and isinstance(r2, dict) and str(r2.get("path", "")).startswith(ROOT))
    check(f"越界被拒：{why}", ok, f"{s2} {str(r2)[:70]}")

print("== 13. 传输令牌边界 ==")
def probe(url, headers):
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code

check("不存在的传输会话 → 401",
      probe(B + "/files/transfers/0000000000000000/data", {}) == 401)
check("伪造传输令牌 → 401",
      probe(B + "/files/transfers/0000000000000000/data",
            {"X-Transfer-Token": "forged"}) == 401)
check("拿共享 token 冒充传输令牌 → 401（两个信任域不混用）",
      probe(B + "/files/transfers/0000000000000000/data",
            {"X-Transfer-Token": T}) == 401)
check("设备 token 不能调管理 API → 401",
      probe(B + "/nodes", {"X-Auth-Token": "device-token-forged"}) == 401)
check("无 token 调文件浏览 → 401",
      probe(B + f"/nodes/{NODE}/files/roots", {}) == 401)

print()
print("=" * 60)
print(f"失败 {len(FAIL)} 项" + ("" if not FAIL else "：" + ", ".join(FAIL)))
