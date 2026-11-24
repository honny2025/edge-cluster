"""模型库 + 部署状态机验收（需求 §5 模型管理 / §6 部署流程）。

覆盖的是**真机路径**：用真实在线的 chk-01 节点跑完整下发，
而不是拿假节点糊过去 —— 假节点不会执行 Self Test，也不会拉附属物，
那两件事恰恰是这一阶段最容易出错的地方。

跑法：
    python docs/acceptance/m6_model_deploy_check.py

前置：平台已启动（:8000），chk-01 已在线。
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("EDGE_BASE", "http://127.0.0.1:8000") + "/api/v1"
TOKEN = os.environ.get("EDGE_TOKEN", "edge-cluster-dev-token")
NODE = os.environ.get("EDGE_NODE", "chk-01")
# Agent 上的模型目录：附属物落位后要在这里找到（相对仓库根）
MODELS_DIR = os.environ.get(
    "EDGE_MODELS_DIR",
    os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
                 "node-agent", "models"),
)

FAIL: list[str] = []
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def call(method: str, path: str, body=None, raw: bytes | None = None,
         ctype: str = "application/json", timeout: int = 60):
    url = BASE + path
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("X-Auth-Token", TOKEN)
    if data is not None:
        req.add_header("Content-Type", ctype)
    try:
        with _opener.open(req, timeout=timeout) as r:
            blob = r.read()
            try:
                return r.status, json.loads(blob)
            except Exception:
                return r.status, blob
    except urllib.error.HTTPError as e:
        blob = e.read()
        try:
            return e.code, json.loads(blob)
        except Exception:
            return e.code, blob.decode("utf-8", "replace")


def check(name: str, ok: bool, extra: str = "") -> None:
    print(("  PASS  " if ok else "  FAIL  ") + name + (f"   {extra}" if extra else ""))
    if not ok:
        FAIL.append(name)


def multipart(fields: list[tuple[str, str]], files: list[tuple[str, str, bytes]]):
    """手搓 multipart —— urllib 不自带，标准库里也没有。"""
    boundary = "----edgem6probe"
    out = b""
    for k, v in fields:
        out += (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'
        ).encode()
    for field, filename, content in files:
        out += (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="{field}"; filename="{filename}"\r\n'
            "Content-Type: application/octet-stream\r\n\r\n"
        ).encode() + content + b"\r\n"
    out += f"--{boundary}--\r\n".encode()
    return out, f"multipart/form-data; boundary={boundary}"


def upload_model(name: str, blob: bytes, **meta) -> tuple[int, dict | str]:
    fields = [("name", name), ("version", "v1")]
    fields += [(k, str(v)) for k, v in meta.items()]
    body, ctype = multipart(fields, [("file", "probe.bin", blob)])
    return call("POST", "/models", raw=body, ctype=ctype)


def _raw_status(path: str, headers: dict | None = None) -> int:
    """只要状态码的请求。用于验证"被拒"的场景 —— 这类响应体是错误详情，
    解析它没有意义，我们要的就是那个 401。"""
    req = urllib.request.Request(BASE + path)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with _opener.open(req, timeout=20) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def wait_node_status(task_id: str, node_id: str, want: set[str], timeout: float = 60.0):
    """轮询直到该节点进入目标终态。返回 (最终状态, 该节点 dict)。"""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        st, task = call("GET", f"/deployments/{task_id}")
        if st == 200 and isinstance(task, dict):
            for n in task.get("nodes", []):
                if n.get("node_id") == node_id:
                    last = n
                    if n.get("status") in want:
                        return n.get("status"), n
        time.sleep(0.5)
    return (last or {}).get("status"), last or {}


def main() -> int:
    stamp = int(time.time()) % 1000000
    created: list[str] = []

    print("=" * 70)
    print("M6 模型库 + 部署状态机验收")
    print("=" * 70)

    # ---------------------------------------------------------- 1. 元数据登记
    print("\n== 1. 上传模型并登记完整元数据（需求 §5 Model Registry）==")
    blob = b"edge-m6-model-" + os.urandom(4096)
    sha = hashlib.sha256(blob).hexdigest()
    st, m = upload_model(
        f"m6-probe-{stamp}", blob,
        quantization="Q4_K_M", target_soc="SM8550", qnn_version="2.20",
        backends='["htp","cpu"]',
        self_test_command="echo selftest-ok", self_test_timeout="30",
    )
    check("上传成功", st in (200, 201), f"status={st}")
    if st >= 400:
        print(f"    响应：{str(m)[:400]}")
        return 1
    mid = m["model_id"]
    created.append(mid)
    check("sha256 与本地一致", m.get("sha256") == sha, m.get("sha256", "")[:16])
    check("quantization 已登记", m.get("quantization") == "Q4_K_M", str(m.get("quantization")))
    check("target_soc 已登记", m.get("target_soc") == "SM8550", str(m.get("target_soc")))
    check("qnn_version 已登记", m.get("qnn_version") == "2.20", str(m.get("qnn_version")))
    check("backends 解析成列表", m.get("backends") == ["htp", "cpu"], str(m.get("backends")))
    check("self_test_command 已登记",
          m.get("self_test_command") == "echo selftest-ok", str(m.get("self_test_command")))

    # 回归保护：曾经指标接口在路由里手写字段列表，库里明明有值、接口恒返回 null。
    st, m2 = call("GET", f"/models/{mid}")
    check("详情接口如实返回元数据（不是 null）",
          st == 200 and m2.get("quantization") == "Q4_K_M"
          and m2.get("target_soc") == "SM8550" and m2.get("backends") == ["htp", "cpu"],
          f"status={st}")

    # ---------------------------------------------------------- 2. PATCH 增量更新
    print("\n== 2. 增量更新登记信息（PATCH 只改给定的字段）==")
    st, mp = call("PATCH", f"/models/{mid}", {"quantization": "Q8_0"})
    check("PATCH 成功", st == 200, f"status={st}")
    check("目标字段已改", mp.get("quantization") == "Q8_0", str(mp.get("quantization")))
    check("未给出的字段没被清空",
          mp.get("target_soc") == "SM8550" and mp.get("qnn_version") == "2.20"
          and mp.get("self_test_command") == "echo selftest-ok",
          f"target_soc={mp.get('target_soc')} qnn={mp.get('qnn_version')}")
    call("PATCH", f"/models/{mid}", {"quantization": "Q4_K_M"})  # 还原

    # JSON 接口的类型校验由 Pydantic 做，返回 422；上传表单里手填 JSON 串
    # 的那种才返回 400（见 `_json_list`）。两者都是**拒绝**，
    # 这里只要求"不静默吞掉"—— 具体是 400 还是 422 不该写死。
    st, bad = call("PATCH", f"/models/{mid}", {"backends": "not-json"})
    check("非法 JSON 字段被拒绝（不静默吞掉）", st in (400, 422), f"status={st}")

    # ---------------------------------------------------------- 3. 附属物 Manifest
    print("\n== 3. Deployment Package 附属物（需求 §6）==")
    asset_blob = json.dumps({"n_ctx": 2048, "probe": stamp}).encode()
    asset_sha = hashlib.sha256(asset_blob).hexdigest()
    body, ctype = multipart(
        [("kind", "config")],
        [("files", f"m6-config-{stamp}.json", asset_blob)],
    )
    st, up = call("POST", f"/models/{mid}/manifest", raw=body, ctype=ctype)
    check("附属物上传成功", st == 200, f"status={st}")
    asset_name = f"m6-config-{stamp}.json"
    man = (up or {}).get("manifest", []) if isinstance(up, dict) else []
    entry = next((x for x in man if x.get("name") == asset_name), None)
    check("manifest 里有该条目", entry is not None, str(man)[:120])
    check("附属物 sha256 正确", bool(entry) and entry.get("sha256") == asset_sha)
    check("附属物带类型 kind=config", bool(entry) and entry.get("kind") == "config")

    # 附属物本身是 JSON，`call()` 会把它解析成 dict —— 所以比对解析结果，
    # 不能拿 bytes 比（那样比出来的是"测试脚本自己没算对"，不是产品的问题）。
    st, got_asset = call("GET", f"/models/{mid}/manifest/{asset_name}")
    check("附属物可下载且内容一致",
          st == 200 and got_asset == json.loads(asset_blob), f"status={st}")

    st2 = _raw_status(f"/models/{mid}/download", headers={})
    check("无令牌下载模型被拒（401）", st2 == 401, f"status={st2}")
    st3 = _raw_status(f"/models/{mid}/download", headers={"X-Download-Token": "forged-token"})
    check("伪造下载令牌被拒（401）", st3 == 401, f"status={st3}")

    # ---------------------------------------------------------- 4. 兼容性预检
    print("\n== 4. 兼容性预检（需求 §5）==")
    st, pre = call("POST", "/deployments/precheck", {"model_id": mid, "node_ids": [NODE]})
    ok_row = (pre or {}).get("results", [{}])[0] if isinstance(pre, dict) else {}
    check("匹配的 SoC 判定通过", st == 200 and ok_row.get("ok") is True,
          f"Soc(设备)={ok_row.get('checks', [{}])[0].get('got') if ok_row.get('checks') else ''}")

    # 换成一颗设备没有的 SoC
    call("PATCH", f"/models/{mid}", {"target_soc": "SM8650"})
    st, pre2 = call("POST", "/deployments/precheck", {"model_id": mid, "node_ids": [NODE]})
    bad_row = (pre2 or {}).get("results", [{}])[0] if isinstance(pre2, dict) else {}
    check("不匹配的 SoC 判定不通过", bad_row.get("ok") is False, str(bad_row.get("reason"))[:80])
    soc_check = next((c for c in bad_row.get("checks", []) if c.get("name") == "soc"), {})
    check("原因是 soc 维度", soc_check.get("verdict") == "fail", str(soc_check)[:100])
    call("PATCH", f"/models/{mid}", {"target_soc": "SM8550"})  # 还原

    # 拿不到 ≠ 不兼容
    st, m3 = upload_model(f"m6-bare-{stamp}", b"bare-" + os.urandom(256))
    bare_id = m3["model_id"] if isinstance(m3, dict) and "model_id" in m3 else ""
    if bare_id:
        created.append(bare_id)
    st, pre3 = call("POST", "/deployments/precheck", {"model_id": bare_id, "node_ids": [NODE]})
    bare_row = (pre3 or {}).get("results", [{}])[0] if isinstance(pre3, dict) else {}
    check("没登记元数据 → 判定 unknown 而不是不兼容", bare_row.get("ok") is True,
          str(bare_row.get("reason"))[:60])
    check("未登记项如实标 unknown",
          all(c.get("verdict") == "unknown" for c in bare_row.get("checks", [])),
          str([c.get("verdict") for c in bare_row.get("checks", [])]))

    # ---------------------------------------------------------- 5. 正常部署
    print("\n== 5. 正常部署：下载 → 校验 → 落位 → 附属物 → Self Test ==")
    st, task = call("POST", "/deployments", {"model_id": mid, "node_ids": [NODE], "note": "m6"})
    check("创建部署任务", st in (200, 201), f"status={st}")
    if st >= 400:
        print(f"    响应：{str(task)[:300]}")
        return 1
    tid = task["task_id"]
    final, nd = wait_node_status(tid, NODE, {"ready", "failed", "cancelled"}, timeout=90)
    check("节点最终 ready", final == "ready", f"status={final} err={nd.get('error', '')[:120]}")
    check("进度到 100", nd.get("progress") == 100, f"progress={nd.get('progress')}")
    check("回传了下载字节数", int(nd.get("bytes_downloaded") or 0) == len(blob),
          f"{nd.get('bytes_downloaded')}/{len(blob)}")
    asset_path = os.path.join(MODELS_DIR, asset_name)
    check("附属物已落到设备模型目录（Agent 真的拉了）", os.path.isfile(asset_path),
          asset_path)
    if os.path.isfile(asset_path):
        got = open(asset_path, "rb").read()
        check("落盘的附属物内容一致", got == asset_blob, f"{len(got)}B")
    st, t2 = call("GET", f"/deployments/{tid}")
    check("任务聚合状态 succeeded", t2.get("status") == "succeeded", str(t2.get("status")))

    # ---------------------------------------------------------- 6. Self Test 拦得住
    print("\n== 6. Self Test 不通过时不许判 ready（需求 §6 的 Testing 环节）==")
    st, m4 = upload_model(
        f"m6-stfail-{stamp}", b"stfail-" + os.urandom(512),
        self_test_command="exit 7", self_test_timeout="10",
    )
    stf_id = m4["model_id"] if isinstance(m4, dict) and "model_id" in m4 else ""
    if stf_id:
        created.append(stf_id)
    st, task2 = call("POST", "/deployments", {"model_id": stf_id, "node_ids": [NODE]})
    tid2 = task2["task_id"]
    final2, nd2 = wait_node_status(tid2, NODE, {"ready", "failed", "cancelled"}, timeout=90)
    check("Self Test 失败 → 节点 failed（不是 ready）", final2 == "failed", f"status={final2}")
    check("失败原因写明是 Self Test", "Self Test" in str(nd2.get("error", "")),
          str(nd2.get("error", ""))[:120])

    # ---------------------------------------------------------- 7. 兼容性拦得住
    print("\n== 7. 兼容性不通过时不下发（需求 §5）==")
    st, m5 = upload_model(f"m6-incompat-{stamp}", b"incompat-" + os.urandom(512),
                          target_soc="SM8650")
    inc_id = m5["model_id"] if isinstance(m5, dict) and "model_id" in m5 else ""
    if inc_id:
        created.append(inc_id)
    st, task3 = call("POST", "/deployments", {"model_id": inc_id, "node_ids": [NODE]})
    tid3 = task3["task_id"]
    final3, nd3 = wait_node_status(tid3, NODE, {"ready", "failed", "cancelled"}, timeout=60)
    check("不兼容 → 节点 failed", final3 == "failed", f"status={final3}")
    check("失败原因写明兼容性", "兼容性检查未通过" in str(nd3.get("error", "")),
          str(nd3.get("error", ""))[:120])

    # ---------------------------------------------------------- 8. 取消
    print("\n== 8. 取消部署（需求 §6 Cancelled 状态）==")
    big = b"cancel-me-" + os.urandom(8 * 1024 * 1024)   # 8MB，留出取消的时间窗
    st, m6 = upload_model(f"m6-cancel-{stamp}", big)
    cid = m6["model_id"] if isinstance(m6, dict) and "model_id" in m6 else ""
    if cid:
        created.append(cid)
    st, task4 = call("POST", "/deployments", {"model_id": cid, "node_ids": [NODE]})
    tid4 = task4["task_id"]
    st, cx = call("POST", f"/deployments/{tid4}/cancel")
    check("取消接口返回 200", st == 200, f"status={st}")
    check("任务状态为 cancelled", isinstance(cx, dict) and cx.get("status") == "cancelled",
          str((cx or {}).get("status")))
    check("逐节点都是 cancelled",
          isinstance(cx, dict) and all(n.get("status") == "cancelled" for n in cx.get("nodes", [])),
          str([n.get("status") for n in (cx or {}).get("nodes", [])]))
    # 关键：晚到的 ready 回执不能把已取消的节点"复活"
    time.sleep(4)
    st, after = call("GET", f"/deployments/{tid4}")
    check("4 秒后仍是 cancelled（晚到的回执没有复活它）",
          after.get("status") == "cancelled",
          str(after.get("status")))
    st, _again = call("POST", f"/deployments/{tid4}/cancel")
    check("已结束的任务再取消 → 409（不假装成功）", st == 409, f"status={st}")

    # ---------------------------------------------------------- 9. 收尾
    print("\n== 9. 清理 ==")
    st, _ = call("DELETE", f"/models/{mid}")
    check("删除模型（含附属物）", st == 200, f"status={st}")
    st, _ = call("GET", f"/models/{mid}")
    check("删除后详情 404", st == 404, f"status={st}")
    for extra_id in created:
        if extra_id != mid:
            call("DELETE", f"/models/{extra_id}")

    print("\n" + "=" * 70)
    if FAIL:
        print(f"失败 {len(FAIL)} 项：")
        for f in FAIL:
            print(f"  - {f}")
        return 1
    print("全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
