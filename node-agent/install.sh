#!/data/data/com.termux/files/usr/bin/bash
# ============================================================================
#  手机端一键安装脚本（Termux · 免 root）
#
#  做的事：
#    1. 安装 Python 及可选依赖（termux-api 用于读电池温度）
#    2. 安装 Agent 的 Python 依赖
#    3. 申请存储与唤醒锁（wake-lock）权限，防止息屏后被杀
#    4. 写入 ~/.bashrc 保活片段（前台守护 + 定期自检）
#    5. 可选：安装 termux-boot 开机自启脚本
#
#  用法（在 Termux 里）：
#    bash install.sh
#    bash install.sh --token 你的平台token --platform http://192.168.1.100:8000
#    bash install.sh --no-boot        # 不装开机自启
#
#  ⚠️ 系统限制（必须知悉）：
#   * 免 root 方案无法做成真正的 Android Service，进程优先级依赖 Termux 自身；
#   * 手机厂商的「省电策略 / 后台限制」可能杀死 Termux，需手动在
#     设置 → 应用 → Termux 中关闭省电限制、允许后台活动；
#   * MIUI / EMUI / ColorOS / FuntouchOS 等还需在「最近任务」里给 Termux 加锁；
#   * termux-wake-lock 能显著提升存活率，但**不能 100% 保证不被杀**。
# ============================================================================
set -uo pipefail

AGENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG_FILE="$AGENT_DIR/config.toml"

TOKEN=""
PLATFORM=""
INSTALL_BOOT=1

while [ $# -gt 0 ]; do
  case "$1" in
    --token)     TOKEN="$2"; shift 2 ;;
    --platform)  PLATFORM="$2"; shift 2 ;;
    --no-boot)   INSTALL_BOOT=0; shift ;;
    -h|--help)   sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "未知参数: $1"; exit 1 ;;
  esac
done

log()  { printf '\n\033[36m>>> %s\033[0m\n' "$*"; }
warn() { printf '\033[33m  ! %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m  ✓ %s\033[0m\n' "$*"; }

# ------------------------------------------------------------------ 环境检查
log "检查运行环境"
if [ ! -d "/data/data/com.termux" ]; then
  warn "看起来不是 Termux 环境。本脚本为 Termux 设计，其他 Linux 直接跑 agent.py 即可。"
fi

# ------------------------------------------------------------------ 依赖安装
log "安装系统依赖"
export DEBIAN_FRONTEND=noninteractive

pkg update -y >/dev/null 2>&1 || warn "pkg update 失败，继续尝试"

for p in python python-pip; do
  if ! command -v "${p/python/python3}" >/dev/null 2>&1; then
    pkg install -y "$p" >/dev/null 2>&1 || warn "安装 $p 失败"
  fi
done

if command -v python3 >/dev/null 2>&1; then
  ok "python3 $(python3 --version 2>&1 | awk '{print $2}')"
else
  echo "  ✗ Python 安装失败，无法继续。请手动执行：pkg install python"
  exit 1
fi

# termux-api 是可选的：用于读取电池温度等指标。缺了 Agent 照样能跑（相关指标留空）
if ! command -v termux-battery-status >/dev/null 2>&1; then
  warn "未检测到 termux-api（可选）。若要采集电池温度："
  warn "  1) 执行 pkg install termux-api"
  warn "  2) 另需从应用商店安装 'Termux:API' 应用"
  warn "  缺失时相关指标会留空，不影响 Agent 运行。"
else
  ok "termux-api 已就绪"
fi

log "安装 Python 依赖"
python3 -m pip install --quiet --upgrade pip >/dev/null 2>&1 || true
python3 -m pip install --quiet websockets httpx >/dev/null 2>&1 || {
  warn "pip 安装失败，尝试指定清华镜像"
  python3 -m pip install --quiet -i https://pypi.tuna.tsinghua.edu.cn/simple websockets httpx
}
ok "Python 依赖安装完成"

# ------------------------------------------------------------------ 写入配置
log "写入 Agent 配置"
if [ -n "$TOKEN" ]; then
  sed -i "s|^token = .*|token = \"$TOKEN\"|" "$CONFIG_FILE"
  ok "已设置 token"
fi
if [ -n "$PLATFORM" ]; then
  sed -i "s|^platform_url = .*|platform_url = \"$PLATFORM\"|" "$CONFIG_FILE"
  ok "已设置平台地址：$PLATFORM"
