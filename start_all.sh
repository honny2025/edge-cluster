#!/usr/bin/env bash
# ============================================================================
#  PC 端一键启动（Linux / macOS / WSL，Rust 版）
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
#   * v0.5.0-m5 起后端整体用 Rust 重写，本脚本拉起的是编译出的 Rust 二进制
#     （server-rust 的 edge-cluster-server、node-agent-rust 的 edge-cluster-agent），
#     不再依赖 Python / venv / pip。唯一前置是 Rust 工具链（cargo）。
#   * 二进制**自动探测**：target/release > target/debug > 目录平铺；
#     找不到时自动 `cargo build --release`（首次较慢，之后秒过）。
#   * 启动前检查端口占用，被占用时给出明确提示，而不是静默失败。
#   * 本地直连自动清空代理变量，否则 Agent 连 127.0.0.1 会被代理拒绝。
# ============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVER_DIR="$ROOT/server-rust"
AGENT_DIR="$ROOT/node-agent-rust"
WEB_DIR="$ROOT/web"
SERVER_BIN="edge-cluster-server"
AGENT_BIN="edge-cluster-agent"
NODES="${NODES:-2}"

# ---------------------------------------------------------------- 平台端口探测
# 优先环境变量 PORT，其次读 server-rust/config.toml，最后默认 8000
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

# 端口占用检测（bash 内置 /dev/tcp，不依赖 ss / netstat / lsof）
port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") >/dev/null 2>&1
}

# 找 Rust 二进制：target/release > target/debug > 目录平铺
find_binary() {
  local dir="$1" name="$2" cand
  for rel in "target/release/$name" "target/debug/$name" "$name"; do
    cand="$dir/$rel"
    if [ -x "$cand" ]; then echo "$cand"; return 0; fi
  done
  return 1
}

# 找二进制；找不到就编译。返回完整路径（echo 到 stdout）。
ensure_binary() {
  local dir="$1" name="$2" label="$3" bin
  if bin="$(find_binary "$dir" "$name")"; then echo "$bin"; return 0; fi
  echo "  未找到 $label 二进制，正在编译（首次较慢，之后秒过）..." >&2
  ( cd "$dir" && cargo build --release ) || {
    echo "$label 编译失败。请确认 cargo 已安装且工具链完整。" >&2
    return 1
  }
  bin="$(find_binary "$dir" "$name")" || {
    echo "$label 编译后仍未找到二进制。" >&2
    return 1
  }
  echo "$bin"
}

# ---------------------------------------------------------------- 前端
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

SERVER_EXE="$(ensure_binary "$SERVER_DIR" "$SERVER_BIN" "服务端")"
mkdir -p "$SERVER_DIR/data/logs" "$AGENT_DIR/logs"
# Rust 服务端不支持 --port 参数，端口走 EDGE_SERVER__PORT 环境变量（优先级高于 config.toml）。
export EDGE_SERVER__PORT="$PORT"
( cd "$SERVER_DIR" && nohup "$SERVER_EXE" \
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
  AGENT_EXE="$(ensure_binary "$AGENT_DIR" "$AGENT_BIN" "Agent")"
  echo ">>> 启动 $NODES 个模拟节点"
  : > "$ROOT/.agents.pids"
  for i in $(seq 1 "$NODES"); do
    name=$(printf 'phone-%02d' "$i")
    ( cd "$AGENT_DIR" && nohup "$AGENT_EXE" --simulate --name "$name" \
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
