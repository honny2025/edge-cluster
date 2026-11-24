#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""部署失败回滚验收（A1）。

**不需要平台在跑，也不需要真机** —— 起一个本机 HTTP 服务当"平台的文件服务"，
把 Agent 的 `_deploy_model` 真跑一遍，然后看设备目录变成了什么样。

为什么要有这个脚本
------------------
回滚最危险的失败不是"回滚没生效"，而是**回滚没生效却报告成功**。
用户看到"已回滚"，就不再去看设备了 —— 而设备其实停在半新半旧的状态。
所以下面有两组断言是专门钉这个的：

  * F 段：制造一个"还原不回去"的局面，回滚**必须**返回失败，
    而不是"尽力了打勾就算过"；
  * I 段：设备上原本**没有**这个文件时，回滚必须把它**删掉**，
    而不是当成"还原"留在那里。

另一组同样重要的断言是"不该动的东西一个都没动"：
下载就断了的部署根本碰不到设备文件，回滚不该有任何动作（G 段）。

覆盖：
  A 全新设备   目标文件原本不存在 → 回滚后必须不存在
  B 覆盖还原  目标文件原本存在   → 回滚后内容必须是原来那份（逐字节比对）
  C 成功不回滚 部署成功 → 旧版本作废、日志目录清空
  D 幂等      连滚两次不炸
  E 拿不到后悔药就中止   建不了回滚点 → 部署直接失败，旧文件原封不动
  F 回滚失败要如实报    还原不回去时必须返回 False，且日志目录保留
  G 不该动就不动       落位之前就失败 → 一个字节都没动过
  H 崩溃可恢复  上一轮遗留的回滚清单，下一轮必须先还原掉
  I 附属物也回滚  DLC / config 同样被覆盖过，同样要还原
  J 路径安全    task_id 带 ../ 不能跑到目录外面去
  K 平台侧接线  列 / 迁移 / 序列化三处都在（防止"Agent 报了但平台存不下"）

用法：
    python docs/acceptance/rollback_check.py
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import shutil
import sys
import tempfile
import threading
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "node-agent"))

# 平台侧那几个断言要 import app.*，必须在 import 之前把数据目录隔离掉
TMP_ENV = Path(tempfile.mkdtemp(prefix="edge-rollback-env-"))
os.environ.setdefault("EDGE_DATA__DB_PATH", str(TMP_ENV / "edge.db"))
os.environ.setdefault("EDGE_DATA__MODELS_DIR", str(TMP_ENV / "models"))
os.environ.setdefault("EDGE_DATA__LOGS_DIR", str(TMP_ENV / "logs"))
os.environ.setdefault("EDGE_WEB__SERVE_STATIC", "false")

import agent_rollback  # noqa: E402
from agent_rollback import InstallJournal, RollbackError  # noqa: E402

_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read(path: Path) -> bytes:
    try:
        return path.read_bytes()
    except OSError:
        return b""


# ===========================================================================
# 一个本机 HTTP 服务，扮演"平台的文件服务"
# ===========================================================================
@contextlib.contextmanager
def serve_dir(directory: Path):
    class Quiet(SimpleHTTPRequestHandler):
        def __init__(self, *a, **kw):
            super().__init__(*a, directory=str(directory), **kw)

        def log_message(self, *a):  # 不要污染验收输出
            pass

    srv = ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        yield f"http://127.0.0.1:{srv.server_address[1]}"
    finally:
        srv.shutdown()


class StubWS:
    """只做一件事：把 Agent 发出去的消息收下来。"""

    def __init__(self) -> None:
        self.sent: list[dict] = []

    async def send(self, raw: str) -> None:
        try:
            self.sent.append(json.loads(raw))
        except ValueError:
            pass

    def ack(self) -> dict:
        acks = [m for m in self.sent if m.get("type") == "ack"]
        return acks[-1] if acks else {}


