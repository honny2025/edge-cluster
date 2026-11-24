#!/usr/bin/env python3
"""里程碑 3 验收脚本（上半：模型库 + 下载口）。

自动断言 §5「模型库与模型下发」里**平台侧**需要成立的前提：

  M3-1 鉴权：/models、/deployments、/services 无 token 必须 401
  M3-2 上传：流式落盘，`sha256` 与本地一致（下发的唯一成功判据）
  M3-3 版本唯一：同名同版本重复上传 → 409，不是 500
  M3-4 全量下载：200 + 内容逐字节一致 + `Accept-Ranges` / `ETag`
  M3-5 断点续传：`Range` → 206，两段拼回来必须等于原文件
  M3-6 续传一致性：`If-Range` 与当前 ETag 不符时必须**回落全量 200**，
        否则 Agent 会拼出"前半旧版 + 后半新版"的坏文件
  M3-7 下载计数：全量下载才 +1，续传不重复计数
  M3-8 删除：记录与文件都要消失

用法：
    python docs/acceptance/m3_check.py
    python docs/acceptance/m3_check.py --base http://127.0.0.1:8000 --token xxx

脚本自建自删，只用一个 2MiB 临时文件，不污染模型库。
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import sys
import urllib.error
import urllib.request
import uuid

TOKEN = "edge-cluster-dev-token"
DEFAULT_BASE = "http://127.0.0.1:8000"

_args: argparse.Namespace
_results: list[tuple[str, bool, str]] = []

# ★ 必须绕开系统代理：本机/公司环境常设 HTTP_PROXY，访问 127.0.0.1 会被代理
#   拦掉并返回 502，看起来像"平台挂了"，实际是代理在捣乱（Agent 侧同款坑）。
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok


def call(method: str, path: str, *, data: bytes | None = None,
         headers: dict | None = None, token: bool = True, raw: bool = False):
    """返回 (status, headers(dict，键统一小写), body)。HTTP 错误不抛异常。"""
    h = {}
    if token:
        h["X-Auth-Token"] = _args.token
    if headers:
        h.update(headers)
    req = urllib.request.Request(_args.base + "/api/v1" + path, data=data, headers=h, method=method)
    try:
        with _opener.open(req, timeout=60) as r:
            body = r.read()
            return r.status, {k.lower(): v for k, v in r.headers.items()}, body if raw else body.decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        body = e.read()
        return e.code, {k.lower(): v for k, v in (e.headers or {}).items()}, body if raw else body.decode("utf-8", "replace")
    except Exception as exc:
        return 0, {}, f"请求异常: {exc}"


def upload(name: str, version: str, payload: bytes) -> tuple[int, dict]:
    boundary = "----m3check" + uuid.uuid4().hex
    body = io.BytesIO()

    def field(k: str, v: str) -> None:
        body.write(f'--{boundary}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode())

    field("name", name)
    field("version", version)
    field("framework", "other")
    field("format", "bin")
    field("description", "m3_check temp")
    body.write(
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="probe.bin"\r\n'
        f"Content-Type: application/octet-stream\r\n\r\n".encode()
    )
    body.write(payload)
    body.write(f"\r\n--{boundary}--\r\n".encode())
    code, _, text = call("POST", "/models", data=body.getvalue(),
                         headers={"Content-Type": f"multipart/form-data; boundary={boundary}"})
    try:
        return code, json.loads(text)
    except json.JSONDecodeError:
        return code, {"_raw": text[:200]}


def main() -> int:
    global _args, TOKEN
    p = argparse.ArgumentParser(description="里程碑 3 验收（模型库 + 下载口）")
    p.add_argument("--base", default=DEFAULT_BASE)
    p.add_argument("--token", default=TOKEN)
    _args = p.parse_args()

    print("=" * 70)
    print(f"里程碑 3 验收（模型库 + 下载口）  base={_args.base}")
    print("=" * 70)

    # ---------------- M3-1 鉴权 ----------------
    print("\n[M3-1] 鉴权：能上传文件 / 能指挥手机下东西的接口，不允许裸奔")
    for path in ("/models", "/deployments", "/services"):
        code, _, _ = call("GET", path, token=False)
        check(f"无 token GET {path} → 401", code == 401, f"实际 {code}")
    code, _, _ = call("GET", "/models")
    check("带 token GET /models → 200", code == 200, f"实际 {code}")

    # ---------------- M3-2 上传 ----------------
    print("\n[M3-2] 上传：流式落盘 + sha256")
    payload = os.urandom(2 * 1024 * 1024)
    local_sha = hashlib.sha256(payload).hexdigest()
    name = f"m3-check-{uuid.uuid4().hex[:8]}"
    code, info = upload(name, "v1", payload)
    if not check("上传返回 200", code == 200, str(info)[:200]):
        return _summary()
    model_id = info.get("model_id")
    check("file_size 与本地一致", info.get("file_size") == len(payload),
          f"{info.get('file_size')} vs {len(payload)}")
    check("sha256 与本地一致", info.get("sha256") == local_sha, str(info.get("sha256")))

    # ---------------- M3-3 版本唯一 ----------------
    print("\n[M3-3] 版本唯一：同名同版本必须被拒（409），且不能留半个文件")
    code, info2 = upload(name, "v1", payload)
    check("重复上传 → 409", code == 409, f"实际 {code}")

    # ---------------- M3-4 全量下载 ----------------
    print("\n[M3-4] 全量下载")
    code, hdr, data = call("GET", f"/models/{model_id}/download", raw=True)
    check("全量下载 200", code == 200, f"实际 {code}")
    check("内容逐字节一致", data == payload, f"{len(data)} vs {len(payload)}")
    check("响应带 Accept-Ranges: bytes", hdr.get("accept-ranges") == "bytes",
          str(hdr.get("accept-ranges")))
    etag = hdr.get("etag") or ""
    check("响应带 ETag（续传一致性校验要用）", bool(etag), etag or "(无)")

    # ---------------- M3-5 断点续传 ----------------
    print("\n[M3-5] 断点续传：Range 分片拼回来必须等于原文件")
    half = len(payload) // 2
    c1, h1, p1 = call("GET", f"/models/{model_id}/download",
                      headers={"Range": f"bytes=0-{half - 1}"}, raw=True)
    c2, h2, p2 = call("GET", f"/models/{model_id}/download",
                      headers={"Range": f"bytes={half}-"}, raw=True)
    check("前半段 206", c1 == 206, f"实际 {c1}")
    check("后半段 206", c2 == 206, f"实际 {c2}")
    check("两段字节数正确", len(p1) == half and len(p2) == len(payload) - half,
          f"{len(p1)}+{len(p2)} vs {len(payload)}")
    check("拼接后与原文件一致（续传可用）", p1 + p2 == payload)
    check("分片响应带 Content-Range", bool(h1.get("content-range")), str(h1.get("content-range")))

    # ---------------- M3-6 续传一致性 ----------------
    print("\n[M3-6] 续传一致性：If-Range 与当前 ETag 不符 → 必须回落全量 200")
    c3, _, p3 = call("GET", f"/models/{model_id}/download",
                     headers={"Range": f"bytes={half}-", "If-Range": '"deadbeef-stale-etag"'}, raw=True)
    check("ETag 不符时回落全量 200（不可返回 206 分片）", c3 == 200 and len(p3) == len(payload),
          f"实际 {c3}, {len(p3)} 字节")

    # ---------------- M3-7 下载计数 ----------------
    print("\n[M3-7] 下载计数：只有全量下载才 +1")
    _, _, text = call("GET", f"/models/{model_id}")
    before = json.loads(text).get("download_count")
    check("计数情况可读", isinstance(before, int), f"download_count={before}")
    if isinstance(before, int):
        # before 已含 M3-4 的一次 + M3-6 回落全量的一次
        check("续传未造成重复计数（≤ 2 次全量）", before <= 2, f"download_count={before}")

    # ---------------- M3-8 清理 ----------------
    print("\n[M3-8] 删除：记录与文件都要消失")
    code, _, _ = call("DELETE", f"/models/{model_id}")
    check("删除返回 200", code == 200, f"实际 {code}")
    code, _, _ = call("GET", f"/models/{model_id}")
    check("删除后详情 404", code == 404, f"实际 {code}")

    return _summary()


def _summary() -> int:
    passed = sum(1 for _, ok, _ in _results if ok)
    total = len(_results)
    print("\n" + "=" * 70)
    print(f"结果：{passed}/{total} 通过")
    for name, ok, detail in _results:
        if not ok:
            print(f"  ✗ {name}" + (f" — {detail}" if detail else ""))
    print("=" * 70)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
