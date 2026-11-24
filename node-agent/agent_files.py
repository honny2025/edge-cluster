"""设备端文件管理（需求 §4）。

三条边界，先说清楚再写代码：

1. **路径由 Agent 抽象，不写死 /sdcard 或 /data/local/tmp。**
   平台只发「根标识 + 路径」，根清单由 Agent 在 `fs_roots` 里自报。
   不同设备能碰到的目录差别极大（Android 11 的分区存储、Termux 的 HOME、
   厂商 ROM 的魔改路径），写死任何一个都会在某些机型上直接失败 ——
   而失败的地方恰恰是验收要看的"能管理文件"。

2. **所有路径都在沙箱内解析。** 每个请求先 resolve() 展开软链接与 `..`，
   再断言它落在某个根之下。这是**唯一的**安全边界：没有它，
   一个 `../../..` 就能读到整个文件系统。

3. **删除是唯一不可逆的操作**，所以它必须显式带 recursive 才能删目录，
   且拒绝删除根本身 —— 那是"清空整个存储"，不该由一个手滑的点击完成。
"""

from __future__ import annotations

import os
import shutil
from pathlib import Path
from typing import Any, Iterator

# 单次遍历的上限：防止在一个几十万文件的目录上把 Agent 卡死。
# 触顶就**明确报告"已截断"**，而不是假装遍历完了。
_MAX_WALK_ENTRIES = 20000


class FileError(Exception):
    """文件操作失败。消息会原样回给平台，所以要写成人能看懂的话。"""


def _real(p: str) -> str:
    try:
        return str(Path(p).resolve())
    except Exception:
        return ""


def _fs_space(path: str) -> dict[str, Any]:
    """剩余空间。拿不到返回 None（**不是 0**）。

    用 `shutil.disk_usage`：它在 POSIX 上内部就是 statvfs(f_bavail)，语义一致，
    同时又兼顾了 `os.statvfs` 不存在的平台（Windows 上调试 Agent 时就是这样）。
    直接调 statvfs 会让接口在这些平台上永远返回 null —— 而"剩余空间"
    恰恰是需求 §4 明列的一项，不该因为调试平台不同就消失。
    """
    try:
        usage = shutil.disk_usage(path)
        return {"total": int(usage.total), "free": int(usage.free)}
    except Exception:
        pass
    try:  # 兜底：极老的 Termux / 受限环境
        st = os.statvfs(path)
        return {"total": st.f_blocks * st.f_frsize, "free": st.f_bavail * st.f_frsize}
    except Exception:
        return {"total": None, "free": None}


def _candidate_roots(extra: list[tuple[str, str]] = ()) -> list[tuple[str, str, str]]:
    """候选根：(标识, 展示名, 路径)。顺序即优先级。

    **同一个真实目录只留第一次出现的那条** —— Android 上 /sdcard、
    /storage/emulated/0、/storage/self/primary 常常是三个名字指向同一个地方，
    全列出来只会让用户看到三个一模一样的根。
    """
    home = os.path.expanduser("~")
    prefix = os.environ.get("PREFIX", "")
    cands: list[tuple[str, str, str]] = [
        ("home", "Agent 主目录", home),
        ("storage", "内部存储", "/sdcard"),
        ("shared", "共享存储", "/storage/emulated/0"),
        ("external", "外置存储", "/storage"),
        ("tmp", "临时目录", "/data/local/tmp"),
    ]
    if prefix:
        cands.append(("prefix", "Termux 前缀", prefix))
    cands.extend(extra)
    return cands


