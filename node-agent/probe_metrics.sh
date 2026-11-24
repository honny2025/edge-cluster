#!/data/data/com.termux/files/usr/bin/bash
# ============================================================================
#  指标可行性探针 —— 在手机上跑，回答一个问题：
#  「这台机器（Android + Termux，免 root）到底能拿到哪些指标？」
#
#  用法（Termux 里）：
#      bash ~/probe_metrics.sh
#
#  它只读不写、不改任何配置，可以放心跑。
#  输出请整段贴回给我，我据此决定 Agent 的采集降级链怎么写。
# ============================================================================

OK=0; FAIL=0

# 探测一条路径：能读且非空 → OK，并打印首行内容
probe_file() {
  local label="$1" path="$2" max="${3:-120}"
  local first
  if [ ! -e "$path" ]; then
    printf '  [NO ] %-38s 不存在\n' "$label"; FAIL=$((FAIL+1)); return
  fi
  if ! first=$(head -c "$max" "$path" 2>&1); then
    printf '  [ERR] %-38s %s\n' "$label" "$(echo "$first" | head -1)"; FAIL=$((FAIL+1)); return
  fi
  if [ -z "$first" ]; then
    printf '  [EMP] %-38s 存在但为空\n' "$label"; FAIL=$((FAIL+1)); return
  fi
  printf '  [OK ] %-38s %s\n' "$label" "$(echo "$first" | tr '\n' ' ' | cut -c1-90)"
  OK=$((OK+1))
}

echo "==================================================================="
echo " 指标可行性探针   $(date '+%F %T')"
echo " 设备: $(getprop ro.product.model 2>/dev/null) / Android $(getprop ro.build.version.release 2>/dev/null)"
echo " 内核: $(uname -r)   Termux: $(uname -m)"
echo "==================================================================="

echo
echo "--- 1. CPU 使用率来源（Android 对 app 通常封锁 /proc 全局文件）---"
probe_file "/proc/stat"            /proc/stat
probe_file "/proc/loadavg"         /proc/loadavg
probe_file "/proc/uptime"          /proc/uptime
probe_file "/proc/pressure/cpu"    /proc/pressure/cpu
probe_file "/proc/self/stat"       /proc/self/stat
probe_file "/proc/self/statm"      /proc/self/statm
echo "  --- dumpsys cpuinfo（需要 DUMP 权限，多半被拒）---"
if ! command -v dumpsys >/dev/null 2>&1; then
  printf '  [ERR] %-38s %s\n' "dumpsys cpuinfo" "dumpsys 不在 PATH（Termux 里属正常）"
  FAIL=$((FAIL+1))
else
  DUMP=$(timeout 6 dumpsys cpuinfo 2>&1 | head -3)
  if echo "$DUMP" | grep -qi "permission\|denied\|not found\|no such file\|exception"; then
    printf '  [ERR] %-38s %s\n' "dumpsys cpuinfo" "$(echo "$DUMP" | head -1)"
    FAIL=$((FAIL+1))
  elif [ -n "$DUMP" ]; then
    printf '  [OK ] %-38s %s\n' "dumpsys cpuinfo" "$(echo "$DUMP" | tr '\n' ' ' | cut -c1-90)"
    OK=$((OK+1))
  else
    printf '  [EMP] %-38s 无输出\n' "dumpsys cpuinfo"; FAIL=$((FAIL+1))
  fi
fi

echo
echo "--- 2. 网络流量来源 ---"
probe_file "/proc/net/dev"         /proc/net/dev
probe_file "/proc/self/net/dev"    /proc/self/net/dev
probe_file "/sys/class/net/wlan0/statistics/rx_bytes" /sys/class/net/wlan0/statistics/rx_bytes
probe_file "/sys/class/net/wlan0/statistics/tx_bytes" /sys/class/net/wlan0/statistics/tx_bytes

echo
echo "--- 3. CPU 频率（用于「频率占比」近似，已确认可读）---"
probe_file "cpu0 scaling_cur_freq" /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq
probe_file "cpu4 scaling_cur_freq" /sys/devices/system/cpu/cpu4/cpufreq/scaling_cur_freq
probe_file "cpu0 cpuinfo_max_freq" /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq
probe_file "cpu4 cpuinfo_max_freq" /sys/devices/system/cpu/cpu4/cpufreq/cpuinfo_max_freq
probe_file "cpu0 频率驻留统计"      /sys/devices/system/cpu/cpu0/cpufreq/stats/time_in_state
echo "  在线核心数: $(ls -d /sys/devices/system/cpu/cpu[0-9]* 2>/dev/null | wc -l)"

echo
echo "--- 4. GPU 使用率（已在你的机器上部分可读）---"
for p in /sys/class/kgsl/kgsl-3d0/gpu_busy_percentage \
         /sys/class/kgsl/kgsl-3d0/gpubusy \
         /sys/class/misc/mali0/device/utilization \
         /sys/kernel/gpu/gpu_busy \
         /sys/class/devfreq/gpufreq/load; do
  probe_file "$(basename "$p")" "$p" 60
done
echo "  --- devfreq 下所有可读 load 节点 ---"
for d in /sys/class/devfreq/*/; do
  [ -e "${d}load" ] && probe_file "$(basename "$d")/load" "${d}load" 40
done

echo
echo "--- 5. NPU ---"
probe_file "/sys/kernel/npu/load"  /sys/kernel/npu/load
probe_file "/sys/class/npu/npu_busy" /sys/class/npu/npu_busy

echo
echo "--- 6. 温度 / 内存 / 存储（已知可用，做基线对照）---"
probe_file "/proc/meminfo"         /proc/meminfo 60
echo "  thermal_zone 可读个数: $(
  n=0
  for z in /sys/class/thermal/thermal_zone*/temp; do
    [ -r "$z" ] && n=$((n+1))
  done
  echo "$n") / $(ls -d /sys/class/thermal/thermal_zone* 2>/dev/null | wc -l)"
echo "  /data 可用: $(df -h /data 2>/dev/null | tail -1)"

echo
echo "--- 7. 可选增强（装了才有，没有不影响）---"
for c in termux-battery-status termux-sensor termux-wifi-connectioninfo termux-api-start; do
  if command -v "$c" >/dev/null 2>&1; then
    printf '  [OK ] %-38s 已安装\n' "$c"; OK=$((OK+1))
  else
    printf '  [NO ] %-38s 未安装\n' "$c"; FAIL=$((FAIL+1))
  fi
done

echo
echo "==================================================================="
echo " 汇总: 可用 $OK 项 / 不可用 $FAIL 项"
echo " 把上面整段输出贴回给我即可。"
echo "==================================================================="