def make_agent(models_dir: Path):
    """不跑 `__init__` 的 Agent 桩。

    只补齐 `_deploy_model` 真正会用到的那几个属性 —— 走 `__init__` 会去读
    设备的 getprop、写 state/ 目录，那是真机身份文件，验收脚本不该碰。
    """
    from agent import EdgeAgent

    ag = EdgeAgent.__new__(EdgeAgent)
    ag.cfg = SimpleNamespace(models_path=str(models_dir))
    ag.node_id = "rollback-test"
    ag._cancelled_tasks = set()
    return ag


def deploy(models_dir: Path, payload: dict) -> dict:
    ag = make_agent(models_dir)
    ws = StubWS()
    asyncio.run(ag._deploy_model(ws, "cmd-1", payload))
    return ws.ack()


def payload_for(base_url: str, name: str, data: bytes, *, manifest=None, self_test=None) -> dict:
    return {
        "url": f"{base_url}/{name}",
        "target": name,
        "sha256": sha256_bytes(data),
        "size": len(data),
        "task_id": "task-rollback",
        "model_id": "m-1",
        "download_token": "",
        "manifest": manifest or [],
        "self_test": self_test,
    }


def failing_self_test() -> dict:
    return {
        "command": f'"{sys.executable}" -c "import sys;sys.exit(1)"',
        "timeout": 30,
    }


def passing_self_test() -> dict:
    return {
        "command": f'"{sys.executable}" -c "import sys;sys.exit(0)"',
        "timeout": 30,
    }


