#!/usr/bin/env bash
# ============================================================================
#  PC 端一键启动（Linux / macOS / WSL）
#
#  用法：
#    ./start_all.sh                # 起平台 + 2 个模拟节点
#    NODES=3 ./start_all.sh        # 起 3 个模拟节点
#    NO_NODES=1 ./start_all.sh     # 只起平台
#    BUILD_WEB=1 ./start_all.sh    # 先构建前端（单端口交付）
#    PORT=8001 ./start_all.sh      # 换平台端口（默认读 config.toml，即 8000）
#
#  停止：./stop_all.sh
#
#  ★ 可移植性说明（与 start_all.ps1 对齐，换一台电脑也能一样跑）：
#   * 运行时**自动探测**，不写死任何本机路径：优先环境变量 PY，其次 python3 / python；
#     探测到的解释器会校验版本 >= 3.10
#   * 虚拟环境建在项目内 .venv —— 整个 edge-cluster 目录可整体拷贝到别的机器
#   * pip 安装失败时自动改用清华镜像重试（国内网络更稳）
#   * 启动前检查端口占用，被占用时给出明确提示，而不是静默失败
#   * 本地直连自动清空代理变量，否则 Agent 连 127.0.0.1 会被代理拒绝
# ============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVER_DIR="$ROOT/server"
AGENT_DIR="$ROOT/node-agent"
WEB_DIR="$ROOT/web"
NODES="${NODES:-2}"

# ---------------------------------------------------------------- 平台端口探测
# 优先环境变量 PORT，其次读 server/config.toml，最后默认 8000
PORT="${PORT:-}"
if [ -z "$PORT" ] && [ -f "$SERVER_DIR/config.toml" ]; then
  # 用 awk 单命令提取，避免 sed | head 在 set -o pipefail 下被 SIGPIPE 误判为失败
  PORT="$(awk -F'=' '/^[[:space:]]*port[[:space:]]*=/ {gsub(/[^0-9]/, "", $2); print $2; exit}' \
          "$SERVER_DIR/config.toml" 2>/dev/null || true)"
fi
PORT="${PORT:-8000}"
URL="http://127.0.0.1:$PORT"

# 本地直连必须绕过系统代理，否则 WS 握手会被代理以 502 拒绝
export NO_PROXY="127.0.0.1,localhost,10.0.0.0/8,192.168.0.0/16,172.16.0.0/12"
export no_proxy="$NO_PROXY"

# ---------------------------------------------------------------- 工具函数

resolve_python() {
  if [ -n "${PY:-}" ]; then echo "$PY"; return 0; fi
  local c
  for c in python3 python; do
    if command -v "$c" >/dev/null 2>&1; then echo "$c"; return 0; fi
  done
  echo ""
}

# 校验解释器可用且版本 >= 3.10；成功时打印版本号（如 3.13）
assert_python_version() {
  local py="$1" v major minor
  v="$("$py" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null || true)"
  if [ -z "$v" ]; then
    echo "无法执行 Python：$py" >&2
    return 1
  fi
  major="${v%%.*}"
  minor="${v#*.}"
  if [ "$major" -lt 3 ] || { [ "$major" -eq 3 ] && [ "$minor" -lt 10 ]; }; then
    echo "需要 Python 3.10 或更高版本，当前为 $v （$py）。" >&2
    echo "  请升级 Python 后重试，或用环境变量 PY 指定其他解释器。" >&2
    return 1
  fi
  echo "$v"
}

# 安装依赖：默认源失败时自动改用清华镜像重试
install_reqs() {
  local py="$1" req="$2" label="$3"
  if "$py" -m pip install --quiet --disable-pip-version-check -r "$req"; then
    return 0
  fi
  echo "  $label 依赖安装失败，改用清华镜像重试..."
  if ! "$py" -m pip install --quiet --disable-pip-version-check \
        -i https://pypi.tuna.tsinghua.edu.cn/simple -r "$req"; then
    echo "$label 依赖安装失败（默认源与清华镜像都试过了）。" >&2
    echo "  请检查网络后手工执行：" >&2
    echo "    $py -m pip install -r $req" >&2
    return 1
  fi
}

# 端口占用检测（bash 内置 /dev/tcp，不依赖 ss / netstat / lsof）
port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") >/dev/null 2>&1
}

# ---------------------------------------------------------------- 环境准备
# 虚拟环境放在项目内（.venv），随目录整体拷贝即可带走，不依赖任何外部路径
echo ">>> 检查 Python 运行环境"
VENV="$ROOT/.venv"
VENV_PY="$VENV/bin/python"
VENV_READY=0

