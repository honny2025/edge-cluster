# 部署文档（PC 平台 + 手机 Termux 真机）

本文覆盖两条部署路径：① PC/Linux 上跑平台服务端；② Android 手机上跑节点 Agent。

---

## 一、平台服务端部署（PC / Linux）

### 1.1 环境要求

| 项 | 要求 |
|---|---|
| 操作系统 | Linux 优先；Windows 需 WSL 或直接原生运行（本项目已在 Windows 原生验证） |
| Python | 3.10+（本机实测 3.13.14） |
| 内存/磁盘 | 2GB / 10GB 起（模型文件另算） |
| 网络 | 与手机处于**同一局域网**，且能接收 UDP 广播（同一网段、无 AP 隔离） |

### 1.2 安装与启动

```bash
cd server
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt      # Windows: .venv\Scripts\pip
python3 run.py
```

启动后：

```
平台地址: http://0.0.0.0:8000
接口文档: http://127.0.0.1:8000/docs
共享token: edge-cluster-dev-token
发现端口: 37020 (UDP)
数据目录: data/edge_cluster.db
```

### 1.3 配置项（`server/config.toml`）

所有可调项，**默认值开箱即用**；也可用环境变量覆盖（`EDGE_<段>__<键>`）。

```toml
[server]
host = "0.0.0.0"
port = 8000
token = "edge-cluster-dev-token"   # 必须与 Agent 一致
discovery_port = 37020
offline_threshold = 15             # 心跳超时阈值（秒）
offline_confirm_times = 3          # 防抖：连续 N 次巡检超时才标 offline
heartbeat_interval = 5             # 巡检周期（秒）
service_probe_interval = 15        # 服务探活周期（秒）
metric_retention_days = 7          # 指标保留天数
log_level = "INFO"

[data]
db_path = "data/edge_cluster.db"
models_dir = "data/models"
logs_dir = "data/logs"

[web]
serve_static = true                # 托管 web/dist（单端口交付）
```

环境变量示例：

```bash
EDGE_SERVER__PORT=9000 EDGE_SERVER__TOKEN=my-secret python3 run.py
```

### 1.4 生产化运行（Linux systemd，可选）

```ini
# /etc/systemd/system/edge-cluster.service
[Unit]
Description=Edge Cluster Platform
After=network.target

[Service]
Type=simple
User=edge
WorkingDirectory=/opt/edge-cluster/server
ExecStart=/opt/edge-cluster/server/.venv/bin/python run.py
Restart=always
RestartSec=5
Environment=NO_PROXY=127.0.0.1,localhost

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now edge-cluster
sudo journalctl -u edge-cluster -f
```

### 1.5 防火墙

```bash
# Linux (ufw)
sudo ufw allow 8000/tcp      # REST + WebSocket
sudo ufw allow 37020/udp     # 自动发现

# Linux (firewalld)
sudo firewall-cmd --permanent --add-port=8000/tcp
sudo firewall-cmd --permanent --add-port=37020/udp
sudo firewall-cmd --reload

# Windows（管理员 PowerShell）
New-NetFirewallRule -DisplayName "EdgeCluster HTTP" -Direction Inbound -Protocol TCP -LocalPort 8000 -Action Allow
New-NetFirewallRule -DisplayName "EdgeCluster Discovery" -Direction Inbound -Protocol UDP -LocalPort 37020 -Action Allow
```

### 1.6 前端控制台

**方式 A：单端口交付（推荐）**

```bash
cd web
npm install
npm run build          # 产出 web/dist
# 平台启动后自动托管 → 直接访问 http://<平台IP>:8000
```

**方式 B：开发态（热更新）**

```bash
cd web
npm run dev            # http://127.0.0.1:5173，已配好 /api 与 /ws 代理
```

---

## 二、手机端 Agent 部署（Android · Termux · 免 root）

### 2.1 前置准备（手机侧）

1. 安装 **Termux**（建议从 F-Droid 安装，Google Play 版本已停止维护）
2. 可选：安装 **Termux:API**（读电池温度需要）
3. 可选：安装 **Termux:Boot**（开机自启需要，安装后需手动打开一次以激活）

### 2.2 拷贝 Agent 到手机

任选一种：

```bash
# 方式 A：git（手机能访问你的仓库时）
pkg install git
git clone <你的仓库地址> ~/edge-cluster

# 方式 B：从 PC 推送（PC 上执行）
adb push edge-cluster/node-agent /sdcard/Download/
# 然后在 Termux 里：
termux-setup-storage
cp -r /sdcard/Download/node-agent ~/edge-cluster
```

