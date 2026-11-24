"""部署回滚（A1）—— 部署失败时，把设备还原到"动手之前"的样子。

为什么需要它
------------
`_deploy_model` 的落盘纪律本来就很严：`.part` 临时文件 + 流式 sha256 + 原子改名。
所以**下载失败**从来不会污染设备 —— 那部分没问题。真正漏的是落位**之后**的步骤：

    下载 → 校验 → 落位(temp.replace) → 拉附属物 → Self Test

Self Test 排在最后。它一挂，部署被判 failed —— 可是新模型**已经顶替了旧模型**，
附属物也已经覆盖过。设备上留下的是"一份连自检都过不了的新版本"，
而原来那份能跑的旧版本 **已经没有了**。用户界面上写的是"部署失败"，
设备实际处于"半新半旧且不可用"。A1 要修的就是这一件事。

为什么是 move 不是 copy
-----------------------
模型动辄几个 GB。复制一份等于把手机的存储占用量翻倍，复制本身还要几十秒。
而在**同一个文件系统内**，`os.replace` 只是一次改名：常数时间、不占额外空间。
所以这里把旧文件**挪进**回滚日志目录，而不是拷贝。

三条不能破的纪律
----------------
1. **拿不到后悔药就不动手**。`capture()` 失败一律抛 `RollbackError`，
   由调用方中止这次部署。覆盖一个救不回来的文件，比这次部署失败严重得多。
2. **回滚之后必须验证**。`rollback()` 会逐个确认"该回来的回来了、该没的没了"，
   验证不通过就在返回值里明说。回滚失败却报"已回滚"，是最坏的结果 ——
   用户会以为设备是干净的，不再去看。
3. **只碰自己记下的路径**。不做任何通配删除，绝不 `rmtree(models_dir)`。

崩溃可恢复
----------
Agent 会被 Android 的省电策略杀掉（见 `手机端部署指南.md` 的保活一节）。
如果它正好死在"旧模型已挪走、新模型还没落位"的中间，
只留在内存里的记录会随着进程一起消失 —— 那份旧模型就**永久丢了**。
所以清单每次都落盘成 `journal.json`；下一轮部署开始时先把它还原回来。
"""

from __future__ import annotations

import json
import os
import shutil
from pathlib import Path
from typing import Any

MANIFEST_NAME = "journal.json"


class RollbackError(Exception):
    """留不下后悔药。

    调用方**必须**中止，不能"没有备份也先覆盖上去再说" ——
    这正是 A1 存在的理由，绕过它等于把这个模块删掉。
    """