if [ -x "$VENV_PY" ]; then
  # 已存在 .venv：先体检。它可能是从别的电脑/别的系统拷过来的，指向一个
  # 本机不存在的基础解释器，那种情况下 python 连启动都会失败 —— 直接重建。
  if venv_ver="$(assert_python_version "$VENV_PY")"; then
    echo "  复用已有虚拟环境 .venv (Python $venv_ver)"
    VENV_READY=1
  else
    echo "  已有 .venv 不可用，将重建"
    # 用「改名留存」而不是 rm -rf：
    #   ① 改名是 O(1)，递归删几千个文件又慢又可能中途失败留下半残目录
    #   ② 改名即使目录里有文件被占用也能成功
    #   ③ 万一诊断错了，坏环境还在，可以自行回看
    BROKEN="$ROOT/.venv.broken-$(date +%Y%m%d-%H%M%S)"
    if mv "$VENV" "$BROKEN" 2>/dev/null; then
      echo "  已把损坏的环境改名保留为：${BROKEN##*/}（确认没用后可自行删除）"
    else
      echo "  改名失败，退回直接删除"
      rm -rf "$VENV"
    fi
  fi
fi

if [ "$VENV_READY" != "1" ]; then
  PYBIN="$(resolve_python)"
  if [ -z "$PYBIN" ]; then
    echo "找不到 Python 3.10+。请先安装 Python，或用环境变量 PY 指定解释器路径。" >&2
    exit 1
  fi
  base_ver="$(assert_python_version "$PYBIN")" || exit 1
  echo "  使用 Python：$PYBIN ($base_ver)"
  echo "  创建隔离虚拟环境 $VENV"
  "$PYBIN" -m venv "$VENV"
  if [ ! -x "$VENV_PY" ]; then
    echo "虚拟环境创建失败，未生成 $VENV_PY。请确认 Python 安装完整（含 venv 模块）。" >&2
    exit 1
  fi
fi

echo "  安装/校验后端依赖（已装则秒过）"
install_reqs "$VENV_PY" "$SERVER_DIR/requirements.txt" "后端"
echo "  安装/校验 Agent 依赖"
install_reqs "$VENV_PY" "$AGENT_DIR/requirements.txt" "Agent"

if [ "${BUILD_WEB:-0}" = "1" ]; then
  echo ">>> 构建前端"
  if ! command -v npm >/dev/null 2>&1; then
    echo "构建前端需要 Node.js 18+。请先安装 Node，或用环境变量 PATH 引入 npm。" >&2
    exit 1
  fi
  ( cd "$WEB_DIR" && { [ -d node_modules ] || npm install --no-audit --no-fund; } && npm run build )
fi

# ---------------------------------------------------------------- 平台
echo ">>> 启动平台服务端（端口 $PORT）"

# 端口预检：被占用时给出可执行的下一步，而不是等 20 秒后一句"未就绪"
if port_in_use "$PORT"; then
  echo "  端口 $PORT 已被占用，平台起不来。" >&2
  echo "    如果平台其实已经在跑，直接打开即可： $URL" >&2
  echo "    想换端口的话：" >&2
  echo "      PORT=8001 bash start_all.sh" >&2
  exit 1
fi

mkdir -p "$SERVER_DIR/data/logs" "$AGENT_DIR/logs"
( cd "$SERVER_DIR" && nohup "$VENV_PY" run.py --port "$PORT" \
    > "$SERVER_DIR/data/logs/server.console.log" 2>&1 & echo $! > "$ROOT/.server.pid" )
echo "  平台 PID=$(cat "$ROOT/.server.pid")"

echo "  等待平台就绪..."
READY=0
for _ in $(seq 1 30); do
  sleep 0.7
  if curl -sf --noproxy '*' "$URL/api/v1/system/health" >/dev/null 2>&1; then
    READY=1; break
  fi
done
if [ "$READY" = "1" ]; then
  echo "  平台已就绪：$URL  (文档 $URL/docs)"
else
  echo "  !! 平台未就绪，请查看 $SERVER_DIR/data/logs/server.console.log"
fi

# ---------------------------------------------------------------- 模拟节点
if [ "${NO_NODES:-0}" != "1" ]; then
  echo ">>> 启动 $NODES 个模拟节点"
  : > "$ROOT/.agents.pids"
  for i in $(seq 1 "$NODES"); do
    name=$(printf 'phone-%02d' "$i")
    ( cd "$AGENT_DIR" && nohup "$VENV_PY" agent.py --simulate --name "$name" \
        --platform-url "$URL" \
        > "$AGENT_DIR/logs/$name.console.log" 2>&1 & echo $! >> "$ROOT/.agents.pids" )
    echo "  $name 已启动"
    sleep 0.4
  done
fi

cat <<EOF

======================================================================
 启动完成
======================================================================
 平台地址   : $URL
 接口文档   : $URL/docs
 共享 token : edge-cluster-dev-token
EOF
[ "${BUILD_WEB:-0}" = "1" ] && echo " 控制台     : $URL   （已托管前端构建产物）"
[ "${NO_NODES:-0}" != "1" ] && echo " 模拟节点   : $NODES 个（phone-01 ...）"
cat <<EOF

 停止全部   : ./stop_all.sh
EOF
if [ "$PORT" = "8000" ]; then
  echo " 验收检查   : python3 docs/acceptance/m1_check.py --phase online"
else
  echo " 验收检查   : python3 docs/acceptance/m1_check.py --base $URL --phase online"
fi
