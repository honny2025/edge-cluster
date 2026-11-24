#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""备份与恢复验收（A4）。

**不需要平台在跑，也不需要真机** —— 脚本自己在临时目录建一个 SQLite 库，
塞进构造好的数据，调服务，然后断言。跑完删干净。

为什么要有这个脚本
------------------
备份最危险的失败不是"打不出包"，而是**打出来的包看起来是好的、里面却少一截**：

  * 数据库开了 WAL，最近的提交还在 `edge.db-wal` 里，没合并进主文件。
    此时直接复制 `edge.db`，会得到一个"少了最后一截、还不报错"的库。
  * 恢复时如果 `edge.db-wal` / `-shm` 还躺在旁边，SQLite 会拿**旧 WAL**
    往**新库**上重放 —— 刚恢复的数据被悄悄改回去。

这两种错误都不会抛异常，只会在"换机器之后发现少了几天的记录"时暴露出来。
所以下面有两组专门钉死它们的断言（A 段与 G 段），并且 G 段是**真的换一套数据**
再恢复，用结果本身来证明旧 WAL 没有回流。

覆盖：
  A 快照自包含   备份里的库单独拿出来就能开，不依赖 -wal
  B manifest     版本 / 时间 / sha256 / 表行数 / 模型清单齐全且属实
  C 校验         坏包必须被识别，不能"看起来 ok"
  D dry-run      confirm 不为真时一个字节都不动
  E 恢复         数据真的被替换成备份里的那份
  F 安全备份     恢复前自动留下的 .bak 必须是能被读的，不是摆设
  G WAL 不回流   恢复后不得残留旧 WAL 把数据带回来
  H 恢复后可用   恢复完 init_db 能正常跑，库能正常查
  I 模型开关     include_models 控制权重是否入包，清单里 file_present 如实标注

用法：
    python docs/acceptance/backup_check.py
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok


# ---------------------------------------------------------------------------
# 环境隔离：必须在 import app.config 之前设置 —— settings 是模块级单例
# ---------------------------------------------------------------------------
TMP = Path(tempfile.mkdtemp(prefix="edge-backup-"))
os.environ["EDGE_DATA__DB_PATH"] = str(TMP / "edge.db")
os.environ["EDGE_DATA__MODELS_DIR"] = str(TMP / "models")
os.environ["EDGE_DATA__LOGS_DIR"] = str(TMP / "logs")
os.environ["EDGE_WEB__SERVE_STATIC"] = "false"

sys.path.insert(0, str(ROOT / "server"))

from app.config import settings  # noqa: E402
from app.database import SessionLocal, init_db  # noqa: E402
from app.models import Model, Node  # noqa: E402
from app.services import backup as backup_svc  # noqa: E402


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def _node_ids(db) -> list[str]:
    return sorted(r[0] for r in db.query(Node.node_id).all())


def _make_zip(path: Path, members: dict[str, bytes]) -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        for name, data in members.items():
            zf.writestr(name, data)
    return path


def _manifest_bytes(**over) -> bytes:
    m = {
        "kind": backup_svc.BACKUP_KIND,
        "format": backup_svc.BACKUP_FORMAT,
        "app_version": "test",
        "created_at": "2026-01-01T00:00:00",
        "db": {"file": "edge.db", "size": 0, "sha256": ""},
        "tables": {},
        "models": [],
        "includes_models": False,
    }
    m.update(over)
    return json.dumps(m).encode("utf-8")