# ===========================================================================
def unit_section() -> None:
    base = Path(tempfile.mkdtemp(prefix="edge-journal-"))

    # ---------------------------------------------------------- A / B / C
    print("\nA/B/C 回滚日志本身：新建的删掉、覆盖的还原、成功的不回来")

    work = base / "a"
    work.mkdir()
    old = work / "model.bin"
    old.write_bytes(b"OLD-VERSION")

    j = InstallJournal(work / ".rollback", "task-a")
    j.prepare()
    j.capture(old)                       # 旧文件被挪走
    check("B capture 之后旧文件已不在原位（说明它真的被保护起来了）", not old.exists())
    old.write_bytes(b"NEW-VERSION")      # 新版本落位

    ok, note = j.rollback()
    check("B 回滚报告成功", ok, note)
    check("B 旧文件回到原位，且**内容是原来那一份**",
          read(old) == b"OLD-VERSION", repr(read(old)))
    check("B 回滚说明写清了做了什么", "还原" in note, note)
    check("B 回滚完成后日志目录已清空", not (work / ".rollback" / "task-a").exists())

    fresh = base / "a2"
    fresh.mkdir()
    j2 = InstallJournal(fresh / ".rollback", "task-a2")
    j2.prepare()
    newfile = fresh / "brand-new.bin"
    j2.capture(newfile)                  # 设备上原本没有它
    newfile.write_bytes(b"NEW")
    ok, note = j2.rollback()
    check("A 原本不存在的文件，回滚后必须**不存在**", not newfile.exists())
    check("A 回滚报告成功", ok, note)

    okd = base / "a3"
    okd.mkdir()
    dst = okd / "model.bin"
    dst.write_bytes(b"OLD")
    j3 = InstallJournal(okd / ".rollback", "task-a3")
    j3.prepare()
    j3.capture(dst)
    dst.write_bytes(b"NEW")
    j3.commit()
    check("C commit 之后保持新版内容不变", read(dst) == b"NEW", repr(read(dst)))
    check("C commit 之后悔药已丢弃（旧版本不会再冒出来）",
          not (okd / ".rollback" / "task-a3").exists()
          and not list(okd.glob("**/OLD*")))

    # ---------------------------------------------------------- D
    print("\nD 幂等：连滚两次不炸")
    j4 = InstallJournal(base / "d" / ".rollback", "task-d")
    j4.prepare()
    ok1, _ = j4.rollback()
    ok2, note2 = j4.rollback()
    check("第一次回滚成功", ok1)
    check("第二次回滚不抛异常，且仍报成功", ok2, note2)

    # ---------------------------------------------------------- F
    print("\nF 回滚失败必须如实报 —— 这是本模块最重要的一条")
    fdir = base / "f"
    fdir.mkdir()
    f = fdir / "model.bin"
    f.write_bytes(b"OLD")
    j5 = InstallJournal(fdir / ".rollback", "task-f")
    j5.prepare()
    j5.capture(f)
    f.write_bytes(b"NEW")
    # 制造"还原不回去"：把救命的备份文件删掉（磁盘坏了 / 被人清了的等价情形）
    for saved in (fdir / ".rollback" / "task-f").glob("*-model.bin"):
        saved.unlink()
    ok, note = j5.rollback()
    check("备份丢了 → 回滚必须返回 False，不能打勾", ok is False, note)
    check("失败说明里点出是哪个文件没回去", "model.bin" in note, note)
    check("回滚失败时**保留**日志目录，不把最后的机会也删掉",
          (fdir / ".rollback" / "task-f").exists())

    # ---------------------------------------------------------- H
    print("\nH 崩溃可恢复：Agent 被系统杀掉后留下的清单")
    hdir = base / "h"
    hdir.mkdir()
    h = hdir / "model.bin"
    h.write_bytes(b"OLD")
    j6 = InstallJournal(hdir / ".rollback", "task-h")
    j6.prepare()
    j6.capture(h)
    h.write_bytes(b"NEW")
    # 这里**故意不 rollback 也不 commit** —— 等价于进程在中间被杀掉，
    # 只剩磁盘上的 journal.json
    check("遗留现场的清单文件确实落盘了",
          (hdir / ".rollback" / "task-h" / "journal.json").is_file())

    j7 = InstallJournal(hdir / ".rollback", "task-h")
    note = j7.prepare()
    check("新一轮 prepare 先把上一轮遗留的还原了", "还原" in note, note)
    check("H 被挪走的旧文件回到了原位，内容是旧的",
          read(h) == b"OLD", repr(read(h)))
    check("H 还原完才开新一轮：日志目录是干净的，没有残留的旧备份",
          not any((hdir / ".rollback" / "task-h").glob("*-model.bin")))

    # ---------------------------------------------------------- J
    print("\nJ 路径安全：目录名不允许跑出 base")
    outside = base / "outside.bin"
    outside.write_bytes(b"SHOULD-NOT-BE-TOUCHED")
    j8 = InstallJournal(base / "j" / ".rollback", "../../outside")
    check("带 ../ 的 task_id 没能逃出 base 目录",
          (base / "j" / ".rollback") in j8.dir.parents or j8.dir.parent == base / "j" / ".rollback",
          str(j8.dir))
    j8.prepare()
    j8.rollback()
    check("J 目录外面的文件毫发无损", read(outside) == b"SHOULD-NOT-BE-TOUCHED")

    # ---------------------------------------------------------- E(单元)
    print("\nE 拿不到后悔药就不动手")
    edir = base / "e"
    edir.mkdir()
    # 把回滚目录位置先占成一个**普通文件** —— mkdir 必然失败
    (edir / ".rollback").write_text("occupied", encoding="utf-8")
    je = InstallJournal(edir / ".rollback", "task-e")
    try:
        je.prepare()
        check("建不了回滚目录时 prepare 必须抛 RollbackError", False, "居然成功了")
    except RollbackError as exc:
        check("建不了回滚目录时 prepare 抛 RollbackError", True, str(exc)[:60])

    shutil.rmtree(base, ignore_errors=True)


