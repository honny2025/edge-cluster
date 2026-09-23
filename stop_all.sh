#!/usr/bin/env bash
# 停止由 start_all.sh 启动的全部进程
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

stop_pidfile() {
  local f="$1" label="$2"
  [ -f "$f" ] || return 0
  while read -r pid; do
    [ -n "$pid" ] || continue
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
      sleep 0.3
      kill -KILL "$pid" 2>/dev/null || true
      echo "  已停止 $label PID=$pid"
    fi
  done < "$f"
  rm -f "$f"
}

stop_pidfile "$ROOT/.agents.pids" "模拟节点"
stop_pidfile "$ROOT/.server.pid" "平台服务端"

# 兜底：按命令行特征回收（run.py 和 agent.py 都要覆盖，否则平台会漏杀）
pkill -f 'edge-cluster.*(agent|run)\.py' 2>/dev/null || true
echo "全部进程已停止。"