class InstallJournal:
    """一次部署的"后悔药"。用法固定为四步：

        j = InstallJournal(models_dir / ".rollback", task_id)
        j.prepare()                  # ① 建立回滚点（失败即中止，别往下走）
        ... j.capture(path) ...      # ② 每次**改动文件之前**记一笔
        j.commit()                   # ③ 成功：丢掉后悔药
        # 或
        ok, note = j.rollback()      # ③ 失败：还原，并验证确实还原了

    `capture()` 的语义是"这个路径马上要变了"：
      * 路径**已存在** → 把它挪进日志目录，回滚时挪回去；
      * 路径**不存在** → 只记下来，回滚时删掉（这次部署新建了它）。

    这个区分就是"回滚"与"删库"的区别：全新设备上部署失败，回滚之后
    设备应该回到"没有这个模型"，而不是回到"有个别人的模型"。
    """

    def __init__(self, base_dir: Path, task_id: Any = None) -> None:
        # 目录名要做安全过滤：task_id 是平台给的字符串，直接拼进路径
        # 就等于把目录穿越的口子开在了自己家门口（与文件管理同一条红线）。
        safe = "".join(c for c in str(task_id or "") if c.isalnum() or c in "-_")
        self.dir = Path(base_dir) / (safe or "untagged")
        self._saved: list[tuple[Path, Path]] = []   # (挪到哪, 原本在哪)
        self._created: list[Path] = []              # 这次部署新建的路径
        self._prepared = False
        # 回滚失败过一次就记住结论，重复调用返回同一份 —— 见 rollback()
        self._failed_outcome: tuple[bool, str] | None = None

    # ------------------------------------------------------------------ 建立
    def prepare(self) -> str:
        """建立回滚点。返回一段说明：正常情况下为空串。

        目录里若已有上一轮遗留的清单，**先把那一轮还原掉再开工**。
        原因见模块开头的"崩溃可恢复"：那份清单指向的是被挪走的旧文件，
        不清不楚地覆盖掉，等于把上一轮用户的旧模型永久销毁。
        """
        stale = self._read_manifest()
        note = ""
        if stale:
            ok, detail = self._apply(
                _pairs(stale.get("saved")),
                [Path(str(x)) for x in stale.get("created") or [] if str(x).strip()],
            )
            if not ok:
                raise RollbackError(
                    f"上一轮部署遗留的回滚日志还原失败（{detail}）。"
                    f"这些文件还在 {self.dir}，没有被删掉，请人工确认后再试。"
                )
            note = f"已还原上一轮遗留的回滚点（{detail}）"
            shutil.rmtree(self.dir, ignore_errors=True)

        try:
            self.dir.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            raise RollbackError(f"无法创建回滚日志目录 {self.dir}：{exc}") from exc
        self._prepared = True
        self._flush()
        return note

    # ------------------------------------------------------------------ 记录
    def capture(self, path: Path) -> None:
        """记下"这个文件即将被改动"。**必须在改动之前调用。**"""
        if not self._prepared:
            raise RollbackError("回滚点尚未建立，不能动设备上的文件")
        p = Path(path)

        # 同一路径记一次就够：一个文件被反复改，第一次的备份仍然是最原始的，
        # 后面再 capture 一次会把"备份好的原始版"当成"当前版"再挪一次 ——
        # 那就把拯救索引用它自己替换掉了。
        if any(p == original for _saved, original in self._saved) or any(p == c for c in self._created):
            return

        if p.exists():
            # 名字带上序号：模型本体与附属物有可能是同名文件，
            # 光用 basename 会在日志目录里互相覆盖，回滚时挪错东西。
            dest = self.dir / f"{len(self._saved):03d}-{p.name}"
            try:
                os.replace(p, dest)
            except OSError as exc:
                raise RollbackError(
                    f"无法先把 {p.name} 挪进回滚日志（{exc}）——"
                    f"覆盖之后将无法还原，因此中止这次部署"
                ) from exc
            self._saved.append((dest, p))
        else:
            self._created.append(p)

        self._flush()

    # ------------------------------------------------------------------ 成功
    def commit(self) -> None:
        """部署成功：后悔药作废。

        这里不校验文件是否真的在目标位置上 —— 那是部署流程自己的事；
        走到 commit 说明 Self Test 已经通过，设备处于用户要的状态。
        """
        shutil.rmtree(self.dir, ignore_errors=True)
        self._saved.clear()
        self._created.clear()
        self._drop_base_if_empty()

    # ------------------------------------------------------------------ 撤销
    def rollback(self) -> tuple[bool, str]:
        """还原到部署之前。返回 `(是否**完整**还原, 说明)`。

        返回 False 时**日志目录会被保留** —— 里面还有救得回来的文件，
        删掉它们等于把最后一次机会也丢了。调用方应当把说明原样报给用户，
        并让平台看到这条失败。
        """
        # 已经失败过一次就不再重试：第一次失败时有些文件已经被挪回去了，
        # 再跑一遍会把"已经还原成功"的那些误判成"备份丢了" —— 报出假的故障，
        # 比不报还坏。把上一次的结论原样还回去。
        if self._failed_outcome is not None:
            return self._failed_outcome

        ok, detail = self._apply(self._saved, self._created)
        if ok:
            shutil.rmtree(self.dir, ignore_errors=True)
            self._saved.clear()
            self._created.clear()
            self._drop_base_if_empty()
        else:
            self._failed_outcome = (ok, detail)
        return ok, detail

    def _drop_base_if_empty(self) -> None:
        """收拾干净之后，如果连空的 `.rollback` 目录都不剩，就把它也删掉。

        它建在 `models_dir` 里（为了和模型文件同一个文件系统），
        所以每一次部署都会在用户能在文件页看到的目录里留下一个空壳。
        `rmdir` 只在目录**确实是空的**时候成功 —— 有残留就留着，
        那正是"回滚没收拾干净"的证据，不该被悄悄抹掉。
        """
        try:
            self.dir.parent.rmdir()
        except OSError:
            pass

    # ------------------------------------------------------------------ 内部
    def _apply(
        self, saved: list[tuple[Path, Path]], created: list[Path]
    ) -> tuple[bool, str]:
        """按清单还原，然后**验证**。回滚与"还原遗留清单"走的是同一段代码 ——
        两条路径分开写，迟早只有一条是修过的。"""
        problems: list[str] = []

        # ---- 动手之前先点一次数 ----
        # 每一条 saved 的备份副本此刻**必须还在**：它只可能被下面那句
        # os.replace 消耗掉。如果进来就不见了，说明有人（或某个清理程序）
        # 动过日志目录，那就**永远还原不回去**了 —— 必须当场说出来。
        #
        # 为什么不能只靠"还原完再看 original 在不在"：在这次部署刚写过
        # original 的情况下，那个位置**必然**有个文件（就是那个新的），
        # 存在性检查会被它骗过去，把"根本没还原"报成"回滚成功"。
        # 这一段是唯一能拆穿它的证据，F 段验收就是为了钉这个。
        for dest, original in saved:
            if not dest.exists():
                problems.append(f"{original.name} 的备份副本已丢失，无法还原")

        # 先删新建的，再把旧的挪回来。
        # 顺序不能反：如果某个路径这轮既被删过又被建过，先挪回旧的可能被后一步删掉。
        for p in created:
            try:
                p.unlink(missing_ok=True)
            except OSError as exc:
                problems.append(f"清除新建文件 {p.name} 失败：{exc}")

        for dest, original in reversed(saved):
            try:
                if dest.exists():
                    original.parent.mkdir(parents=True, exist_ok=True)
                    os.replace(dest, original)
            except OSError as exc:
                problems.append(f"还原 {original.name} 失败：{exc}")

        # ---- 验证：这一步是"回滚失败却报已回滚"的唯一防线 ----
        for dest, original in saved:
            if not original.exists():
                problems.append(f"{original.name} 没有回到原位")
        for p in created:
            if p.exists():
                problems.append(f"新建的 {p.name} 没有被清掉")

        if problems:
            return False, "；".join(problems)

        if not saved and not created:
            return True, "无需还原（部署在改动设备文件之前就失败了）"
        parts = []
        if saved:
            parts.append(f"还原 {len(saved)} 个被覆盖的文件")
        if created:
            parts.append(f"清除 {len(created)} 个新建文件")
        return True, "，".join(parts)

    def _flush(self) -> None:
        """把清单落盘。失败只记不抛：写不了清单不该让部署停下，
        但它会让崩溃恢复失效，所以值得在日志里留一条。"""
        try:
            self.dir.mkdir(parents=True, exist_ok=True)
            data = {
                "saved": [{"saved": str(d), "original": str(o)} for d, o in self._saved],
                "created": [str(p) for p in self._created],
            }
            (self.dir / MANIFEST_NAME).write_text(
                json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8"
            )
        except OSError:
            pass

    def _read_manifest(self) -> dict[str, Any] | None:
        """读上一轮遗留的清单。空目录 / 坏清单都返回 None（按"没有"处理）：
        一个写坏了的清单说明不了任何事，拿它去删文件才是灾难。"""
        f = self.dir / MANIFEST_NAME
        if not f.is_file():
            return None
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None
        if not isinstance(data, dict):
            return None
        if not data.get("saved") and not data.get("created"):
            return None
        return data


def _pairs(items: Any) -> list[tuple[Path, Path]]:
    """把清单里的 saved 段还原成 (挪到哪, 原本在哪)。

    **缺字段的条目直接丢掉，不猜**。一个写坏的清单说明不了任何事，
    照着它去搬文件才是灾难 —— 与项目里"拿不到就留空"同一条纪律。
    """
    out: list[tuple[Path, Path]] = []
    for item in items or []:
        if not isinstance(item, dict):
            continue
        saved, original = str(item.get("saved") or "").strip(), str(item.get("original") or "").strip()
        if saved and original:
            out.append((Path(saved), Path(original)))
    return out
