#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""打「可移植交付包」—— 把 edge-cluster 打包成一个可以直接拷给别人 / 拷到别的机器的 zip。

与 .gitignore 的区别（重要）：
  * .gitignore 排除 web/dist（开发习惯，构建产物不入库）
  * 本脚本 **包含 web/dist** —— 这样对方不装 Node.js 也能直接跑控制台，
    实现"解压即用"。想改前端代码的人再自行 npm install。

用法：
    python tools/pack_portable.py                  # 输出到项目上一级目录，并清掉旧包
    python tools/pack_portable.py -o D:/dist       # 指定输出目录
    python tools/pack_portable.py --list           # 只打印清单，不打包
    python tools/pack_portable.py --no-clean       # 不清掉同目录的旧包

设计原则：**白名单式收集**。默认什么都不收，只收显式列出的东西，
避免"忘了排除某个含隐私数据的目录"。

但白名单有个反面：新写了一份文档却忘了加进清单，包就悄悄漏了它。
所以本脚本在收集完之后会做一次**对账**：根目录下凡是 `.md` / `.txt` / `.html`
而既不在白名单、也不在"明确不收"清单里的，一律 WARN 列出来让你决定。
漏文件比多收文件难发现得多，所以要吵出来。
"""

from __future__ import annotations

import argparse
import getpass
import os
import socket
import re
import sys
import zipfile
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------- 收集规则

# 顶层要带上的单个文件：可执行入口 + 配置文件
TOP_FILES = [
    # 入口脚本
    "start_all.ps1", "start_all.sh", "stop_all.ps1", "stop_all.sh",
    "启动.bat", "停止.bat",      # 免 PowerShell 执行策略限制（双击即可）
    "环境初始化.bat", "环境初始化.ps1",  # 新机器跑一次：装 WebView2 / 放行防火墙 / 查 UCRT
    # 配置 / 元信息
    ".gitignore", ".gitattributes", ".editorconfig", ".env.example",
    "LICENSE",
]

# 桌面壳二进制：打包时平铺到包根（edge-cluster/ 下）。它启动时会顺着当前目录向上
# 找到 server-rust/config.toml，再拉起包里的 edge-cluster-server，所以必须放在包根、
# 不能塞进 web/src-tauri/target 的深路径里。源是 MSVC release 构建产物，构建后由
# 人/脚本复制到项目根（edge-cluster-desktop.exe）——见 collect() 里对它的显式复制。
DESKTOP_SHELL_SRC = "web/src-tauri/target/x86_64-pc-windows-msvc/release/edge-cluster-desktop.exe"
DESKTOP_SHELL_DST = "edge-cluster-desktop.exe"

# 根目录下要带上的文档。**新增根目录文档记得加到这里**（忘了加会有对账 WARN 提醒）。
# 其余文档已分类到 docs/guides、docs/design、docs/reports 等子目录，
# 由 TOP_DIRS 的 rglob 自动收进包，不用再在这里列。
ROOT_DOCS = [
    # —— 入口三件套（给不同的人是不同的入口）——
    "快速开始.txt",           # 想直接跑起来
    "平台使用指南.md",         # 想搞懂这个平台每一块是干什么的
    "手机端部署指南.md",       # 要往手机上装 Agent
    # —— 根目录只留标准项 + 到处被引用的核心单 ——
    "README.md",
    "CHANGELOG.md", "ROADMAP.md", "CONTRIBUTING.md", "SECURITY.md",
    "功能清单.md",             # 已完成 / 还缺什么（多处引用，留根目录）
    "系统迁移与使用指南.md",   # 换机器 / 怎么用 / 出问题找谁
    "工作室真机测试现场指南(1).html",  # 工作室现场照着执行的权威指南（交付物）
]

# 根目录下**明确不收**的（写了理由，免得后人以为是漏了）
ROOT_DOCS_EXCLUDED = {
    "项目详细报告.html": "一次性的汇报稿，内容已并入 README / 实现情况报告",
    "手机边缘计算平台.html": "早期汇报稿，已被「平台使用指南.md」取代",
    "平台详细情况与待解决问题.html": "早期汇报稿，内容已并入 功能清单.md",
    "迁移与真机部署操作指南.html": "早期汇报稿，已被「手机端部署指南.md」取代",
    "_new_studio_guide.html": "工作室指南的权威工作副本，交付用正式名「工作室真机测试现场指南(1).html」",
}

# 要递归收集的顶层目录
# v0.5.0-m5 起后端整体 Rust 化：server/（Python FastAPI）已被 server-rust/ 完全取代，
# 不再入包；node-agent/（Python）保留 —— 手机端过渡期还在用（install.sh + agent.py）。
TOP_DIRS = ["server-rust", "node-agent-rust", "web", "node-agent", "docs", "tools"]

# 随包分发**预编译**的 Rust release 二进制（跨 Windows x64 直接跑，对方免装 Rust）。
# 二进制是自包含的：SQLite 走 bundled、TLS 走 rustls、不依赖 MinGW 的 libgcc/libwinpthread，
# 只依赖 Windows 系统 DLL + Universal CRT（Win10/11 自带）。见系统迁移与使用指南.md §1.3。
PREBUILT_BIN_CRATES = [
    ("server-rust", "edge-cluster-server"),
    ("node-agent-rust", "edge-cluster-agent"),
]

# 任何位置都跳过的目录名
SKIP_DIR_NAMES = {
    "node_modules", ".venv", "venv", "__pycache__", ".git",
    ".idea", ".vscode", ".pytest_cache", ".mypy_cache",
    "target",              # Rust 编译产物（web/src-tauri/target，跑过 cargo build 后几百 MB）
    "gen",                 # Tauri 开发期生成目录
}

# 相对项目根、按前缀跳过的路径（这些含本机数据或隐私，绝不能外传）
SKIP_PREFIXES = (
    "server/data",        # 旧 Python 版的 SQLite 库 + 日志（已废弃，但目录可能还在）
    "server/staging",     # 旧 Python 版文件暂存区
    "server-rust/data",   # Rust 版 SQLite 库（本机节点记录 + 历史指标）+ 日志 + 模型索引
    "node-agent/state",   # 节点身份文件（node_id）—— 外传会导致身份冲突
    "node-agent/logs",    # 本机运行日志
    "node-agent/models",  # 本机下载过的模型权重
    "node-agent-rust/state",   # Rust 版节点身份（node_id / token）
    "node-agent-rust/logs",    # Rust 版运行日志
    "node-agent-rust/models",  # Rust 版模型目录
)

# 跳过的文件后缀（模型权重、日志、字节码、本机状态）
SKIP_SUFFIXES = (
    ".pyc", ".pyo", ".log", ".tmp", ".pid",
    ".gguf", ".onnx", ".safetensors", ".bin", ".pt", ".pth", ".tflite", ".dlc",
)

# 跳过的文件名（散落在仓库里的本机运行态文件）
SKIP_FILE_NAMES = {".running.pids", ".DS_Store", "Thumbs.db", "desktop.ini",
                   "_m5_state.json"}  # m5_selfheal_check 的中间态，跑完就该清掉

# 空目录也要保留的（用 .gitkeep 占位）
KEEP_EMPTY_DIRS = ["node-agent/models"]

# 打包完自检时必须存在的路径 —— 少一个说明收集规则被改坏了
REQUIRED_IN_PACKAGE = [
    "edge-cluster/README.md",
    "edge-cluster/快速开始.txt",
    "edge-cluster/平台使用指南.md",
    "edge-cluster/手机端部署指南.md",
    "edge-cluster/功能清单.md",
    # Rust 后端（server-rust / node-agent-rust）
    "edge-cluster/server-rust/Cargo.toml",
    "edge-cluster/server-rust/config.toml",
    "edge-cluster/server-rust/src/main.rs",
    "edge-cluster/node-agent-rust/Cargo.toml",
    "edge-cluster/node-agent-rust/src/main.rs",
    # 手机端过渡期 Python agent
    "edge-cluster/node-agent/agent.py",
    "edge-cluster/node-agent/install.sh",
    # 桌面壳 + 新机器环境初始化脚本
    "edge-cluster/edge-cluster-desktop.exe",
    "edge-cluster/环境初始化.bat",
    "edge-cluster/环境初始化.ps1",
]


def version() -> str:
    """从 server-rust/Cargo.toml 读出当前版本号，避免包名与代码不一致。"""
    cargo = ROOT / "server-rust" / "Cargo.toml"
    try:
        m = re.search(r'^version\s*=\s*"([^"]+)"', cargo.read_text(encoding="utf-8"), re.M)
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
    if rel.name in SKIP_FILE_NAMES:
        return True
    return False


def missing_root_docs() -> list[str]:
    """根目录下的文档，既不在白名单也没写"明确不收"理由的。

    这条是为了治「写了新文档但忘了加进包」——它不会报错，只会在别人解压后
    发现少东西。宁可每次打包装的时候聒噪一句。
    """
    known = set(ROOT_DOCS) | set(ROOT_DOCS_EXCLUDED)
    out: list[str] = []
    for p in sorted(ROOT.iterdir()):
        if not p.is_file():
            continue
        if p.suffix.lower() not in (".md", ".txt", ".html"):
            continue
        if p.name not in known:
            out.append(p.name)
    return out


def collect() -> list[Path]:
    """返回要入包的相对路径列表（相对 ROOT）。"""
    picked: list[Path] = []

    for name in TOP_FILES + ROOT_DOCS:
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

    # 预编译 release 二进制（target/ 被 SKIP_DIR_NAMES 整目录跳过，这里显式补上
    # target/release/ 下的可执行文件，让 Windows x64 用户解压即用，免装 Rust）
    for crate, name in PREBUILT_BIN_CRATES:
        for exe in (name + ".exe", name):
            p = ROOT / crate / "target" / "release" / exe
            if p.is_file():
                picked.append(p.relative_to(ROOT))

    # 桌面壳：平铺到包根（打包前 build() 里已把它从 MSVC release 同步到项目根）
    if (ROOT / DESKTOP_SHELL_DST).is_file():
        picked.append(Path(DESKTOP_SHELL_DST))

    return sorted(set(picked))


def sync_desktop_shell() -> None:
    """把 MSVC release 的桌面壳同步到项目根，保证打包用的是最新编译的那一版。"""
    src = ROOT / DESKTOP_SHELL_SRC
    dst = ROOT / DESKTOP_SHELL_DST
    if not src.is_file():
        return
    if dst.is_file() and dst.stat().st_mtime >= src.stat().st_mtime:
        return
    try:
        dst.write_bytes(src.read_bytes())
        print(f"[sync] 桌面壳已从 {DESKTOP_SHELL_SRC} 同步到项目根")
    except OSError as e:
        print(f"[WARN] 同步桌面壳失败（若它正在运行会锁文件；打包将跳过它）：{e}")


def fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.1f} {unit}" if unit != "B" else f"{n} B"
        n /= 1024.0
    return f"{n:.1f} GB"




# ---------------------------------------------------------------- 本机身份
# 交付包是给别人的。里面一旦出现**打包这台机器**的用户名、主机名或内网 IP，
# 就是一次信息泄露 —— 而且这种泄露看目录清单根本发现不了，只能读内容。
# 所以这里动态取本机身份词，而不是写死一串关键词：换个人在这台机器上打包，
# 同样能拦住他自己。
def machine_identity() -> list[tuple[str, str]]:
    toks: list[tuple[str, str]] = []
    try:
        user = os.environ.get("USERNAME") or getpass.getuser()
    except Exception:
        user = ""
    if user and len(user) >= 3:          # 太短的名字（如 ab）会满屏误报
        toks.append(("本机用户名", user))
    try:
        host = socket.gethostname()
    except OSError:
        host = ""
    if host and len(host) >= 3:
        toks.append(("本机主机名", host))
    return toks


def local_ips() -> set[str]:
    """本机出口 IP。连一个不会被真的发包的对端即可问出来。"""
    ips: set[str] = set()
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            s.connect(("10.255.255.255", 1))
            ips.add(s.getsockname()[0])
        finally:
            s.close()
    except OSError:
        pass
    return ips


# 只扫文本；这些后缀是二进制，跳过（否则解码全是噪音）
TEXT_SKIP_SUFFIXES = (
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".woff", ".woff2", ".ttf",
    ".eot", ".zip", ".gz", ".pdf", ".db", ".sqlite", ".so", ".dll", ".exe",
)


def build_manifest(ver: str, files: list[Path], total: int) -> str:
    """包根目录放一份清单，对方解压后第一眼就知道这是什么、从哪开始读。"""
    lines = [
        "edge-cluster 可移植交付包",
        "=" * 46,
        f"版本        : {ver}",
        f"打包时间    : {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}",
        f"文件数      : {len(files)}",
        f"未压缩体积  : {fmt_size(total)}",
        "",
        "怎么开始",
        "-" * 46,
        "第一次在新电脑上用（3 步）：",
        "  1. 双击「环境初始化.bat」跑一次 —— 自动装 WebView2 运行时（桌面壳依赖）、",
        "     放行防火墙 8000 端口（让手机能连上来）。重复跑不会重复装。",
        "  2. 双击「edge-cluster-desktop.exe」打开桌面控制台，或双击「启动.bat」",
        "     （起平台 + 用浏览器打开 http://127.0.0.1:8000）。",
        "  3. 手机端照「手机端部署指南.md」装 Agent 并连平台。",
        "",
        "  包内已带**预编译**的 Rust 二进制（server-rust 的 edge-cluster-server、",
        "  node-agent-rust 的 edge-cluster-agent、桌面壳 edge-cluster-desktop），",
        "  SQLite 静态编译进二进制、前端 web/dist 已打包。所以 Windows x64 用户",
        "  **无需装 Python / Rust / Node.js**，解压即用。",
        "",
        "  （桌面壳本身只依赖系统自带的 WebView2 运行时，Win10/11 一般已内置；",
        "   老系统跑「环境初始化.bat」会自动补装。）",
        "",
        "  只有两种情形才需要额外装工具链：",
        "  1. 要改后端 Rust 代码重新编译 → 装 Rust 工具链（rustup）+ C 链接器",
        "     （Windows 用 MSVC 的 VS Build Tools，或 MinGW-w64）。",
        "  2. 要改前端源码重新构建 → 装 Node.js 18+，然后 npm install && npm run build。",
        "",
        "  Linux / macOS：包内没有对应平台的二进制，需装 Rust 工具链后：",
        "    cd server-rust && cargo build --release",
        "    cd node-agent-rust && cargo build --release",
        "    然后 bash start_all.sh",
        "",
        "  手机端（安卓/Termux）走 node-agent/（Python 过渡期），见「手机端部署指南.md」。",
        "",
        "根目录文档导读",
        "-" * 46,
        "  快速开始.txt              最小启动步骤（先看这个）",
        "  系统迁移与使用指南.md     换电脑 / 日常使用 / 排错（第二先看这个）",
        "  平台使用指南.md           界面每一块是什么、怎么看懂「—」",
        "  手机端部署指南.md         怎么把 Agent 装到安卓手机上（含给 AI 的提示词）",
        "  功能清单.md               已实现什么、还缺什么（A/B/C 三类分开列）",
        "  docs/design/平台设计文档.md   架构与状态机",
        "  docs/guides/真机测试检查单.md 插上手机后照着一项项打勾",
        "  docs/user-manual.md       完整用户手册",
        "  docs/acceptance/          验收脚本（可复跑，每个都写明验什么）",
        "  docs/guides/              真机接入 / Termux / 局域网部署等操作指南",
        "  docs/design/              架构、技术决策、做不了功能的解决办法",
        "",
        "这个包不含什么",
        "-" * 46,
        "  不含编译中间产物（各 target/ 目录、.venv / node_modules）",
        "  不含 server-rust/data/     —— 原机器的数据库与日志，带过去会串数据",
        "  不含 node-agent-rust/state/ —— 节点身份文件（node_id/token），带过去会身份冲突",
        "  不含 node-agent/state/ logs/ models/ —— 同理（手机端本机状态）",
        "  不含模型权重（.gguf/.onnx 等，动辄几 GB）—— 模型要重新部署或按备份清单手工拷",
        "",
        "  包内已带 web/dist/ 和 Windows x64 的预编译二进制，",
        "  所以不装 Node.js、不装 Rust 也能直接用控制台。",
        "",
    ]
    return "\n".join(lines)


def build(out_dir: Path | None, list_only: bool, clean: bool) -> int:
    sync_desktop_shell()
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
    for p in ("web/node_modules", "server/data", "server/staging",
              "server-rust/data", "node-agent/state", "node-agent/logs",
              "node-agent/models", "node-agent-rust/state", "node-agent-rust/logs",
              "node-agent-rust/models", ".venv"):
        if (ROOT / p).exists():
            print(f"  - {p}")

    stray = missing_root_docs()
    if stray:
        print("\n[WARN] 根目录有文档没进包，也没写不收的理由：")
        for n in stray:
            print(f"  ? {n}")
        print("       要收 → 加进 pack_portable.py 的 ROOT_DOCS")
        print("       不收 → 加进 ROOT_DOCS_EXCLUDED 并写明理由")

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

    manifest = build_manifest(ver, files, total)

    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as zf:
        zf.writestr("edge-cluster/打包说明.txt", manifest)
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

    ok = True
    print("\n自检：")

    leaks = [
        n for n in names
        if any(part in n for part in ("/node_modules/", "/.venv/", "/__pycache__/"))
        or n.endswith((".pyc", ".gguf", ".log", ".pid"))
        or any(p in n for p in ("/server/data/", "/node-agent/state/",
                                "/server-rust/data/", "/node-agent-rust/state/"))
    ]
    if leaks:
        print("  [FAIL] 包内发现不该有的路径：")
        for n in leaks[:10]:
            print(f"    {n}")
        ok = False
    else:
        print("  [PASS] 无 node_modules / 无本机身份文件 / 无数据库 / 无日志")

    ident = machine_identity()
    mine_ips = local_ips()
    id_hits: list[str] = []
    ip_hits: list[str] = []
    with zipfile.ZipFile(zip_path) as zf:
        for n in names:
            if n.endswith("/") or n.lower().endswith(TEXT_SKIP_SUFFIXES):
                continue
            try:
                body = zf.read(n).decode("utf-8", errors="ignore")
            except (OSError, KeyError):
                continue
            low = body.lower()
            for label, tok in ident:
                if tok.lower() in low:
                    id_hits.append(f"{n}  含{label} {tok!r}")
            for ip in mine_ips:
                if ip in body:
                    ip_hits.append(f"{n}  含本机 IP {ip}")
    if id_hits:
        print("  [FAIL] 包内出现本机身份信息（交付出去等于自报家门）：")
        for h in id_hits[:10]:
            print(f"    {h}")
        print("     → 改成占位符，如 <用户名> / <你的IP>")
        ok = False
    else:
        print(f"  [PASS] 无本机身份信息（已核对 {len(ident)} 个身份词"
              f"{'、' + str(len(mine_ips)) + ' 个本机 IP' if mine_ips else ''}）")
    if ip_hits:
        print("  [WARN] 包内出现本机内网 IP（示例请改用 192.168.1.100 这类通用地址）：")
        for h in ip_hits[:10]:
            print(f"    {h}")

    missing = [r for r in REQUIRED_IN_PACKAGE if r not in names]
    if missing:
        print("  [FAIL] 关键文件不在包里：")
        for n in missing:
            print(f"    {n}")
        ok = False
    else:
        print(f"  [PASS] {len(REQUIRED_IN_PACKAGE)} 个关键文件齐全（入口文档 / 依赖清单 / 启动脚本）")

    print(f"  [PASS] 条目总数 {len(names)}（含顶层目录 edge-cluster/）")

    has_dist = any(n.endswith("web/dist/index.html") for n in names)
    print(f"  [{'PASS' if has_dist else 'FAIL'}] 前端构建产物 "
          f"{'已包含（对方免装 Node 即可用控制台）' if has_dist else '未包含 —— 对方必须自己 npm run build'}")
    if not has_dist:
        ok = False

    # 清掉同目录下更早的包，只留最新 —— 交付时「保留最新版的就行」
    if clean:
        olds = sorted(
            p for p in target_dir.glob("edge-cluster-portable-*.zip") if p != zip_path
        )
        if olds:
            print("\n清理旧包：")
            for p in olds:
                try:
                    sz = fmt_size(p.stat().st_size)
                    p.unlink()
                    print(f"  - 已删除 {p.name}（{sz}）")
                except OSError as exc:
                    print(f"  ! 删不掉 {p.name}: {exc}")
        else:
            print("\n清理旧包：无")

    return 0 if ok else 1


def main() -> int:
    ap = argparse.ArgumentParser(description="打 edge-cluster 可移植交付包")
    ap.add_argument("-o", "--out", default=None, help="输出目录（默认项目上一级）")
    ap.add_argument("--list", action="store_true", help="只列出清单，不实际打包")
    ap.add_argument("--no-clean", action="store_true", help="保留同目录下的旧包")
    args = ap.parse_args()
    return build(Path(args.out) if args.out else None, args.list, not args.no_clean)


if __name__ == "__main__":
    sys.exit(main())