else
  warn "未指定 --platform，将依赖 UDP 广播自动发现（需与平台同一局域网）"
fi

# ------------------------------------------------------------------ 权限
log "申请 Termux 权限"
if command -v termux-setup-storage >/dev/null 2>&1; then
  # 需要用户在弹出的系统对话框中点「允许」
  termux-setup-storage >/dev/null 2>&1 || true
  ok "已请求存储权限（若弹出系统对话框请点允许）"
fi

# ------------------------------------------------------------------ 保活
log "配置后台保活"

# 1) ~/.bashrc 保活片段：进入 Termux 时自动拉起 Agent（已在跑则跳过）
BASHRC="$HOME/.bashrc"
MARKER="# >>> edge-cluster agent keepalive >>>"
if ! grep -q "$MARKER" "$BASHRC" 2>/dev/null; then
  cat >> "$BASHRC" <<EOF

$MARKER
# 进入 Termux 时自动保活：申请唤醒锁 + 拉起 Agent（单实例）
if [ -d "$AGENT_DIR" ] && command -v python3 >/dev/null 2>&1; then
  if ! pgrep -f "agent.py" >/dev/null 2>&1; then
    termux-wake-lock >/dev/null 2>&1 || true
    cd "$AGENT_DIR" && nohup python3 agent.py >> "$AGENT_DIR/logs/nohup.log" 2>&1 &
    echo "[edge-cluster] Agent 已在后台启动"
  fi
fi
# <<< edge-cluster agent keepalive <<<
EOF
  ok "已写入 ~/.bashrc 保活片段"
else
  ok "~/.bashrc 保活片段已存在，跳过"
fi

# 2) 立即申请唤醒锁
if command -v termux-wake-lock >/dev/null 2>&1; then
  termux-wake-lock >/dev/null 2>&1 && ok "已申请 termux-wake-lock（防息屏后被挂起）"
else
  warn "termux-wake-lock 不可用（需 pkg install termux-api + Termux:API 应用）"
fi

# 3) 开机自启（termux-boot）
if [ "$INSTALL_BOOT" = "1" ]; then
  BOOT_DIR="$HOME/.termux/boot"
  mkdir -p "$BOOT_DIR"
  cat > "$BOOT_DIR/edge-cluster-agent.sh" <<EOF
#!/data/data/com.termux/files/usr/bin/sh
# 开机自启（由 Termux:Boot 应用触发）
termux-wake-lock
cd "$AGENT_DIR"
sleep 15   # 等网络就绪
nohup python3 agent.py >> "$AGENT_DIR/logs/nohup.log" 2>&1 &
EOF
  chmod +x "$BOOT_DIR/edge-cluster-agent.sh"
  ok "已安装开机自启脚本：$BOOT_DIR/edge-cluster-agent.sh"
  warn "开机自启需另装 'Termux:Boot' 应用，并**至少手动打开一次**该应用以激活"
else
  warn "已跳过开机自启安装（--no-boot）"
fi

# ------------------------------------------------------------------ 启动
log "启动 Agent"
mkdir -p "$AGENT_DIR/logs"
cd "$AGENT_DIR"
if pgrep -f "agent.py" >/dev/null 2>&1; then
  warn "检测到已有 Agent 在运行，先停止旧进程"
  pkill -f "agent.py" >/dev/null 2>&1 || true
  sleep 1
fi
nohup python3 agent.py >> "$AGENT_DIR/logs/nohup.log" 2>&1 &
sleep 4

cat <<'EOF'

======================================================================
 安装完成
======================================================================
 查看日志   : tail -f logs/agent.log
 查看进程   : pgrep -f agent.py
 停止 Agent : pkill -f agent.py
 重启 Agent : cd 到本目录 && nohup python3 agent.py >> logs/nohup.log 2>&1 &
 唤醒锁状态 : termux-wake-lock / termux-wake-unlock

 ⚠️ 请务必完成以下手动设置，否则后台很容易被杀：
   1) 系统设置 → 应用 → Termux → 省电策略 → 无限制 / 允许后台活动
   2) 系统设置 → 应用 → Termux → 允许自启动
   3) 最近任务列表里给 Termux「加锁」，防止一键清理时被杀
EOF

# 打印最近几行日志，方便立即确认是否连上平台
echo
echo "--- 最近日志 ---"
tail -n 12 "$AGENT_DIR/logs/agent.log" 2>/dev/null || tail -n 12 "$AGENT_DIR/logs/nohup.log" 2>/dev/null || echo "(暂无日志)"
