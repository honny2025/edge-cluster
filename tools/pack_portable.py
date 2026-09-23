#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""打「可移植交付包」—— 把 edge-cluster 打包成一个可以直接拷给别人/拷到别的机器的 zip。

与 .gitignore 的区别（重要）：
  * .gitignore 排除 web/dist（开发习惯，构建产物不入库）
  * 本脚本 **包含 web/dist** —— 这样对方不装 Node.js 也能直接跑控制台，
    实现"解压即用"。想改前端代码的人再自行 npm install。

用法：
    python tools/pack_portable.py                # 输出到项目同级目录
    python tools/pack_portable.py -o D:/dist     # 指定输出目录
    python tools/pack_portable.py --list         # 只打印清单，不打包

设计原则：**白名单式收集**。默认什么都不收，只收显式列出的东西，
避免"忘了排除某个含隐私数据的目录"。
"""

from __future__ import annotations

import argparse
import re
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------- 收集规则

# 顶层要带上的单个文件
TOP_FILES = [
    "README.md",
    ".gitignore",
    "start_all.ps1",
    "start_all.sh",
    "stop_all.ps1",
    "stop_all.sh",
    "启动.bat",        # 免 PowerShell 执行策略限制的入口（双击即可）
    "停止.bat",
    "快速开始.txt",
]

# 要递归收集的顶层目录
TOP_DIRS = ["server", "web", "node-agent", "docs", "tools"]

# 任何位置都跳过的目录名
SKIP_DIR_NAMES = {
    "node_modules", ".venv", "venv", "__pycache__", ".git",
    ".idea", ".vscode", ".pytest_cache", ".mypy_cache",
}

# 相对项目根、按前缀跳过的路径（这些含本机数据或隐私，绝不能外传）
SKIP_PREFIXES = (
    "server/data",        # SQLite 库（本机节点记录 + 历史指标）+ 日志
    "node-agent/state",   # 节点身份文件（node_id）—— 外传会导致身份冲突
    "node-agent/logs",    # 本机运行日志
)

# 跳过的文件后缀（模型权重、日志、字节码）
SKIP_SUFFIXES = (
    ".pyc", ".pyo", ".log", ".tmp",
    ".gguf", ".onnx", ".safetensors", ".bin", ".pt", ".pth",
)

# 空目录也要保留的（用 .gitkeep 占位）
KEEP_EMPTY_DIRS = ["node-agent/models"]


def version() -> str:
    """从代码里读出当前版本号，避免包名与代码不一致。"""
    main_py = ROOT / "server" / "app" / "main.py"
    try:
        m = re.search(r'^VERSION\s*=\s*"([^"]+)"', main_py.read_text(encoding="utf-8"), re.M)
        if m:
            return m.group(1)
    except OSError:
        pass
    return "dev"


def should_skip(rel: Path) -> bool:
    posix = rel.as_posix()
    if any(posix == p or posix.startswith(p + "/") for p in SKIP_PREFIXES):
        return True
    if rel.suffix.lower() in SKIP_SUFFIXES:
        return True
    return False


def collect() -> list[Path]:
    """返回要入包的相对路径列表（相对 ROOT）。"""
    picked: list[Path] = []

    for name in TOP_FILES:
        p = ROOT / name
        if p.is_file():
            picked.append(Path(name))

    for d in TOP_DIRS:
        base = ROOT / d
        if not base.is_dir():
            continue
        for p in sorted(base.rglob("*")):
            if not p.is_file():
                continue
            rel = p.relative_to(ROOT)
            if any(part in SKIP_DIR_NAMES for part in rel.parts):
                continue
            if should_skip(rel):
                continue
            picked.append(rel)

    # 空目录占位
    for d in KEEP_EMPTY_DIRS:
        if (ROOT / d).is_dir():
            picked.append(Path(d) / ".gitkeep")

    return sorted(set(picked))


def fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.1f} {unit}" if unit != "B" else f"{n} B"
        n /= 1024.0
    return f"{n:.1f} GB"


def build(out_dir: Path | None, list_only: bool) -> int:
    files = collect()
    ver = version()
    zip_name = f"edge-cluster-portable-{ver}.zip"

    total = 0
    sizes: list[tuple[int, Path]] = []
    for rel in files:
        p = ROOT / rel
        if p.is_file():
            s = p.stat().st_size
            total += s
            sizes.append((s, rel))

    print(f"项目版本   : {ver}")
    print(f"收集文件   : {len(files)} 个")
    print(f"未压缩体积 : {fmt_size(total)}")
    print("\n体积最大的 8 个文件：")
    for s, rel in sorted(sizes, reverse=True)[:8]:
        print(f"  {fmt_size(s):>10}  {rel}")

    print("\n排除的内容（本机数据 / 依赖）：")
    for p in ("web/node_modules", "server/data", "node-agent/state",
              "node-agent/logs", ".venv"):
        if (ROOT / p).exists():
            print(f"  - {p}")

    if list_only:
        print("\n清单预览（前 40 项）：")
        for rel in files[:40]:
            print(f"  {rel}")
        if len(files) > 40:
            print(f"  ... 其余 {len(files) - 40} 项")
        return 0

    target_dir = out_dir or ROOT.parent
    target_dir.mkdir(parents=True, exist_ok=True)
    zip_path = target_dir / zip_name

    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as zf:
        for rel in files:
            src = ROOT / rel
            if src.is_file():
                zf.write(src, arcname=str(Path("edge-cluster") / rel))
            else:
                # 空目录占位：写一个零字节条目即可
                zf.writestr(str(Path("edge-cluster") / rel), "")

    zsize = zip_path.stat().st_size
    print(f"\n打包完成   : {zip_path}")
    print(f"压缩后体积 : {fmt_size(zsize)}（压缩率 {100.0 * zsize / max(total, 1):.0f}%）")

    # ------------------------------------------------------------ 自检
    with zipfile.ZipFile(zip_path) as zf:
        names = zf.namelist()
    leaks = [
        n for n in names
        if "node_modules" in n or "/state/" in n or "/data/" in n
        or n.endswith((".pyc", ".gguf", ".log"))
    ]
    print("\n自检：")
    if leaks:
        print("  [FAIL] 包内发现不该有的路径：")
        for n in leaks[:10]:
            print(f"    {n}")
        return 1
    print("  [PASS] 无 node_modules / 无本机身份文件 / 无数据库 / 无日志")
    print(f"  [PASS] 条目总数 {len(names)}（含顶层目录 edge-cluster/）")

    has_dist = any(n.endswith("web/dist/index.html") for n in names)
    print(f"  [{'PASS' if has_dist else 'WARN'}] 前端构建产物 "
          f"{'已包含（对方免装 Node 即可用控制台）' if has_dist else '未包含'}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="打 edge-cluster 可移植交付包")
    ap.add_argument("-o", "--out", default=None, help="输出目录（默认项目上一级）")
    ap.add_argument("--list", action="store_true", help="只列出清单，不实际打包")
    args = ap.parse_args()
    return build(Path(args.out) if args.out else None, args.list)


if __name__ == "__main__":
    sys.exit(main())