# ===========================================================================
def end_to_end_section() -> None:
    """**真跑一遍 `_deploy_model`** —— 上面那些断言证明日志模块是对的，
    这一段证明它真的被接进了部署流程，而不是躺在那儿没人调用。"""
    print("\nE2E 真部署路径：让 Self Test 失败，看设备最后是什么样")

    work = Path(tempfile.mkdtemp(prefix="edge-rollback-e2e-"))
    models = work / "models"
    models.mkdir()
    served = work / "served"
    served.mkdir()

    OLD = b"OLD-MODEL-BYTES-" + b"x" * 4096
    NEW = b"NEW-MODEL-BYTES-" + b"y" * 4096
    target = models / "demo.bin"
    target.write_bytes(OLD)
    (served / "demo.bin").write_bytes(NEW)

    with serve_dir(served) as base_url:
        # ---------------------------------------------------------- 失败 → 回滚
        ack = deploy(models, payload_for(base_url, "demo.bin", NEW,
                                         self_test=failing_self_test()))
        check("E2E Self Test 失败 → ack.ok=False、status=failed",
              ack.get("ok") is False and ack.get("status") == "failed",
              f"{ack.get('ok')} {ack.get('status')}")
        check("E2E ack 里带上了回滚结论",
              ack.get("rolled_back") is True and bool(ack.get("rollback_note")),
              str(ack.get("rollback_note")))
        check("E2E **设备上的旧模型被还原了**（逐字节比对）",
              read(target) == OLD, f"{len(read(target))} 字节, 期待 {len(OLD)}")
        check("E2E 回滚后日志目录没留下垃圾",
              not list(models.glob(".rollback/*/*-demo.bin")))

        # ---------------------------------------------------------- 成功 → 不回滚
        ack = deploy(models, payload_for(base_url, "demo.bin", NEW,
                                         self_test=passing_self_test()))
        check("E2E Self Test 通过 → ready，且没有回滚动作",
              ack.get("ok") is True and ack.get("status") == "ready"
              and ack.get("rolled_back") is False and not ack.get("rollback_note"),
              f"{ack.get('status')} {ack.get('rollback_note')!r}")
        check("E2E 成功部署后设备上是**新**模型", read(target) == NEW)
        check("E2E 成功后回滚目录被整个清掉",
              not any(models.glob(".rollback/*")))

        # ---------------------------------------------------------- G 落位前失败
        (models / "demo.bin").write_bytes(OLD)
        bad = payload_for(base_url, "demo.bin", NEW, self_test=failing_self_test())
        bad["sha256"] = "0" * 64          # 校验必然失败，走不到落位
        ack = deploy(models, bad)
        check("G sha 校验失败 → 部署失败",
              ack.get("ok") is False and ack.get("status") == "failed",
              str(ack.get("error"))[:50])
        check("G 回滚如实说'无需还原'，而不是假装做了事",
              "无需还原" in str(ack.get("rollback_note")),
              str(ack.get("rollback_note")))
        check("G **落位之前就失败了，旧文件一个字节都没被动过**",
              read(target) == OLD)
        check("G 没有留下 .part 垃圾", not (models / "demo.bin.part").exists())

        # ---------------------------------------------------------- I 附属物
        print("\nI 附属物（DLC / config）同样会被覆盖，同样要还原")
        ctx_old = b"OLD-CONTEXT-CONFIG"
        NEW_CTX = b"NEW-CONTEXT-CONFIG"
        (models / "ctx.json").write_bytes(ctx_old)
        (served / "ctx.json").write_bytes(NEW_CTX)

        manifest = [{
            "name": "ctx.json",
            "kind": "config",
            "size": len(NEW_CTX),
            "sha256": sha256_bytes(NEW_CTX),
            "url": f"{base_url}/ctx.json",
        }]
        ack = deploy(models, payload_for(base_url, "demo.bin", NEW,
                                         manifest=manifest,
                                         self_test=failing_self_test()))
        check("I 模型本体失败了，但附属物**先一步已经落位过**",
              read(target) == OLD,
              f"model={read(target)[:16]!r}")
        check("I **附属物也被还原成了旧的那一份**",
              read(models / "ctx.json") == ctx_old, repr(read(models / "ctx.json")))
        check("I 回滚说明里数得出还原了几个文件",
              "还原 2 个" in str(ack.get("rollback_note")),
              str(ack.get("rollback_note")))

        # ---------------------------------------------------------- E 端到端
        print("\nE 建不了回滚点 → 部署直接中止，设备原封不动")
        # 把回滚目录的位置占成一个**普通文件** —— 目录建不出来，
        # 等价于"这台设备上没有地方放后悔药"
        rb = models / ".rollback"
        if rb.is_dir():
            shutil.rmtree(rb, ignore_errors=True)
        elif rb.exists():
            rb.unlink()
        rb.write_text("occupied", encoding="utf-8")
        (models / "demo.bin").write_bytes(OLD)
        ack = deploy(models, payload_for(base_url, "demo.bin", NEW,
                                         self_test=passing_self_test()))
        check("E 部署失败，且原因说的是回滚点建不起来",
              ack.get("ok") is False and "回滚点" in str(ack.get("error")),
              str(ack.get("error"))[:70])
        check("E **设备上的旧模型完全没被动过**（宁可部署失败，也不冒险覆盖）",
              read(target) == OLD)
        check("E 连 .part 都没有残留", not (models / "demo.bin.part").exists())

    shutil.rmtree(work, ignore_errors=True)