def main() -> int:
    print("=" * 70)
    print("备份与恢复验收")
    print(f"临时库：{settings.db_file}")
    print("=" * 70)

    init_db()
    db_file = settings.db_file

    # ---------- 造第一批数据：old-01 / old-02 ----------
    with SessionLocal() as db:
        db.add(Node(node_id="old-01", name="旧节点一", status="online"))
        db.add(Node(node_id="old-02", name="旧节点二", status="offline"))
        db.add(Model(model_id="m-old", name="旧模型", version="v1",
                     file_name="old.bin", file_size=11, sha256="a" * 64,
                     storage_path="m-old-old.bin"))
        db.commit()

    # 造一个"模型权重文件"，用来验 file_present 与 include_models
    models_dir = settings.models_path
    models_dir.mkdir(parents=True, exist_ok=True)
    (models_dir / "m-old-old.bin").write_bytes(b"old-model!!")

    # ---------------------------------------------------------------
    print("\nA 快照自包含（这是 VACUUM INTO 存在的理由）")
    # ---------------------------------------------------------------
    wal = Path(str(db_file) + "-wal")
    check("写入之后 WAL 文件确实存在（说明确实处在'最近提交没合并进主文件'的状态）",
          wal.exists() and wal.stat().st_size > 0,
          f"{wal.name} = {wal.stat().st_size if wal.exists() else 0} 字节")

    bkp = backup_svc.create_backup()
    check("备份包已生成", bkp.is_file() and bkp.suffix == ".zip", str(bkp.name))

    # 把备份里的 edge.db 单独解到一个**没有 wal/shm 的空目录**，看能不能读全
    lone = TMP / "lone"
    lone.mkdir(exist_ok=True)
    with zipfile.ZipFile(bkp) as zf:
        zf.extract(backup_svc.DB_MEMBER, lone)
    lone_db = lone / backup_svc.DB_MEMBER

    con = sqlite3.connect(f"file:{lone_db.as_posix()}?mode=ro", uri=True)
    try:
        got = sorted(r[0] for r in con.execute("SELECT node_id FROM nodes").fetchall())
    finally:
        con.close()
    check("单独拿出 edge.db（不给它 -wal）也能读到全部数据",
          got == ["old-01", "old-02"], str(got))

    # ---------------------------------------------------------------
    print("\nB manifest 内容属实")
    # ---------------------------------------------------------------
    with zipfile.ZipFile(bkp) as zf:
        man = json.loads(zf.read(backup_svc.MANIFEST_MEMBER).decode("utf-8"))
        names = set(zf.namelist())

    check("manifest 与 edge.db 都在包里",
          backup_svc.MANIFEST_MEMBER in names and backup_svc.DB_MEMBER in names)
    check("kind 标记正确", man.get("kind") == backup_svc.BACKUP_KIND, str(man.get("kind")))
    check("format 版本号存在", man.get("format") == backup_svc.BACKUP_FORMAT)
    check("记了 app_version 与 created_at",
          bool(man.get("app_version")) and bool(man.get("created_at")),
          f"{man.get('app_version')} @ {man.get('created_at')}")
    check("db.sha256 与包内 edge.db 实际值一致",
          (man.get("db") or {}).get("sha256") == _sha256(lone_db))
    check("tables 行数与真实库一致",
          man.get("tables", {}).get("nodes") == 2
          and man.get("tables", {}).get("models") == 1,
          str(man.get("tables")))

    m0 = (man.get("models") or [{}])[0]
    check("模型清单含 sha256（恢复后能逐个核对文件）",
          m0.get("sha256") == "a" * 64, str(m0.get("sha256")))
    check("模型清单如实标注权重文件在原机上还在（file_present=true）",
          m0.get("file_present") is True, str(m0.get("file_present")))
    check("默认不含模型权重，并如实写进 manifest",
          man.get("includes_models") is False
          and not any(n.startswith("models/") for n in names),
          f"includes_models={man.get('includes_models')}")

    # ---------------------------------------------------------------
    print("\nC 校验能识别坏包（不能'看起来 ok'）")
    # ---------------------------------------------------------------
    bad_dir = TMP / "bad"
    bad_dir.mkdir(exist_ok=True)

    not_zip = bad_dir / "not-a-zip.zip"
    not_zip.write_bytes(b"this is definitely not a zip file")
    r = backup_svc.inspect_backup(not_zip)
    check("非 zip 文件 → ok=false", not r["ok"] and r["problems"], str(r["problems"][:1]))

    no_manifest = _make_zip(bad_dir / "no-manifest.zip",
                            {backup_svc.DB_MEMBER: lone_db.read_bytes()})
    r = backup_svc.inspect_backup(no_manifest)
    check("缺 manifest.json → 报'不是本平台的备份包'",
          not r["ok"] and any("manifest" in p for p in r["problems"]), str(r["problems"][:1]))

    wrong_kind = _make_zip(bad_dir / "wrong-kind.zip", {
        backup_svc.MANIFEST_MEMBER: _manifest_bytes(kind="someone-elses-backup"),
        backup_svc.DB_MEMBER: lone_db.read_bytes(),
    })
    r = backup_svc.inspect_backup(wrong_kind)
    check("kind 对不上 → 拒绝", not r["ok"] and any("kind" in p for p in r["problems"]),
          str(r["problems"][:1]))

    future_fmt = _make_zip(bad_dir / "future.zip", {
        backup_svc.MANIFEST_MEMBER: _manifest_bytes(format=99),
        backup_svc.DB_MEMBER: lone_db.read_bytes(),
    })
    r = backup_svc.inspect_backup(future_fmt)
    check("format 版本不认 → 明确说出来，而不是硬着头皮恢复",
          not r["ok"] and any("格式" in p for p in r["problems"]), str(r["problems"][:1]))

    no_db = _make_zip(bad_dir / "no-db.zip",
                      {backup_svc.MANIFEST_MEMBER: _manifest_bytes()})
    r = backup_svc.inspect_backup(no_db)
    check("包里没有 edge.db → 拒绝", not r["ok"] and any("edge.db" in p for p in r["problems"]),
          str(r["problems"][:1]))

    not_sqlite = _make_zip(bad_dir / "not-sqlite.zip", {
        backup_svc.MANIFEST_MEMBER: _manifest_bytes(),
        backup_svc.DB_MEMBER: b"PK\x03\x04 not a database at all" * 100,
    })
    r = backup_svc.inspect_backup(not_sqlite)
    check("edge.db 不是 SQLite（魔数不对）→ 拒绝",
          not r["ok"] and any("SQLite" in p or "文件头" in p for p in r["problems"]),
          str(r["problems"][:1]))

    # 篡改一个字节 → sha256 对不上
    tampered_db = bytearray(lone_db.read_bytes())
    tampered_db[-1] ^= 0xFF
    tampered = _make_zip(bad_dir / "tampered.zip", {
        backup_svc.MANIFEST_MEMBER: man and json.dumps(man).encode("utf-8"),
        backup_svc.DB_MEMBER: bytes(tampered_db),
    })
    r = backup_svc.inspect_backup(tampered)
    check("库被改动一个字节 → sha256 校验抓得住",
          not r["ok"] and r["sha256_matches"] is False
          and any("sha256" in p for p in r["problems"]),
          str(r["problems"][:1]))

    r = backup_svc.inspect_backup(bkp)
    check("反过来：我们自己打的包必须校验通过", r["ok"] and not r["problems"],
          str(r["problems"]))
    check("校验结果里带上本平台版本，方便判断兼容性",
          r["current_app_version"] == backup_svc._app_version(),
          str(r["current_app_version"]))

    # ---------------------------------------------------------------
    print("\nD dry-run：不带 confirm 时一个字节都不动")
    # ---------------------------------------------------------------
    before_hash = _sha256(db_file)
    dry = backup_svc.restore_backup(bkp, confirm=False)
    check("confirm=false → dry_run=true、ok=false",
          dry["dry_run"] is True and dry["ok"] is False, str(dry.get("message"))[:40])
    check("库文件字节完全没变", _sha256(db_file) == before_hash)
    check("dry-run 也顺带把校验结果带回来（省一次上传）",
          isinstance(dry.get("preview"), dict) and dry["preview"].get("ok") is True)
    check("dr-yun 消息里说清后果（会替换全部数据）",
          "替换" in (dry.get("message") or ""), (dry.get("message") or "")[:30])

    # ---------------------------------------------------------------
    print("\nE/F/G 真恢复：换一套数据，再用备份换回来")
    # ---------------------------------------------------------------
    with SessionLocal() as db:
        db.query(Model).delete()
        db.query(Node).delete()
        db.add(Node(node_id="new-99", name="恢复后不该存在的节点", status="online"))
        db.commit()
    check("已把数据换成另一套（埋好诱饵）", _node_id_list(db_file) == ["new-99"],
          str(_node_id_list(db_file)))

    res = backup_svc.restore_backup(bkp, confirm=True)
    check("恢复返回 ok=true", res.get("ok") is True, str(res.get("message"))[:40])
    check("恢复完提示要重启平台（后台任务还攥着旧连接）",
          "重启" in (res.get("message") or ""), (res.get("message") or "")[:40])

    ids = _node_id_list(db_file)
    check("E 数据真的换回了备份里的那份", ids == ["old-01", "old-02"], str(ids))
    check("E 恢复后的那套数据里没有 '新数据' 的残留", "new-99" not in ids, str(ids))

    # G 段的核心：如果旧 WAL 没被删，上面那套 new-99 会被重放回来
    check("G 恢复后旧 WAL 没有把数据带回来（这一步不删 -wal 必挂）",
          "new-99" not in ids, f"nodes={ids}")

    safety = res.get("safety_backup") or ""
    check("F 恢复前自动留了一份安全备份", bool(safety), safety)
    if safety:
        safety_path = db_file.with_name(safety)
        check("F 这份安全备份文件确实存在", safety_path.is_file(), str(safety_path.name))
        try:
            c = sqlite3.connect(f"file:{safety_path.as_posix()}?mode=ro", uri=True)
            try:
                cnt = c.execute("SELECT COUNT(*) FROM nodes").fetchone()[0]
            finally:
                c.close()
            check("F 而且它是**能读**的 —— 里面正是被替换掉的那份 new-99", cnt == 1, f"{cnt} 行")
        except sqlite3.Error as exc:
            check("F 而且它是能读的", False, str(exc))

    # ---------------------------------------------------------------
    print("\nH 恢复完还能正常用")
    # ---------------------------------------------------------------
    init_db()
    with SessionLocal() as db:
        n = db.query(Node).count()
        got = _node_ids(db)
    check("init_db 在恢复后的库上跑得通，数据仍是备份里的那份",
          n == 2 and got == ["old-01", "old-02"], f"{n} 个节点 {got}")

    again = backup_svc.create_backup()
    r2 = backup_svc.inspect_backup(again)
    check("恢复完还能再打一份备份（库没被弄坏）", r2["ok"], str(r2["problems"]))

    # ---------------------------------------------------------------
    print("\nI include_models 开关")
    # ---------------------------------------------------------------
    with_models = backup_svc.create_backup(include_models=True)
    with zipfile.ZipFile(with_models) as zf:
        mnames = zf.namelist()
        mman = json.loads(zf.read(backup_svc.MANIFEST_MEMBER).decode("utf-8"))
    packed = [n for n in mnames if n.startswith("models/") and not n.endswith("/")]
    check("include_models=true 时权重进了包", len(packed) >= 1, str(packed))
    check("manifest 里 includes_models 如实写 true",
          mman.get("includes_models") is True)
    r3 = backup_svc.inspect_backup(with_models)
    check("带模型的包也能通过校验", r3["ok"], str(r3["problems"]))
    check("校验结果里数出包内有几个模型文件",
          r3["model_files_in_package"] == len(packed),
          f"{r3['model_files_in_package']} vs {len(packed)}")

    # ---------------------------------------------------------------
    print("\nJ 现状汇报")
    # ---------------------------------------------------------------
    st = backup_svc.backup_status()
    check("backup_status 带库大小、表行数、备份目录清单",
          st["db_size"] > 0 and st["tables"].get("nodes") == 2 and st["backup_count"] >= 1,
          f"db={st['db_size']}B backups={st['backup_count']} nodes={st['tables'].get('nodes')}")
    check("backup_status 如实报告有没有 WAL 未合并",
          isinstance(st["wal_present"], bool), str(st["wal_present"]))

    passed = sum(1 for _, ok, _ in _results if ok)
    total = len(_results)
    print("\n" + "=" * 70)
    print(f"结果：{passed}/{total} 通过")
    for name, ok, detail in _results:
        if not ok:
            print(f"  ✗ {name}" + (f" — {detail}" if detail else ""))
    print("=" * 70)
    return 0 if passed == total else 1


def _node_id_list(db_file: Path) -> list[str]:
    """绕开 ORM 直接读文件 —— 恢复动作关了连接池，用裸 sqlite3 看结果更实在。"""
    con = sqlite3.connect(f"file:{db_file.as_posix()}?mode=ro", uri=True)
    try:
        return sorted(r[0] for r in con.execute("SELECT node_id FROM nodes").fetchall())
    finally:
        con.close()


if __name__ == "__main__":
    try:
        code = main()
    finally:
        shutil.rmtree(TMP, ignore_errors=True)
    sys.exit(code)
