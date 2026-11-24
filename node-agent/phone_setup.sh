#!/data/data/com.termux/files/usr/bin/bash
# ============================================================================
#  手机端（Termux）一键接入脚本 —— 定位 / 拷贝 / 装依赖 / 写配置 / 启动 / 验证
#
#  与 install.sh 的区别：install.sh 假设代码就在当前目录；
#  本脚本会**自己去找** agent.py（不管你把它放在 Download 哪个子目录里），
#  拷进 Termux 家目录后再跑。也给 AI（codex 等）留了无交互的执行路径。
#
#  用法（Termux 里）：
#    bash phone_setup.sh --platform http://10.86.33.190:8000 --name honor-x70
#    bash phone_setup.sh --platform http://10.86.33.190:8000 --name honor-x70 --token 自定义token
#    bash phone_setup.sh --src /sdcard/Download/termux/node-agent --platform http://... --name xxx
#    bash phone_setup.sh --platform http://... --name xxx --no-kill   # 不杀已有 agent.py
#
#  不传 --platform：走 UDP 广播自动发现（要求与平台同一网段、无客户端隔离）
#  幂等：可重复执行；已有 ~/node-agent 时保留 state/ 与 logs/，不破坏节点身份
# ============================================================================
set -uo pipefail

PLATFORM=""
NAME="phone-android"
TOKEN="edge-cluster-dev-token"
SRC=""
NO_KILL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --platform) PLATFORM="$2"; shift 2 ;;
    --name)     NAME="$2";     shift 2 ;;
    --token)    TOKEN="$2";    shift 2 ;;
    --src)      SRC="$2";      shift 2 ;;
    --no-kill)  NO_KILL=1;     shift ;;
    -h|--help)  sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 1 ;;
  esac
done

log()  { printf '\n\033[36m>>> %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m  [OK] %s\033[0m\n' "$*"; }
warn() { printf '\033[33m  [!] %s\033[0m\n' "$*"; }
die()  { printf '\033[31m  [X] %s\033[0m\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- 0. 参数校验
# 注意：连字符必须放在字符类末尾（_.- 的顺序），否则 `_-.` 会被当成字符范围
# 而导致 honor-x70 这类合法名字被误判为非法（实测踩过）。
case "$NAME" in
  *[!A-Za-z0-9_.-]*) die "节点名只能含字母/数字/下划线/连字符/点，当前是：$NAME" ;;
esac
[ -n "$NAME" ] || die "--name 不能为空"

# ---------------------------------------------------------------- 1. 定位
log "定位 node-agent"
CAND=""
if [ -n "$SRC" ] && [ -f "$SRC/agent.py" ]; then
  CAND="$SRC"
elif [ -f "$HOME/node-agent/agent.py" ]; then
  CAND="$HOME/node-agent"
else
  FOUND=$(find /sdcard/Download /sdcard "$HOME" -maxdepth 5 -name agent.py 2>/dev/null | head -n 1)
  [ -n "$FOUND" ] && CAND=$(dirname "$FOUND")
fi
[ -z "$CAND" ] && die "找不到 agent.py。先确认文件已传到手机，或用 --src /完整/路径 指定。"
ok "源目录：$CAND"

# ---------------------------------------------------------------- 2. 拷进家目录
DEST="$HOME/node-agent"
if [ "$CAND" = "$DEST" ]; then
  ok "已在 $DEST，跳过复制"
elif [ -f "$DEST/agent.py" ]; then
  warn "$DEST 已存在，保留原有 state/ 和 logs/（节点身份不丢）"
  warn "要更新代码请手动执行：cp -r '$CAND'/.' $DEST'/"
else
  mkdir -p "$DEST"
  cp -r "$CAND"/. "$DEST"/ || die "复制失败"
  ok "已复制到 $DEST"
fi