# ===========================================================================
def platform_section() -> None:
    """Agent 报了回滚、平台却存不下，是这类功能最常见的断点。"""
    print("\nK 平台侧接线：列 / 老库补列 / 接口序列化三处都要在")

    try:
        sys.path.insert(0, str(ROOT / "server"))
        from app.database import _MIGRATIONS
        from app.models.deployment import DeployTaskNode
    except Exception as exc:  # pragma: no cover
        check("平台侧模块可导入", False, str(exc))
        return

    check("ORM 里有 rollback_note 列",
          "rollback_note" in {c.name for c in DeployTaskNode.__table__.columns})
    check("老库补列清单里有它（否则升级上来的库会缺列，回执直接写不进去）",
          any(col == "rollback_note" for col, _t, _d in _MIGRATIONS.get("deploy_task_nodes", [])))

    try:
        from app.api.routes_deployments import _node

        fake = SimpleNamespace(
            node_id="n1", node_name="手机一", status="failed", progress=99,
            error="Self Test 未通过", retry_count=1, bytes_downloaded=10, bytes_total=20,
            rollback_note="还原 2 个被覆盖的文件",
            started_at=None, finished_at=None, updated_at=None,
        )
        got = _node(fake)
        check("接口把 rollback_note 序列化出去了",
              got.get("rollback_note") == "还原 2 个被覆盖的文件", str(got.get("rollback_note")))
        check("error 与 rollback_note 是分开的两个字段，互不覆盖",
              got.get("error") == "Self Test 未通过" and got.get("rollback_note"),
              f"error={got.get('error')!r}")
    except Exception as exc:
        check("接口序列化可用", False, str(exc))


# ===========================================================================
def main() -> int:
    print("=" * 70)
    print("部署失败回滚验收（A1）")
    print("=" * 70)

    unit_section()
    end_to_end_section()
    platform_section()

    passed = sum(1 for _n, ok, _d in _results if ok)
    total = len(_results)
    print("\n" + "=" * 70)
    print(f"结果：{passed}/{total} 通过")
    for name, ok, detail in _results:
        if not ok:
            print(f"  ✗ {name}" + (f" — {detail}" if detail else ""))
    print("=" * 70)
    return 0 if passed == total else 1


if __name__ == "__main__":
    try:
        code = main()
    finally:
        shutil.rmtree(TMP_ENV, ignore_errors=True)
    sys.exit(code)