### 2.3 一键安装

```bash
cd ~/edge-cluster
bash install.sh --token edge-cluster-dev-token --platform http://192.168.1.100:8000
```

脚本会自动完成：装 Python 与依赖 → 写配置 → 申请存储/唤醒锁 → 写保活片段 →
装开机自启 → 启动 Agent 并打印最近日志。

**不指定 `--platform`** 时走 UDP 广播自动发现（要求与平台同网段）。

### 2.4 验证

```bash
tail -f ~/edge-cluster/logs/agent.log     # 应看到 "平台确认注册成功"
pgrep -f agent.py                          # 应有一个 PID
```

平台上打开控制台，应出现这台手机，状态为**在线**。

### 2.5 保活配置（**关键，不做会被杀**）

| 步骤 | 位置 | 说明 |
|---|---|---|
| 1 | 设置 → 应用 → Termux → 省电策略 | 选「无限制」/「允许后台活动」 |
| 2 | 设置 → 应用 → Termux → 允许自启动 | 打开 |
| 3 | 最近任务列表 → Termux | 下拉「加锁」，防止一键清理误杀 |
| 4 | Termux 内 | `termux-wake-lock`（install.sh 已自动执行） |

### 2.6 系统限制说明（**务必知悉**）

免 root 方案存在以下**固有**限制，无法通过配置绕过：

1. **不是真正的 Android Service**：进程优先级依赖 Termux 应用本身，系统在极端内存压力下仍可能回收。
2. **厂商省电策略**：MIUI / EMUI / ColorOS / FuntouchOS / OneUI 等国产/定制 ROM 后台管理激进，
   必须在上述第 1、2 步手动放行，否则息屏几分钟内会被冻结。
3. **Doze 模式**：深度休眠时网络会被切断，心跳中断 → 平台判定离线，
   醒来后 Agent 会自动重连恢复（这是预期行为，不是故障）。
4. **termux-wake-lock 的作用边界**：它通过持续持有一个唤醒锁显著提升存活率，
   但**不能 100% 保证进程不被杀**。
5. **需要 root 才能获得的保证**（本项目明确不做）：把 Agent 做成系统服务、
   `am start-foreground-service` 常驻通知栏、修改 `oom_adj` 优先级。

### 2.7 多台手机部署

每台手机只需重复 2.2–2.3。`node_id` 首次启动自动生成并保存在
`node-agent/state/by-name-<name>.json`，**重启后稳定复用**，不会在平台上产生重复节点。

建议给每台手机起可识别的名字：

```bash
# 手机 A
python3 agent.py --name xiaomi-13-lab
# 手机 B
python3 agent.py --name redmi-k60-desk
```

---

## 三、常见问题

| 现象 | 原因 | 解决 |
|---|---|---|
| Agent 报 `proxy rejected connection: HTTP 502` | 本机设了 `HTTP_PROXY`，websockets 走了代理 | 代码已 `proxy=None`；仍出现则设 `NO_PROXY=127.0.0.1,localhost` |
| UDP 发现失败 | 平台未启动 / 不同网段 / AP 隔离 / 防火墙拦 UDP | 检查同网段；或直接 `--platform http://<平台IP>:8000` |
| 平台收不到心跳 | token 不一致 | 比对 `server/config.toml` 与 `node-agent/config.toml` 的 `token` |
| 节点频繁上下线闪烁 | 网络抖动或手机进入 Doze | 调大 `offline_threshold` 与 `offline_confirm_times` |
| 控制台打开提示"前端未构建" | 未执行 `npm run build` | 构建后刷新；或 `npm run dev` 走 5173 |
| 指标显示"—" | 该设备不暴露对应接口（如功耗、NPU） | 这是**预期行为**：拿不到就留空，不造假 |
| 手机息屏后节点离线 | 系统 Doze / 省电策略 | 按 2.5 完成保活配置；仍无法完全避免 |

---

## 四、验证清单（部署后自查）

```bash
# 1) 平台存活
curl -s http://127.0.0.1:8000/api/v1/system/health

# 2) 节点列表（需带 token）
curl -s -H "X-Auth-Token: edge-cluster-dev-token" http://127.0.0.1:8000/api/v1/nodes

# 3) 自动验收
python3 docs/acceptance/m1_check.py --phase online
```

逐项对照：节点状态为 `online`、`last_heartbeat` 在 5s 内、指标有值、
`power_watts` 为 `null`（若设备不支持）。