class FileService:
    """受沙箱约束的文件操作集合。所有方法同步，由调用方丢进线程池。"""

    def __init__(self, extra_roots: list[tuple[str, str]] | None = None) -> None:
        self._extra_roots = list(extra_roots or [])
        self._roots_cache: list[dict[str, Any]] | None = None

    # ------------------------------------------------------------------ 根

    def _roots(self) -> list[dict[str, Any]]:
        """可用根清单。结果缓存 —— 根不会在运行中变，每 5 秒重扫一遍纯属浪费。"""
        if self._roots_cache is not None:
            return self._roots_cache

        out: list[dict[str, Any]] = []
        seen: set[str] = set()
        for rid, label, path in _candidate_roots(self._extra_roots):
            real = _real(path)
            if not real or real in seen or not os.path.isdir(real):
                continue
            seen.add(real)
            out.append(
                {
                    "id": rid,
                    "label": label,
                    "path": real,
                    "readable": os.access(real, os.R_OK),
                    "writable": os.access(real, os.W_OK),
                    **_fs_space(real),
                }
            )
        self._roots_cache = out
        return out

    def invalidate(self) -> None:
        """丢弃根缓存（刚插 SD 卡、刚授权存储权限时用）。"""
        self._roots_cache = None

    def roots(self) -> list[dict[str, Any]]:
        return self._roots()

    def _is_root_path(self, real: Path) -> bool:
        return any(real == Path(r["path"]) for r in self._roots())

    # ------------------------------------------------------------------ 沙箱

    def resolve(self, path: str, *, need_write: bool = False) -> Path:
        """把外部传来的路径解析成沙箱内的绝对路径。越界一律拒绝。

        这是整个模块的安全边界，**只有这一处做校验** —— 集中在一处才审计得清楚。
        need_write 额外要求落在可写的根里：只读根上的写操作要在这里失败，
        而不是等到 open() 抛出一个看不懂的 errno。
        """
        if not path or not str(path).strip():
            raise FileError("路径为空")
        raw = str(path).strip()
        p = Path(raw)
        if not p.is_absolute():
            # 相对路径一律相对"主目录"，语义固定，不给歧义留空间
            home = self._roots()[0]["path"] if self._roots() else os.path.expanduser("~")
            p = Path(home) / raw
        try:
            real = p.resolve(strict=False)
        except Exception as exc:
            raise FileError(f"路径无法解析：{exc}") from exc

        for root in self._roots():
            rp = Path(root["path"])
            if real == rp or rp in real.parents:
                if need_write and not root["writable"]:
                    raise FileError(f"根目录「{root['label']}」不可写")
                return real
        raise FileError("路径不在允许的根目录内")

    # ------------------------------------------------------------------ 读操作

    def list_dir(self, path: str, *, offset: int = 0, limit: int = 500) -> dict[str, Any]:
        """列目录。目录在前、同类按名字排序，分页由平台控制。"""
        target = self.resolve(path)
        if not target.exists():
            raise FileError("目录不存在")
        if not target.is_dir():
            raise FileError("不是一个目录")

        entries: list[dict[str, Any]] = []
        try:
            with os.scandir(target) as it:
                for e in it:
                    entries.append(_entry_of(e))
        except PermissionError as exc:
            raise FileError("没有权限读取该目录") from exc
        except Exception as exc:
            raise FileError(f"读取目录失败：{exc}") from exc

        entries.sort(key=lambda x: (not x["is_dir"], x["name"].lower()))
        limit = max(1, min(int(limit), 2000))
        offset = max(0, int(offset))
        page = entries[offset : offset + limit]
        return {
            "path": str(target),
            "total": len(entries),
            "offset": offset,
            "limit": limit,
            "truncated": offset + limit < len(entries),
            "parent": str(target.parent) if self._in_sandbox(target.parent) else "",
            "space": _fs_space(str(target)),
            "entries": page,
        }

    def _in_sandbox(self, real: Path) -> bool:
        return any(
            real == Path(r["path"]) or Path(r["path"]) in real.parents for r in self._roots()
        )

    def stat(self, path: str) -> dict[str, Any]:
        target = self.resolve(path)
        if not target.exists():
            raise FileError("路径不存在")
        try:
            st = target.stat()
        except Exception as exc:
            raise FileError(f"读取属性失败：{exc}") from exc
        is_dir = target.is_dir()
        return {
            "path": str(target),
            "name": target.name or str(target),
            "is_dir": is_dir,
            "is_link": target.is_symlink(),
            "size": 0 if is_dir else int(st.st_size),
            "mtime": int(st.st_mtime),
            "mode": oct(st.st_mode & 0o777),
            "readable": os.access(target, os.R_OK),
            "writable": os.access(target, os.W_OK),
            "is_root": self._is_root_path(target),
        }

    def du(self, path: str) -> dict[str, Any]:
        """统计目录/文件占用：总字节、文件数、目录数。

        entries 触顶时把 truncated 置真 —— **统计值仍然是真实的下界**，
        但必须让用户知道它是下界，不能当成精确值展示。
        """
        target = self.resolve(path)
        if not target.exists():
            raise FileError("路径不存在")
        if target.is_file():
            return {
                "path": str(target),
                "total_size": int(target.stat().st_size),
                "file_count": 1,
                "dir_count": 0,
                "truncated": False,
                "space": _fs_space(str(target.parent)),
            }

        total = 0
        files = 0
        dirs = 0
        seen = 0
        truncated = False
        for root, dirnames, filenames in os.walk(target, onerror=lambda _e: None):
            dirs += len(dirnames)
            for name in filenames:
                seen += 1
                if seen > _MAX_WALK_ENTRIES:
                    truncated = True
                    break
                try:
                    total += os.path.getsize(os.path.join(root, name))
                except OSError:
                    # 单个文件读不到大小：**不计 0 字节**，直接标 truncated，
                    # 让调用方知道这个总数是"已统计到的部分"而不是精确值。
                    truncated = True
                files += 1
            if truncated:
                break
        return {
            "path": str(target),
            "total_size": total,
            "file_count": files,
            "dir_count": dirs,
            "truncated": truncated,
            "space": _fs_space(str(target)),
        }

    # ------------------------------------------------------------------ 写操作

    def mkdir(self, path: str, *, parents: bool = True) -> dict[str, Any]:
        target = self.resolve(path, need_write=True)
        if target.exists():
            if target.is_dir():
                return {"path": str(target), "created": False, "message": "目录已存在"}
            raise FileError("同名文件已存在")
        try:
            target.mkdir(parents=parents, exist_ok=False)
        except Exception as exc:
            raise FileError(f"创建目录失败：{exc}") from exc
        return {"path": str(target), "created": True}

    def delete(self, path: str, *, recursive: bool = False) -> dict[str, Any]:
        """删除文件或目录。**目录必须显式 recursive**，否则拒绝。

        这不是谨慎过度：一个默认 recursive=True 的"删除目录"按钮，
        就是一次误点清空整块存储。
        """
        target = self.resolve(path, need_write=True)
        if not target.exists():
            raise FileError("路径不存在")
        if self._is_root_path(target):
            raise FileError("拒绝删除根目录")

        if target.is_dir() and not target.is_symlink():
            if not recursive:
                try:
                    has_child = any(target.iterdir())
                except Exception as exc:
                    raise FileError(f"读取目录失败：{exc}") from exc
                if has_child:
                    raise FileError("目录非空，需要显式确认递归删除")
            removed = _count_entries(target)
            try:
                shutil.rmtree(target)
            except Exception as exc:
                raise FileError(f"删除目录失败：{exc}") from exc
            return {"path": str(target), "removed_entries": removed}

        try:
            target.unlink()
        except Exception as exc:
            raise FileError(f"删除文件失败：{exc}") from exc
        return {"path": str(target), "removed_entries": 1}

    def transfer(
        self, src: str, dst: str, *, move: bool = False, overwrite: bool = False
    ) -> dict[str, Any]:
        """复制（move=False）或移动（move=True）。

        三条拒绝理由，都是真实会踩的坑：
        - 目标在源目录里面 → 无限递归复制；
        - 目标是已存在的目录 → 语义歧义（覆盖它还是放进去？），直接拒绝；
        - 目标已存在且未声明 overwrite → 静默丢数据。
        """
        s = self.resolve(src, need_write=move)
        d = self.resolve(dst, need_write=True)
        if not s.exists():
            raise FileError("源路径不存在")
        if s == d:
            raise FileError("源与目标是同一个路径")
        if s.is_dir() and s in d.parents:
            raise FileError("不能把目录复制/移动到它自己的子目录里")
        if d.exists():
            if d.is_dir():
                raise FileError("目标已存在同名目录，请换一个名字")
            if not overwrite:
                raise FileError("目标已存在，需要显式确认覆盖")

        existed = d.exists()
        try:
            if existed:
                d.unlink()
            if move:
                shutil.move(str(s), str(d))
            elif s.is_dir():
                shutil.copytree(str(s), str(d), symlinks=True)
            else:
                shutil.copy2(str(s), str(d))
        except Exception as exc:
            raise FileError(f"{'移动' if move else '复制'}失败：{exc}") from exc
        return {"src": str(s), "dst": str(d), "moved": move, "overwritten": existed}

    # ------------------------------------------------------------------ 传输准备

    def open_read(self, path: str) -> tuple[Path, int]:
        """准备上传（设备 → 平台）。返回 (真实路径, 字节大小)。"""
        target = self.resolve(path)
        if not target.exists():
            raise FileError("路径不存在")
        if target.is_dir():
            raise FileError("不能直接传输目录，请先打包或逐个文件传输")
        try:
            size = int(target.stat().st_size)
        except Exception as exc:
            raise FileError(f"读取文件大小失败：{exc}") from exc
        return target, size

    def prepare_write(self, path: str, *, size: int = 0) -> Path:
        """准备下载（平台 → 设备）：检查父目录可写、剩余空间够。

        **空间不足要在开始下载前就拒绝**：等传到 99% 再失败，
        浪费的是双方的时间和流量，而这两样在移动场景下都稀缺。
        """
        target = self.resolve(path, need_write=True)
        if target.is_dir():
            raise FileError("目标是目录")
        parent = target.parent
        if not parent.exists():
            raise FileError("目标目录不存在")
        if not os.access(parent, os.W_OK):
            raise FileError("目标目录不可写")
        if size > 0:
            free = _fs_space(str(parent)).get("free")
            if free is not None and free < size:
                raise FileError(f"剩余空间不足：需要 {size} 字节，可用 {free} 字节")
        return target


# ---------------------------------------------------------------------- 工具


def _entry_of(e: os.DirEntry) -> dict[str, Any]:
    """把一个目录项转成条目。**读不到属性不等于 0 字节**，所以 size 可为 None。"""
    is_dir = False
    size: int | None = None
    mtime: int | None = None
    try:
        is_dir = e.is_dir(follow_symlinks=False)
    except OSError:
        pass
    try:
        st = e.stat(follow_symlinks=False)
        size = 0 if is_dir else int(st.st_size)
        mtime = int(st.st_mtime)
    except OSError:
        pass
    return {
        "name": e.name,
        "path": e.path,
        "is_dir": is_dir,
        "is_link": e.is_symlink(),
        "size": size,
        "mtime": mtime,
    }


def _walk_paths(root: Path) -> Iterator[Path]:
    for dirpath, dirnames, filenames in os.walk(root, onerror=lambda _e: None):
        for name in dirnames:
            yield Path(dirpath) / name
        for name in filenames:
            yield Path(dirpath) / name


def _count_entries(root: Path) -> int:
    """数一个目录下有多少条目（删除后回报给审计用）。"""
    return sum(1 for _ in _walk_paths(root))