# ---------------------------------------------------------------- 3. Python 与依赖
log "检查 Python"
if ! command -v python3 >/dev/null 2>&1; then
  warn "未找到 python3，尝试安装（首次较慢）"
  pkg update -y >/dev/null 2>&1 || true
  pkg install -y python >/dev/null 2>&1 || die "Python 安装失败，手动执行：pkg install python"
fi
ok "python3 $(python3 -c 'import sys;print(sys.version.split()[0])')"

log "安装 Python 依赖"
python3 -m pip install --quiet websockets httpx 2>/dev/null \
  || python3 -m pip install --quiet -i https://pypi.tuna.tsinghua.edu.cn/simple websockets httpx 2>/dev/null \
  || die "依赖安装失败，手动执行：python3 -m pip install websockets httpx"
ok "websockets / httpx 就绪"

# ---------------------------------------------------------------- 4. 写配置
log "写入配置"
cd "$DEST" || die "无法进入 $DEST"
[ -f config.toml ] || die "config.toml 缺失，说明复制不完整"
sed -i "s|^name = .*|name = \"$NAME\"|" config.toml
sed -i "s|^token = .*|token = \"$TOKEN\"|" config.toml
if [ -n "$PLATFORM" ]; then
  sed -i "s|^platform_url = .*|platform_url = \"$PLATFORM\"|" config.toml
fi
ok "name=$NAME  token=$TOKEN  平台=${PLATFORM:-UDP 自动发现}"

# ---------------------------------------------------------------- 5. 启动
log "启动 Agent"
mkdir -p "$DEST/logs"
if [ "$NO_KILL" = "0" ] && pgrep -f agent.py >/dev/null 2>&1; then
  warn "检测到已有 agent.py，先停掉"
  pkill -f agent.py >/dev/null 2>&1 || true
  sleep 1
fi
nohup python3 agent.py >> "$DEST/logs/nohup.log" 2>&1 &
sleep 5

# ---------------------------------------------------------------- 6. 验证
log "验证"
PID=$(pgrep -f agent.py 2>/dev/null | head -n 1)
if [ -n "$PID" ]; then
  ok "Agent 运行中 PID=$PID"
else
  die "Agent 没起来。排查：tail -n 50 $DEST/logs/nohup.log"
fi

LOG="$DEST/logs/$NAME.log"
[ -f "$LOG" ] || LOG=$(ls -t "$DEST"/logs/*.log 2>/dev/null | grep -v nohup | head -n 1)
if [ -n "$LOG" ]; then
  echo
  echo "--- 最近日志（$LOG）---"
  tail -n 15 "$LOG"
  echo "------------------------"
  if grep -q "平台确认注册成功" "$LOG" 2>/dev/null; then
    ok "平台已确认注册 —— 接入成功"
  else
    warn "还没出现「平台确认注册成功」。等 10 秒再看：tail -f $LOG"
  fi
fi

if [ -n "$PLATFORM" ]; then
  echo
  HEALTH=$(curl -s --max-time 5 "$PLATFORM/api/v1/system/health" 2>/dev/null)
  if [ -n "$HEALTH" ]; then ok "平台可达：$HEALTH"; else warn "平台 health 无响应（可能 IP 变了或未放行端口）"; fi
fi

if command -v termux-wake-lock >/dev/null 2>&1; then
  termux-wake-lock >/dev/null 2>&1 && ok "已申请唤醒锁（防息屏挂起）"
else
  warn "termux-wake-lock 不可用（需 pkg install termux-api + Termux:API 应用），不影响运行"
fi

cat <<EOF

======================================================================
 常用命令
======================================================================
 看日志   : tail -f $LOG
 看进程   : pgrep -f agent.py
 停 Agent : pkill -f agent.py
 重启     : cd $DEST && nohup python3 agent.py >> logs/nohup.log 2>&1 &

 ⚠️ 还有一步必须手动做，否则息屏后会被系统杀掉：
   1) 设置 → 应用 → Termux → 省电策略 → 无限制
   2) 允许自启动
   3) 最近任务列表里给 Termux「加锁」
======================================================================
EOF
