# 手机边缘计算集群管理平台 · 操作说明书

> **文档版本** v1.0 ｜ **对应代码版本** `0.2.0-m2` ｜ **最后更新** 2026-09-16
> **适用对象**：要部署和使用本平台的人
> **配套文档**：[`README.md`](../README.md)（工程说明）、[`docs/deployment.md`](deployment.md)（部署细节）、
> [`docs/design/requirement-review.md`](design/requirement-review.md)（需求评审与优化建议）
>
> 📖 其他文档见 [文档索引](README.md)

---

## 目录

- [0. 先读这一段：现在能做什么、还不能做什么](#0-先读这一段现在能做什么还不能做什么)
- [1. 这套系统是什么](#1-这套系统是什么)
- [2. 你需要准备什么](#2-你需要准备什么)
- [3. 快速上手：模拟环境跑通闭环](#3-快速上手模拟环境跑通闭环)
- [4. 真机接入：把手机变成计算节点](#4-真机接入把手机变成计算节点)
- [5. 控制台使用手册](#5-控制台使用手册)
- [6. 日常运维操作](#6-日常运维操作)
- [7. 接口直接调用](#7-接口直接调用)
- [8. 完整功能蓝图（尚未实现）](#8-完整功能蓝图尚未实现)
- [9. 注意事项](#9-注意事项)
- [10. 故障排查手册](#10-故障排查手册)
- [11. 附录：配置项与状态枚举速查](#11-附录配置项与状态枚举速查)

---

## 0. 先读这一段：现在能做什么、还不能做什么

**这一章是全文最重要的部分。** 平台是分里程碑交付的，当前只完成了前两个。
如果不先看这张表，你可能会照着一个还没写出来的功能去操作，然后以为是自己配错了。

### 0.1 实现状态总表

对照原始需求逐条列出。**✅ = 现在就能用；⏳ = 规划中，代码尚未实现。**

| 原需求条目 | 能力 | 状态 | 备注 |
|---|---|---|---|
| §2 手机端常驻服务 | Agent 常驻、后台保活、自动重连、异常恢复 | ✅ | 重连退避实测 1→2.4→3.7→8.8→15.2s |
| §2 获取 CPU/内存/存储/温度 | 指标采集（真机 + 模拟） | ✅ | `/proc` → `termux-api` → `sysfs` 三级降级 |
| §2 获取 GPU/NPU | GPU/NPU 使用率采集 | ⚠️ 部分 | 取决于机型是否暴露 sysfs 节点，**多数手机会是空**（见 §9.4） |
| §2 获取功耗 | `power_watts` | ⚠️ 恒为空 | 免 root 下拿不到，这是**设计上的诚实留空**，不是缺陷 |
| §2 管理本机模型文件 | 模型下载/安装/删除/更新 | ⏳ M3 | 目录结构已预留（`node-agent/models/`） |
| §2 执行模型服务启停 | 模型服务生命周期管理 | ⏳ M4 | 配置项已预留（`node-agent/config.toml [service]`） |
| §3 局域网自动发现与连接 | UDP 广播发现（37020） | ✅ | 单播回包 + 源 IP 提示回连地址 |
| §3 在线/离线检测 | 四态状态机 + 双层防抖 | ✅ | 严格区分 never_seen / offline |
| §3 心跳与断线重连 | 心跳 5s + 指数退避重连 | ✅ | |
| §3 P2P / 中继 | 跨公网连接 | ⏳ 未开始 | 连接层已抽象（`core/transport.py`），换实现即可 |
| §4 设备名称/型号/SoC/系统/IP | 设备信息上报 | ✅ | 真机从 `getprop` 读 |
| §4 CPU/GPU/NPU/内存/存储/温度/功耗 | 设备指标 | ✅ / ⚠️ | 见上，GPU/NPU/功耗可能为空 |
| §4 在线状态、运行时长 | status / uptime_seconds | ✅ | 平台重启会强制归零（见 §6.4） |
| §4 当前任务 | 当前任务展示 | ⏳ M3 | 语义需先拆清，见评审建议 O6 |
| §4 当前部署运行的模型 | 节点上的模型服务清单 | ⏳ M4 | |
| §5 模型库上传/删除/版本管理 | 模型库 | ⏳ M3 | |
| §5 模型下发 + 进度 | 下发、sha256、断点续传 | ⏳ M3 | |
| §5 批量部署/更新 | 多节点批量下发 | ⏳ M3 | |
| §6 服务一键启停/重启 | 服务生命周期 | ⏳ M4 | |
| §6 服务日志 | 服务日志查看 | ⏳ M4 | |
| §6 服务资源占用 | 单服务 CPU/内存占用 | ⏳ M4 | |
| §6 一机多服务 | 多服务共存 | ⏳ M4 | **需先做资源准入**，见评审建议 O4 |
| §7 OpenAI 兼容 API | LLM 标准接口 | ⏳ M4 | |
| §7 VLM / 图像识别 / OCR API | 多模态接口 | ⏳ M4 | 由 `service_defs` 配置驱动 |
| §7 Endpoint / 端口 / 调用状态展示 | 服务对外信息 | ⏳ M4 | 存储字段已预留（`services.gateway_meta`） |
| §7 统一 API Gateway 调度 | 请求转发与调度 | ⏳ M4 | **建议提前到 M4 做**，见评审建议 O2 |
| §8 在线/离线节点数量 | 大屏统计 | ✅ | |
| §8 设备资源使用情况 | CPU/内存/存储汇总 | ✅ | 只统计 online 且采样新鲜的节点 |
| §8 当前运行模型数量 | 运行中服务数 | ✅ 接口位就绪 | 数据源需等 M4（现在恒为 0） |
| §8 请求量/吞吐量/延迟 | 服务性能指标 | ⏳ M4 | **必须在网关数据路径上**，见评审建议 O2 |
| §8 温度/功耗/异常告警 | 告警引擎 | ✅ | 阈值全部配置驱动 |
| §8 最近部署任务/异常设备 | 大屏展示 | ✅ 异常设备可用 | 最近部署任务需等 M3（现在恒为空） |
| §9 跨公网/P2P 管理 | 异地组网 | ⏳ 未开始 | 见评审建议 O8 |

### 0.2 一句话总结现状

**现在能用的是"看得见"的部分**：设备自动发现、状态监控、指标采集、历史曲线、告警、大屏。
**还不能用的是"动手"的部分**：下发模型、启动服务、对外提供 API。

换句话说，平台目前是一套**完整的集群监控系统**，还不是一套**模型服务平台**。
后一半能力在里程碑 3、4，是接下来要做的。

---

## 1. 这套系统是什么

### 1.1 一句话定位

把局域网里的 Android 手机当成**边缘计算节点**，用一个平台统一完成：
**发现设备 → 监控状态 → 下发模型 → 启动服务 → 对外提供 API → 持续监控**。

### 1.2 为什么架构是这样（理解这个，你才能理解很多"奇怪"的设计）

最关键的一条：**连接方向是反的**。

```
        ┌──────────────────────────┐
        │   平台（一台常开的电脑）    │
        │   FastAPI + SQLite        │
        │   :8000 TCP  REST/WS      │
        │   :37020 UDP 发现          │
        └───────────┬──────────────┘
                    │
        Agent 主动拨出 │  ← ★ 记住这个方向
                    │
      ┌─────────────┼─────────────┐
      │             │             │
 ┌────▼────┐   ┌────▼────┐   ┌────▼────┐
 │ phone-01 │   │ phone-02 │   │ phone-03 │
 │ Termux   │   │ Termux   │   │ Termux   │
 │ agent.py │   │ agent.py │   │ agent.py │
 └──────────┘   └──────────┘   └──────────┘
```

**平台永远不会主动连手机。** 原因是：手机在 NAT 后面、息屏后会进 Doze、厂商省电策略会挂起进程——
所以"平台去连手机"这条路随时会断。而"手机连平台"是手机主动发起的出向连接，
只要手机活着、网络通着，这个方向就可靠。

由此派生出一系列设计：

| 你可能会觉得奇怪的地方 | 真正的原因 |
|---|---|
| 为什么要 Agent 而不是平台去扫手机 | 平台连不上手机 |
| 为什么大文件走 HTTP 而不是 WebSocket | WS 上跑大量数据会阻塞心跳，HTTP 天然支持断点续传 |
| 为什么手机息屏后节点会"离线" | Doze 切断了网络，Agent 发不出心跳；醒来会自动恢复 |
| 为什么平台重启后所有节点先显示离线 | 平台刚启动时不可能持有连接，显示在线就是假的 |

### 1.3 数据流

```
① 发现阶段（仅首次 / 地址变动时）
   Agent ──UDP 广播 EDGE_DISCOVER──▶ 局域网
   平台 ──UDP 单播 EDGE_DISCOVER_ACK──▶ Agent
          （回包带上平台观察到的源 IP，Agent 用它作为回连地址）

② 运行阶段（建立后长期保持）
   Agent ──WebSocket /ws/agent──▶ 平台
          ├─ register  注册（带设备信息）
          ├─ heartbeat 心跳（每 5s，带指标）
          └─ ◀── 平台下发指令（每条带唯一 cmd_id，Agent 去重）

③ 展示阶段
   平台 ──WebSocket /ws/dashboard──▶ 浏览器控制台（节点上下线实时推送）
   浏览器 ──REST 轮询──▶ 平台（指标、曲线、汇总数据）
```

---

## 2. 你需要准备什么

### 2.1 硬件清单

| 角色 | 要求 | 说明 |
|---|---|---|
| **平台主机** | 一台**常开**的电脑（PC / 迷你主机 / 树莓派 / 旧笔记本 / NAS） | 要一直在线，否则手机会连不上、状态会不停跳。这也是"无人值守"的前提 |
| **计算节点** | 1~N 台 Android 手机 | 建议先 1 台跑通，再扩到多台 |
| **网络** | 路由器的局域网 | **所有设备必须在同一网段**（如都是 192.168.1.x） |

**平台主机的选择建议**：
- 不需要很强，平台本身很轻（1 核 512MB 就能跑）
- 但**磁盘要够**：模型文件会存平台侧，一个 LLM 动辄 4~8GB
- 有线连接优先（WiFi 也能用，但平台侧走有线更稳）
- Windows / Linux / macOS 都可以，本项目在 Windows 原生验证过

**手机的选择建议**：
- **内存越大越好**：跑的模型大小直接受内存限制。8GB 能跑 3B 级，12GB 能跑 7B 量化
- **别用主力机**：Agent 会持续占用一点资源，且长期插电对电池不友好
- **散热要注意**：持续推理会让手机发热降频，最好别盖着东西
- **必须插电**：跑推理的耗电远超你想像，电池撑不了多久

### 2.2 手机侧要装的软件

| 软件 | 用途 | 必要性 | 获取方式 |
|---|---|---|---|
| **Termux** | Linux 终端环境，Agent 的运行载体 | **必需** | **从 F-Droid 装**（Google Play 版本已停止维护，装了会出问题） |
| **Termux:API** | 读电池温度/电量等指标 | 可选 | F-Droid。缺了 Agent 照样跑，相关指标留空 |
| **Termux:Boot** | 开机自动启动 Agent | 可选 | F-Droid。**装完必须手动打开一次**才生效 |

> **注意版本一致性**：Termux 与 Termux:API / Termux:Boot 建议都从 F-Droid 安装。
> 混装（比如 Termux 来自 F-Droid、Termux:API 来自 Play）会因为签名不一致而互相不认。

### 2.3 平台主机侧要装的软件

| 软件 | 版本 | 用途 | 备注 |
|---|---|---|---|
| **Python** | 3.10 或更高 | 平台服务端 + Agent 的运行环境 | 平台与 Agent 共用 |
| **Node.js** | 18 或更高 | 构建 Web 控制台 | **只在构建前端时需要**，构建完可以不装 |

如果你用项目自带的一键脚本（`start_all.ps1` / `start_all.sh`），
脚本会**自动创建隔离的虚拟环境并安装依赖**，不用你手工装 Python 包。

脚本还会自动探测 Python 的位置（环境变量 → 常见安装位置 → PATH）并校验版本，
所以**装在哪儿都行，不需要改脚本**。Python 不在默认位置时也可以显式指定：

```powershell
$env:EDGE_PYTHON = "D:\Python311\python.exe"      # Windows
```

```bash
export PY=/opt/python3.12/bin/python3             # Linux / macOS
```

> 完整的可移植性说明（打包带走、目标机器需要什么、哪些东西故意不带）见 §3.9。

### 2.4 网络条件核查（这一步没做对，后面全是玄学问题）

在动手之前，逐项确认：

| 检查项 | 怎么查 | 不对会怎样 |
|---|---|---|
| 手机和平台**在同一网段** | 看两者 IP 前三段是否一致（如 `192.168.1.x`） | UDP 广播收不到，节点不出现 |
| 路由器**没开 AP 隔离 / 客户端隔离** | 路由器设置里找"AP 隔离""客户端隔离" | 手机和电脑互相看不见 |
| 手机连的是**同一个 WiFi**（不是访客网络） | 手机 WiFi 详情 | 访客网络通常做了隔离 |
| 公司/学校网络？ | 试试能不能互相 ping | 这类网络几乎都做了客户端隔离，**建议换家用路由器** |
| 平台主机防火墙放行 | 见下方命令 | 手机连不上 8000 端口 |

**Windows 放行端口**（管理员 PowerShell）：

```powershell
New-NetFirewallRule -DisplayName "EdgeCluster HTTP" -Direction Inbound -Protocol TCP -LocalPort 8000 -Action Allow
New-NetFirewallRule -DisplayName "EdgeCluster Discovery" -Direction Inbound -Protocol UDP -LocalPort 37020 -Action Allow
```

**Linux 放行端口**：

```bash
sudo ufw allow 8000/tcp
sudo ufw allow 37020/udp
```

### 2.5 心理准备：预期管理（**很重要**）

这几条不是劝退，是为了让你别把正常现象当成故障：

1. **手机息屏后节点可能变离线** —— 这是 Android 的 Doze 机制在断网，不是 Agent 崩了。
   做完 §4.6 的保活配置能大幅改善，但**免 root 方案无法 100% 保证不被杀**。
2. **功耗、NPU 使用率很可能一直是"—"** —— 免 root 拿不到这些数据。
   **平台的选择是留空，而不是编一个数字给你看。**
3. **手机跑大模型的真实速度** —— 骁龙 8 Gen 2 级别的手机跑 7B 量化模型，
   大概每秒几个 token 的量级。它适合做**离线、低频、隐私敏感**的推理，
   不适合替代服务器做高并发服务。把它当"分布式小算力池"，不要当"GPU 集群"。
4. **节点数不必多** —— 这套系统的价值在"统一管理"，3 台手机 + 1 个平台就能体现。
   不要为了凑数去找一堆手机。

---

## 3. 快速上手：模拟环境跑通闭环

**强烈建议先跑模拟环境。** 它不需要任何手机，在本机起两个"假手机"，
让你先看清平台长什么样、数据怎么流，再去折腾真机。

### 3.1 Windows（两种方式，任选）

**方式 A：双击 `启动.bat`（最省事，推荐）**

双击后弹出一个命令行窗口，它会自动完成全部步骤。跑完提示"服务已在后台运行"，
此时**可以直接关掉那个窗口**（平台和节点是独立的后台进程，不受窗口影响）。

停止：双击 `停止.bat`。

> 为什么推荐这个：Windows 默认禁止运行 `.ps1` 脚本，
> 双击 bat 会自动绕过这个限制。详见 §3.7。

**方式 B：命令行**

```powershell
cd edge-cluster
.\start_all.ps1 -BuildWeb          # 起平台 + 2 个模拟节点 + 构建前端
```

> **如果这行报「禁止运行脚本」**，不是系统坏了，是 Windows 的默认安全策略。
> 用下面这条命令一定可以：
>
> ```powershell
> powershell -ExecutionPolicy Bypass -File .\start_all.ps1 -BuildWeb
> ```

脚本会依次：创建隔离虚拟环境 → 安装依赖 → 构建前端 → 启动平台 → 启动 2 个模拟节点。

完成后浏览器打开 **<http://127.0.0.1:8000>**，应该看到两个节点实时在线。

停止：

```powershell
.\stop_all.ps1
```

### 3.2 Linux / macOS / WSL

```bash
cd edge-cluster
BUILD_WEB=1 ./start_all.sh         # 起平台 + 2 个模拟节点 + 构建前端
./stop_all.sh                      # 停止全部
```

> **若从 zip 解压得到**：zip 不保留 Unix 可执行权限，所以要用
> `bash start_all.sh`（而不是 `./start_all.sh`），或先执行
> `chmod +x start_all.sh stop_all.sh`。

### 3.3 一键脚本的其他用法

```powershell
.\start_all.ps1 -Nodes 5           # 起 5 个模拟节点（看多节点效果）
.\start_all.ps1 -NoNodes           # 只起平台，不起模拟节点
.\start_all.ps1 -BuildWeb          # 附带构建前端
.\start_all.ps1 -DevWeb            # 额外起 Vite 开发服务器（改前端代码用，热更新）
.\start_all.ps1 -Port 8001         # 换平台端口（模拟节点会自动连新端口）
```

Linux / macOS 用环境变量，能力一一对应：

```bash
NODES=5    ./start_all.sh          # 起 5 个模拟节点
NO_NODES=1 ./start_all.sh          # 只起平台
BUILD_WEB=1 ./start_all.sh         # 附带构建前端
PORT=8001  ./start_all.sh          # 换平台端口
```

### 3.4 手工分步启动（推荐用于调试）

一键脚本把日志藏起来了，调试时建议手工起，能看到实时输出：

```bash
# 终端 1：平台
cd server
python -m venv .venv && .venv/bin/pip install -r requirements.txt   # Windows: .venv\Scripts\pip
python run.py
# → 平台 http://127.0.0.1:8000    接口文档 http://127.0.0.1:8000/docs

# 终端 2：模拟节点 1
cd node-agent
pip install -r requirements.txt
python agent.py --simulate --name phone-01 --platform-url http://127.0.0.1:8000

# 终端 3：模拟节点 2
python agent.py --simulate --name phone-02 --platform-url http://127.0.0.1:8000
```

> **`--name` 必须每个实例不同。** 同名会让两个实例争用同一个身份，互相顶掉连接。

### 3.5 验证是否成功

```bash
# 跑一遍自动验收（共 11 项）
python docs/acceptance/m1_check.py --phase online

# 里程碑 2 的验收（共 17 项）
python docs/acceptance/m2_check.py --phase online
```

**期望输出**：`验收结果：11/11 通过` 和 `验收结果：17/17 通过`。

还有一个不需要启动平台、秒级完成的自检（8 项），专门盯**配置层**：

```bash
python docs/acceptance/config_check.py
# → 验收结果：8/8 通过
```

它验证的是"环境变量覆盖配置文件"这条路径。这条路径曾经因为一个前缀截断的
笔误而**完全静默失效**（表现为 `run.py --port` 不起作用），所以单独钉了一个脚本防回归。

> 换了端口的话，前两个验收脚本要带 `--base`，例如
> `python docs/acceptance/m1_check.py --base http://127.0.0.1:8001 --phase online`。

### 3.6 手动确认一遍（比脚本更直观）

```bash
curl -s http://127.0.0.1:8000/api/v1/system/health
# → {"status":"ok","version":"0.2.0-m2",...}

curl -s -H "X-Auth-Token: edge-cluster-dev-token" http://127.0.0.1:8000/api/v1/nodes
# → 应看到 phone-01 / phone-02，status 都是 online
```

### 3.7 Windows 提示"禁止运行脚本"怎么办

**现象**

```
无法加载文件 ...\start_all.ps1，因为在此系统上禁止运行脚本。
```

或者更隐蔽的一种：**命令执行了，但什么都没发生、一点输出都没有**。
某些自动化/非交互环境会**静默拦截** .ps1，连错误都不报——看起来像"脚本没反应"。

**原因**

Windows PowerShell 的默认执行策略是 `Restricted`（禁止运行任何 .ps1 文件）。
这是操作系统的安全设计，不是项目或系统的问题。

查看当前策略：

```powershell
Get-ExecutionPolicy -List
```

若全是 `Undefined`，实际生效的就是默认的 `Restricted`。

**三种解法，任选一种**

| 方式 | 怎么做 | 适用场景 |
|---|---|---|
| ① 双击 bat | 双击 `启动.bat` / `停止.bat` | **最省事**，推荐 |
| ② 单次绕过 | `powershell -ExecutionPolicy Bypass -File .\start_all.ps1` | 不想改系统设置 |
| ③ 放开当前用户 | `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` | 想以后直接 `.\start_all.ps1` |

方式 ③ 只影响当前用户、**不需要管理员权限**，且仅放开本地脚本
（从网上下载的、带签名的仍按原规则）。改完可以用 `Get-ExecutionPolicy -List` 复核。

> `启动.bat` 内部做的就是方式 ②，所以双击它永远可用。

### 3.8 端口 8000 被占用 / 想换端口

```powershell
.\start_all.ps1 -Port 8001          # Windows
PORT=8001 bash start_all.sh         # Linux / macOS
```

模拟节点会自动连到新端口，**不需要改任何配置文件** ——
脚本启动节点时会显式把 `--platform-url` 设成实际地址。

脚本在启动平台**之前**会检查端口占用，被占用时直接明确报错，不会让你白等。

> 如果报"端口已被占用"但平台其实已经在跑，那**不用再启动一次**，
> 直接打开 `http://127.0.0.1:8000` 即可（换过端口就用你换的那个）。

**两个例外要留意**（`-Port` 只作用于一键脚本）：

| 场景 | 需要怎么做 |
|---|---|
| 手工 `python run.py` | 加 `--port 8001`，或改 `server/config.toml` 的 `port` |
| 验收脚本 | 加 `--base http://127.0.0.1:8001` |

### 3.9 搬到另一台电脑跑（可移植性）

整套系统就是 `edge-cluster/` 这一个文件夹，**没有依赖任何本机路径**。

**打包（在原机器上，改过代码后执行一次）**

```bash
python tools/pack_portable.py
# → edge-cluster-portable-<版本>.zip（约 0.9 MB，85 个文件）
```

**在新机器上**

1. 解压 zip
2. Windows 双击 `启动.bat`；Linux / macOS 执行 `bash start_all.sh`
3. 浏览器打开 `http://127.0.0.1:8000`

**目标机器需要什么**

| 需要 | 说明 |
|---|---|
| Python 3.10+ | **唯一硬性要求**。脚本会自动探测（先看 `EDGE_PYTHON` / `PY`，再找常见安装位置，最后看 PATH），并校验版本 |
| 联网（仅首次） | 装一次依赖；之后可以完全离线跑 |
| Node.js 18+ | **不需要**。控制台构建产物 `web/dist` 已打进包里。只有你要改前端代码时才需要 |

**一键脚本在"陌生机器"上会自动做的事**

1. 探测可用 Python 并校验 ≥ 3.10，不达标直接告诉你不达标在哪
2. 在**项目内**建 `.venv` 装依赖 —— 已存在的 `.venv` 会先体检，若它是从别的机器/别的系统拷来的、指向一个本机不存在的解释器（此时 venv 的 python 会以退出码 103 直接启动失败），脚本会把它**改名保留**为 `.venv.broken-<时间戳>` 然后重建，不会让你卡在一个坏环境上，也不会让你丢失排查线索
3. `pip` 默认源装不动时自动改用清华镜像重试
4. 启动前检查端口占用
5. 清空本地代理变量，否则 Agent 连 `127.0.0.1` 会被系统代理以 502 拒绝
6. **不写任何系统目录、不改注册表** —— 卸载 = 删掉这个文件夹

**这个包故意不含什么**（所以"拷走"不会带走原机器的运行数据）

| 不含 | 原因 |
|---|---|
| `.venv/` | 与解释器路径绑定，必须在新机器重建 |
| `web/node_modules/` | 260 MB，与平台无关，需要时 `npm install` 重建 |
| `server/data/` | 原机器的 SQLite 库（节点记录 + 历史指标） |
| `node-agent/state/` | 节点身份文件（`node_id`）——**外传会造成身份冲突** |
| `node-agent/logs/` | 原机器的运行日志 |
| 模型权重（`*.gguf` / `*.onnx` 等） | 体积大，按需单独拷 |

`pack_portable.py` 是**白名单式**收集：默认什么都不收，只收显式列出的东西，
并且在打包后会自检包里没有 node_modules / 数据库 / 节点身份 / 日志。

#### ⚠️ 升级时不要"覆盖解压"（实测会丢文件）

PowerShell 的 `Expand-Archive` 加 `-Force`（或解压到**已存在**的目录）会先清空
目标目录、然后又不写入文件，而且**不报任何错**。

> 实测数据：同一个 zip 解压两次 —— 第一次 85 个文件，第二次之后 **0 个文件**。

**正确做法**

| 方式 | 步骤 |
|---|---|
| ① 推荐 | 解压到**新文件夹** → 把旧目录的 `server/data/` 与 `node-agent/state/` 拷过去 → 删掉旧文件夹 |
| ② 省事 | 确认不需要旧数据后，直接删掉旧文件夹再解压 |

这两个目录**不在 zip 里**，但装着你的运行数据，别当垃圾丢掉：

- `server/data/` —— 平台数据库（节点列表 + 历史指标）
- `node-agent/state/` —— 节点身份文件（保留它，重装后 `node_id` 不变）

> 用 7-Zip 之类逐文件解压的工具一般不会触发这个问题，但仍建议按上面的方式做。


---

## 4. 真机接入：把手机变成计算节点

### 4.1 第一步：确认平台的局域网地址

手机要能连上平台，得知道平台的 IP。

**Windows**：

```powershell
ipconfig | Select-String "IPv4"
```

**Linux**：

```bash
ip addr | grep "inet " | grep -v 127.0.0.1
```

假设查到的是 `192.168.1.100`，下面都用这个地址举例。

> **注意**：平台默认监听 `0.0.0.0`（所有网卡），所以任务书里的 `127.0.0.1` 只适合本机测试，
> 真机接入必须用局域网 IP。用一键脚本起的平台已经是 `0.0.0.0`，不用改。

### 4.2 第二步：统一 token

平台和 Agent 的共享 token 必须一致，默认值都是 `edge-cluster-dev-token`。

**如果你要改（建议改，原因见 §9.3）**，改两处：

```toml
# server/config.toml
token = "你的token"

# node-agent/config.toml
token = "你的token"
```

> 也可以不手改配置文件，Agent 安装时用 `--token` 参数传入。

### 4.3 第三步：在手机上装 Termux

1. 从 **F-Droid** 安装 Termux（不要用 Google Play 版本）
2. 打开 Termux，先跑一次更新：

```bash
pkg update && pkg upgrade -y
```

3. （可选）装 Termux:API 和 Termux:Boot：
   - 从 F-Droid 装这两个 App
   - **Termux:Boot 装完必须手动打开一次**，否则开机自启不生效

### 4.4 第四步：把 Agent 拷到手机

任选一种方式。

**方式 A：直接从 PC 推送（推荐，不用掏手机操作）**

```bash
# PC 上执行（需要 adb，手机需开 USB 调试）
adb push edge-cluster/node-agent /sdcard/Download/
```

```bash
# 然后在手机的 Termux 里
termux-setup-storage          # 会弹权限对话框，点允许
cp -r /sdcard/Download/node-agent ~/edge-cluster
```

**方式 B：用文件管理器传**

把 `node-agent` 整个目录压缩后传到手机（微信/QQ/网盘都行），
用手机文件管理器解压到 `/sdcard/Download/`，再在 Termux 里 `cp` 过去。

**方式 C：git（手机能访问你的仓库时）**

```bash
pkg install git
git clone <你的仓库地址> ~/edge-cluster
```

> **只需拷 `node-agent` 这一个目录**，别的都不用。它已经被设计成自包含的。

### 4.5 第五步：一键安装

```bash
cd ~/edge-cluster
bash install.sh --token edge-cluster-dev-token --platform http://192.168.1.100:8000
```

这个脚本会自动完成：

1. 安装 Python 及可选依赖
2. 安装 Agent 的 Python 依赖（`websockets`、`httpx`）
3. 写入 token 与平台地址到 `config.toml`
4. 申请存储权限（会弹系统对话框，**记得点允许**）
5. 申请 `termux-wake-lock` 唤醒锁
6. 往 `~/.bashrc` 写保活片段（每次进 Termux 自动拉起 Agent）
7. 安装开机自启脚本到 `~/.termux/boot/`
8. 启动 Agent，并打印最近几行日志

**如果你不传 `--platform`**，Agent 会走 UDP 广播自动发现平台（要求同网段）：

```bash
bash install.sh --token edge-cluster-dev-token
```

**如果你不想装开机自启**：

```bash
bash install.sh --no-boot
```

### 4.6 第六步：保活配置（**不做必被杀**）

这是免 root 方案里最关键、也最容易被忽略的一步。**逐项做完**：

| # | 操作位置 | 具体动作 |
|---|---|---|
| 1 | 系统设置 → 应用 → Termux → 省电策略 | 选「**无限制**」或「**允许后台活动**」 |
| 2 | 系统设置 → 应用 → Termux → 允许自启动 | 打开 |
| 3 | 最近任务列表 → 找到 Termux | 下拉「**加锁**」（防止一键清理时被杀） |
| 4 | Termux 内部 | `termux-wake-lock`（install.sh 已自动执行，可手动复查） |

**各品牌 ROM 的额外注意**：

| 品牌 | 额外要做的事 |
|---|---|
| MIUI / HyperOS | 设置 → 应用设置 → Termux → 省电策略 → 无限制；且需在"后台弹出界面"或"锁屏显示"中允许 |
| EMUI / HarmonyOS | 设置 → 应用启动管理 → Termux → 关闭"自动管理"，手动全开三个开关 |
| ColorOS / realme UI | 设置 → 电池 → 应用耗电管理 → Termux → 允许完全后台行为 |
| FuntouchOS / OriginOS | 设置 → 电池 → 后台耗电管理 → Termux → 允许后台高耗电 |
| OneUI | 设置 → 电池 → 后台使用限制 → 从"休眠应用"中移除 Termux |

> **做完这些仍然可能被杀。** 这是免 root 方案的固有上限，不是配置没做对。
> 真要 100% 常驻，需要 root 后做成系统服务——本项目明确不做这条路。

### 4.7 第七步：验证接入成功

**在手机上**：

```bash
tail -f ~/edge-cluster/logs/agent.log     # 应看到"平台确认注册成功"之类
pgrep -f agent.py                          # 应输出一个 PID
```

**在平台上**：浏览器打开控制台，`节点` 页面应出现这台手机，状态为 **在线**（绿色圆点）。

**如果没出现**，按顺序排查：

1. 手机能不能 ping 通平台？`ping 192.168.1.100`
2. 手机能不能访问平台端口？`curl http://192.168.1.100:8000/api/v1/system/health`
3. Agent 日志里有没有报错？常见的是 token 不一致、连接被拒
4. 平台日志里有没有"鉴权失败"？ → token 不一致
5. 都通但节点不出现？ → 看平台日志是否收到 register

### 4.8 多台手机

每台手机重复 §4.3 ~ §4.7 即可。两件事要注意：

**① 每台手机要有不同的名字。** 建议起可识别的名字：

```bash
python3 agent.py --name xiaomi-13-lab
python3 agent.py --name redmi-k60-desk
```

**② `node_id` 会自动生成并持久化**，存在 `node-agent/state/by-name-<name>.json`。
重启后**复用同一个 node_id**，不会在平台上产生重复节点。你不需要手工管理它。

---

## 5. 控制台使用手册

控制台地址：**<http://127.0.0.1:8000>**（或 `http://<平台IP>:8000`）

界面是深色主题，左侧两个菜单：**总览** 和 **节点**。

### 5.1 顶部状态栏

| 元素 | 含义 |
|---|---|
| 左上角版本号 | 平台当前版本（如 `v0.2.0-m2`） |
| 实时通道 | 浏览器 ↔ 平台的 WebSocket 状态。**"已连接"时节点上下线会实时推送**；显示"重连中"说明平台断了或网络抖动 |
| 在线 / 离线 / 从未上线 | 三态计数。**注意"从未上线"是独立一类**，不是"离线" |
| ⟳ 刷新图标 | 手动刷新平台信息 |

> **"从未上线"和"离线"的区别很重要**：前者是这台设备从来没上报过（可能刚加进来、或配置错了）；
> 后者是曾经上报过、现在断联了。混为一谈会让你排查错方向。

### 5.2 总览页（大屏）

首页是监控大屏，从上到下共 6 个区块：

**① 6 张 KPI 卡**

| 卡片 | 含义 | 怎么读 |
|---|---|---|
| 节点总数 | 平台上登记的全部节点 | |
| 离线节点 | 当前离线的数量 | 大于 0 时卡片会变色 |
| 运行中服务 | 正在运行的模型服务数 | **M4 前恒为 0**（服务管理还没做） |
| CPU 平均 | 在线节点 CPU 使用率均值 | 离线节点**不计入** |
| 内存池 | 所有在线节点的内存累加（已用/总量） | |
| 存储池 | 所有在线节点的存储累加（已用/总量） | |

> **关键理解**：所有汇总值**只统计"在线且采样新鲜"的节点**。
> 一台离线手机的旧数据不会被算进来——这是刻意设计的，避免用陈旧数据骗你。

**② 使用率条**
集群内存、存储的整体使用率。超过告警阈值时转暖色。

**③ 告警面板**
按 `critical`（红，严重）/ `warning`（橙，警告）分级着色。当前无告警时会明确显示"当前无告警"。
每条告警包含：级别、类型、节点、当前值、阈值。

**④ 集群资源趋势图**
近 15 分钟 ~ 24 小时的 CPU / GPU / NPU 曲线（所有在线节点的均值）。右上角可切换时间范围。

**⑤ 异常节点列表**
离线节点和"从未接入"节点**分开说明**，文案不同。

**⑥ 节点实时列表**
带 CPU / 内存 / 存储 / 温度列的表格。**离线行会压暗，指标列显示「—」**。

> **看图最重要的一条**：曲线出现**断点**，表示那一刻采集不到该项数据（比如手机不暴露 NPU 使用率），
> 表示"**无数据**"，**不等于数值是 0**。
> 这是刻意做成断点的——如果把没数据的地方补 0，那就是把"没有测量"伪装成"测量结果是零"。

### 5.3 节点页

节点列表，支持按状态过滤（全部 / 在线 / 离线 / 从未上线）。

每行显示设备名称、型号、SoC、Android 版本、IP、状态、最后心跳时间、运行时长。
点进去是**节点详情页**。

**节点行上的操作**：

| 操作 | 用途 |
|---|---|
| 查看详情 | 进入详情页看指标曲线 |
| 改名 | 给节点起个有意义的名字（比如按位置/用途） |
| 删除 | 从平台移除该节点记录 |

> **删除节点**只是删掉平台侧的记录。手机的 Agent 还在跑，下次心跳会**重新注册**、
> 节点又会出现（带新的或原有的 node_id）。要彻底移除，得先在手机上停掉 Agent。

### 5.4 节点详情页

**① 当前指标卡片组**
CPU / GPU / NPU / 内存 / 存储 / 温度 / 网络 / 负载。
**拿不到的项显示"—"并置灰**，这是真实的"没数据"，不是加载中。

**② 历史曲线**（多张图）

| 曲线 | 内容 |
|---|---|
| CPU / GPU / NPU | 三项使用率百分比 |
| 内存 | 使用率 |
| 存储 | 使用率 |
| 温度 | ℃ |
| 网络 | 收发速率 |

右上角可切换时间范围：**15 分钟 / 1 小时 / 6 小时 / 24 小时**。

**③ 离线节点的行为**
节点离线后，详情页会明确标注**离线**，曲线**停止刷新**，
且**不会把旧值当作当前值展示**。历史曲线仍然可以查（历史数据保留）。

---

## 6. 日常运维操作

### 6.1 启停服务

```powershell
# Windows — 一键起停
.\start_all.ps1 -NoNodes -BuildWeb    # 只起平台
.\stop_all.ps1                        # 停全部
```

```bash
# Linux
./start_all.sh
./stop_all.sh
```

进程 PID 会写在项目根目录的 `.running.pids` 里，`stop_all` 靠它精确回收。

**手工停平台**（一键脚本出问题时）：

```powershell
# Windows：精确杀掉平台进程
Get-CimInstance Win32_Process -Filter "name='python.exe'" |
  Where-Object { $_.CommandLine -like '*edge-cluster*' -and $_.CommandLine -like '*run.py*' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
```

> **⚠️ 这是最容易踩的坑**：用任务管理器式的"结束任务"、或者停止外层壳脚本，
> 常常**只杀掉包装 shell，Python 子进程会活着继续发心跳**。
> 结果就是你明明"关掉"了节点，平台上它还一直在线。
> **一定要按 CommandLine 精确匹配后再杀。**

### 6.2 查看日志

| 日志 | 位置 |
|---|---|
| 平台日志 | `server/data/logs/` 下的轮转日志文件（5MB 轮转） |
| 平台控制台输出 | `server/data/logs/server.console.log`（一键脚本起的才有） |
| 节点 Agent 日志 | 平台控制台 → 节点 → 查看日志；或手机上的 `~/edge-cluster/logs/agent.log` |
| 模拟节点日志 | `node-agent/logs/phone-0X.console.log` |

平台日志也可以在控制台里直接看，或通过接口：

```bash
curl -s -H "X-Auth-Token: edge-cluster-dev-token" \
  "http://127.0.0.1:8000/api/v1/system/server-logs?lines=100"
```

### 6.3 调整告警阈值

改 `server/config.toml` 的 `[alerts]` 段，然后**重启平台**：

```toml
[alerts]
enabled = true
cpu_usage_warn = 85.0            # CPU 使用率 % ≥ 此值告警
mem_usage_warn = 85.0            # 内存使用率 % ≥ 此值告警
storage_usage_warn = 90.0        # 存储使用率 % ≥ 此值告警
temperature_warn = 45.0          # 温度 ℃ ≥ 此值告警
temperature_critical = 55.0      # 温度 ℃ ≥ 此值严重告警
alarm_on_offline = true          # 节点离线是否告警
```

**两条内置规则**（不需要配，但要知道）：
- 指标取不到（`null`）时**一律不告警** —— 没有数据不等于异常
- 节点"从未上报"**不报离线告警** —— 与"离线"严格区分

### 6.4 调整离线判定灵敏度

这是最常需要调的地方。相关配置在 `server/config.toml` 的 `[server]` 段：

```toml
offline_threshold = 15        # 秒：心跳超时多久算离线
offline_confirm_times = 3     # 防抖：连续 N 次巡检都超时才标记 offline
heartbeat_interval = 5        # 秒：巡检周期
```

**实际离线耗时的算法**：

```
实际耗时 ≈ offline_threshold + offline_confirm_times × heartbeat_interval
默认：15 + 3×5 = 30s（实测约 20~25s，取决于巡检相位）
```

| 你的场景 | 建议配置 | 代价 |
|---|---|---|
| 想尽快发现离线（任务书写 15s） | `offline_confirm_times = 1` | 网络抖动时状态可能闪烁 |
| 手机 WiFi 不稳，老误报 | `offline_threshold = 30`，`offline_confirm_times = 3` | 掉线发现慢 |
| 默认（推荐） | 不改 | 取中间的平衡 |

> 为什么默认不是严格 15s：因为任务书同时要求"15s 内标记离线"和"状态机防抖"，
> 这两条本身有张力。当前选择偏向防抖（避免误报）。要严格对齐就改 `1`。
> 更彻底的解法见评审建议 O9（双阈值）。

### 6.5 平台重启后的预期行为

**平台重启时，所有节点会被强制标记为 `offline`。** 这是刻意的，不是 bug。

理由：平台刚启动时不可能持有任何连接，如果沿用数据库里旧的 `online` 状态和 `uptime`，
那就是**用旧数据冒充在线**。

**实测正确表现**：平台重启 → 节点全部显示离线 → Agent 自动重连 → 状态恢复 `online`，
且 `uptime_seconds` **从重连时刻重新计时**（实测 18s / 16s，而不是累积的历史值）。

> 修复这个行为之前，曾出现过平台重启后手机显示 `uptime = 39734s`（约 11 小时）的假在线，
> 而那只手机的 Agent 早就随关机没了。现在已修。

### 6.6 删除节点记录

控制台节点页可直接删除，或用接口：

```bash
curl -X DELETE -H "X-Auth-Token: edge-cluster-dev-token" \
  http://127.0.0.1:8000/api/v1/nodes/phone-01-e55c718e
```

**想彻底移除一台设备**，正确顺序是：

1. 先在手机上停掉 Agent：`pkill -f agent.py`
2. 再在平台上删除节点记录

反过来做的话，Agent 下次心跳会立刻把节点重新注册回来。

### 6.7 跑自动验收

```bash
# 里程碑 1（11 项）
python docs/acceptance/m1_check.py --phase online

# 里程碑 2（17 项，需要节点在线）
python docs/acceptance/m2_check.py --phase online

# 里程碑 2 离线场景（16 项，需先停掉至少 1 个 Agent）
python docs/acceptance/m2_check.py --phase offline
```

**离线场景要真正生效，必须先确认 Agent 进程死透了**（见 §6.1 的警告）。

---

## 7. 接口直接调用

所有 REST 接口在 `/api/v1` 下，需带 `X-Auth-Token` 头（**`/system/health` 除外**）。

**在线接口文档（Swagger）**：<http://127.0.0.1:8000/docs> —— 可以直接在浏览器里点着试。

### 7.1 常用命令

```bash
TOKEN="edge-cluster-dev-token"
BASE="http://127.0.0.1:8000/api/v1"

# 健康检查（无需 token）
curl -s $BASE/system/health

# 平台运行信息
curl -s -H "X-Auth-Token: $TOKEN" $BASE/system/info

# 节点列表（可按状态过滤）
curl -s -H "X-Auth-Token: $TOKEN" $BASE/nodes
curl -s -H "X-Auth-Token: $TOKEN" "$BASE/nodes?status=offline"

# 单个节点详情
curl -s -H "X-Auth-Token: $TOKEN" $BASE/nodes/<node_id>

# 最新指标
curl -s -H "X-Auth-Token: $TOKEN" $BASE/nodes/<node_id>/metrics/latest

# 指标历史（曲线用）
curl -s -H "X-Auth-Token: $TOKEN" "$BASE/nodes/<node_id>/metrics?minutes=60"

# 首页大屏聚合数据（一次性拿全部）
curl -s -H "X-Auth-Token: $TOKEN" $BASE/system/overview

# 集群资源时间序列
curl -s -H "X-Auth-Token: $TOKEN" "$BASE/system/cluster-metrics?minutes=30"

# 平台日志尾部
curl -s -H "X-Auth-Token: $TOKEN" "$BASE/system/server-logs?lines=50"

# 给节点改名
curl -X PATCH -H "X-Auth-Token: $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"客厅备用机"}' $BASE/nodes/<node_id>

# 删除节点
curl -X DELETE -H "X-Auth-Token: $TOKEN" $BASE/nodes/<node_id>
```

### 7.2 完整接口清单

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/system/health` | 存活探针（**无需鉴权**） |
| GET | `/system/info` | 平台运行信息 + 节点统计 |
| GET | `/system/overview` | 首页大屏聚合（统计 + 资源汇总 + 告警 + 异常节点 + 各节点指标） |
| GET | `/system/cluster-metrics?minutes=` | 集群资源时间序列 |
| GET | `/system/server-logs?lines=` | 平台日志尾部 N 行 |
| GET | `/nodes?status=` | 节点列表 |
| GET | `/nodes/{id}` | 节点详情 |
| PATCH | `/nodes/{id}` | 改名 / 备注 |
| DELETE | `/nodes/{id}` | 删除节点记录 |
| GET | `/nodes/{id}/metrics?minutes=&limit=` | 节点指标历史 |
| GET | `/nodes/{id}/metrics/latest` | 节点最新指标 |
| GET | `/nodes/{id}/services` | 节点上的服务列表（**M4 起有数据**） |
| WS | `/ws/agent` | Agent 长连接入口（由 Agent 拨出） |
| WS | `/ws/dashboard` | 前端实时事件通道 |

### 7.3 本机代理问题

如果你的电脑设置了 `HTTP_PROXY` 环境变量，`curl` 和 Agent 都会试图走代理，
连本地 `127.0.0.1` 会被拒绝（报 `HTTP 502`）。

**解决**：命令加 `--noproxy '*'`，或设置环境变量：

```bash
export NO_PROXY="127.0.0.1,localhost"
```

Agent 代码里已经强制 `proxy=None`，所以它不受影响。一键脚本也会自动清空代理变量。

---

## 8. 完整功能蓝图（尚未实现）

**这一章描述的是目标形态，代码还没写。** 列出它是为了让你知道这个系统最终会变成什么样，
以及届时怎么用。实现后本文档会同步更新。

### 8.1 里程碑 3：模型库与模型下发 ⏳

**预期能做的事**：

1. **上传模型** —— 在平台上传模型文件（GGUF / ONNX / 自定义），平台计算 sha256、记录大小/类型/框架
2. **模型库管理** —— 列表、查看详情、多版本共存、删除
3. **选择节点下发** —— 勾选一台或多台手机，点"下发"
4. **看进度** —— 下载 / 安装 / 加载 三段进度，实时更新
5. **断点续传** —— 网络中断或 Agent 重启后从断点继续，不用重头下
6. **批量部署** —— 一次发给多台手机，每台独立进度
7. **失败重试** —— 单台失败不影响其他台，可单独重试

**预期操作路径**：控制台会出现「模型库」菜单 → 上传 → 选中模型 → 点「下发」→ 选节点 → 看进度条。

### 8.2 里程碑 4：服务管理与 API 暴露 ⏳

**预期能做的事**：

1. **一键启停** —— 在控制台点"启动/停止/重启"
2. **运行参数设置** —— 端口、上下文长度、线程数等（由服务定义驱动，不硬编码）
3. **服务日志** —— 在控制台直接看手机上的服务日志
4. **资源占用** —— 单个服务占用的 CPU / 内存
5. **崩溃自动拉起** —— 服务挂了自动重启
6. **健康检查** —— 进程存在 ≠ 服务可用，会做探活
7. **一机多服务** —— 一台手机跑多个模型服务（**前提是做了资源准入**，见评审建议 O4）
8. **标准 API 自动暴露**：
   - LLM → OpenAI 兼容接口（`/v1/chat/completions`）
   - VLM / 多模态 → 文本 + 图片输入
   - 图像识别 / OCR / 分类 / 检测 → REST 接口
9. **Endpoint 展示** —— 平台统一展示每个服务的地址、端口、模型名、调用状态

**预期操作路径**：控制台会出现「服务」菜单 → 选节点 → 选模型 → 点「启动」→ 拿到 Endpoint 地址 → 直接调。

### 8.3 里程碑 5：稳定性收尾 ⏳

- 指令幂等（`cmd_id` 去重，断线重发不重复执行）
- 心跳抖动容错、状态机防抖
- 日志体系完善
- Termux 一键安装脚本打磨（`install.sh` 已提前写好）
- 完整验收报告

### 8.4 更远：跨公网 / P2P ⏳

让异地手机也能接入。

**重要提示**：原版需求写的是"P2P 优先，必要时中继"，
但**我建议反过来——中继保底 + P2P 优化**。理由见评审建议 O8。

---

## 9. 注意事项

### 9.1 系统限制（无法通过配置绕过，必须知悉）

| # | 限制 | 影响 | 能否绕过 |
|---|---|---|---|
| 1 | **不是真正的 Android Service** | 进程优先级依赖 Termux 应用，极端内存压力下系统仍会回收 | 否，需 root |
| 2 | **厂商省电策略** | MIUI / EMUI / ColorOS / FuntouchOS / OneUI 后台管理激进 | 只能手动放行（§4.6） |
| 3 | **Doze 模式断网** | 深度休眠时网络被切断，心跳中断 → 平台判离线 | 否，醒来会自动重连 |
| 4 | **`termux-wake-lock` 非万能** | 能显著提升存活率，但**不能 100% 保证不被杀** | 否 |
| 5 | **功耗 / NPU 使用率拿不到** | 相关指标长期为"—" | 否，需硬件/内核支持 |
| 6 | **手机跑大模型速度有限** | 7B 量化模型约每秒几 token 量级 | 这是硬件上限 |

### 9.2 使用纪律

**① 停 Agent 要杀干净**
用任务管理器式的"停止"通常只杀外层壳，**Python 子进程会活着继续发心跳**。
必须按 CommandLine 精确匹配杀（命令见 §6.1）。

**② 每个模拟实例必须用不同的 `--name`**
同名会争用同一个身份文件，互相顶掉连接。

**③ 不要用 `--platform-url` 硬编码去覆盖正常发现**
真机环境下让 Agent 走 UDP 自动发现；只在发现失败时用 `--platform-url` 兜底。
硬编码 IP 后，平台换地址就得重新配所有手机。

**④ 平台主机必须常开**
平台一关，所有手机都在做无意义的断线重连。这也是"无人值守"的前提。

**⑤ 改配置文件后要重启对应进程**
`config.toml` 在启动时读取（是模块级单例），改了不重启不生效。

### 9.3 安全须知（**请认真看**）

当前版本按需求约定**没有做**多用户、权限、HTTPS。这意味着：

**⚠️ 共享 token 是明文在 HTTP 上传输的。**

局域网内任何设备只要抓包，就能拿到 token，进而：
- 查看你所有的设备和指标
- 伪造节点注册
- **在后续版本（M4）里，还能远程启动手机上的服务和下发文件**

**这不只是"没做权限"，而是"给了他人执行能力"。**

**按成本排序的缓解措施**：

| 措施 | 成本 | 效果 |
|---|---|---|
| **改掉默认 token** | 极低 | 至少不是"公开的默认口令" |
| 不在公共/不可信 WiFi 下使用 | 零 | 避免被人抓包 |
| 组网隔离（单独路由器/AP/VLAN） | 低 | 物理上排除外部设备 |
| 网关模式下给每个服务单独签发 API key | 中 | 限制泄露范围（M4 建议做） |
| 上公网必须 HTTPS + 设备证书 | 高 | 真正解决（当前明确不做） |

**结论：这套系统只应在完全可信的局域网内使用，不要暴露到公网。**

### 9.4 性能与数据预期

| 项 | 说明 |
|---|---|
| **指标存储** | 原始采样保留 **7 天**（可配 `metric_retention_days`），每天自动清理一次 |
| **降采样保护** | 同一节点两次入库最小间隔 2s（`metric_min_interval`），避免高频上报撑爆库 |
| **GPU / NPU 大概率是空** | 多数手机不暴露对应 sysfs 节点，这是**正常的**，不是采集器坏了 |
| **功耗永远是空** | 免 root 拿不到，这是诚实留空 |
| **曲线断点是正常的** | 表示"没采到"，不是数据为 0 |

### 9.5 关于"看到"—"时的正确心态"

这是本项目最核心的设计原则，值得单独说：

**平台宁可显示"—"，也不会补一个 0 上去。**

- 手机不暴露 NPU 使用率 → 显示"—"，而不是"NPU 0%"
- 温度传感器读不到 → 显示"—"，而不是"温度 25℃"
- 节点离线 → 当前指标显示"—"，而不是最后一次的旧值

**为什么这么较真**：数字一旦显示出来，人就会拿它做判断。
如果 NPU 明明在满载跑，界面显示"0%"，你会以为任务没执行；
如果手机 60℃ 烫手，界面显示"25℃"，你会以为散热正常。

**在监控系统里，一个错误的数据比没有数据更危险。**

---

## 10. 故障排查手册

### 10.1 手机上 Agent 连不上平台

| 症状 | 原因 | 处置 |
|---|---|---|
| `proxy rejected connection: HTTP 502` | 本机设了 `HTTP_PROXY`，`websockets` 走了代理 | 代码已强制 `proxy=None`；仍出现则设 `NO_PROXY=127.0.0.1,localhost` |
| UDP 发现失败 | 平台未启动 / 不同网段 / AP 隔离 / 防火墙拦 UDP | 检查同网段；或直接 `--platform http://<平台IP>:8000` |
| 平台收不到心跳 | token 不一致 | 比对 `server/config.toml` 与 `node-agent/config.toml` 的 `token` |
| 连接被立即关闭 | Agent 用了 `127.0.0.1` 作为平台地址 | 真机必须用**局域网 IP** |
| 平台日志有"鉴权失败" | token 不一致 | 同上 |

### 10.2 节点状态异常

| 症状 | 原因 | 处置 |
|---|---|---|
| 节点频繁上下线闪烁 | 网络抖动，或手机进 Doze | 调大 `offline_threshold` 与 `offline_confirm_times` |
| 手机息屏后节点离线 | Doze / 省电策略 | 按 §4.6 完成保活配置；**仍无法完全避免** |
| 杀掉 Agent 后节点一直不离线 | **Python 子进程没死透** | 按 §6.1 用 CommandLine 精确匹配后再杀 |
| 平台重启后节点全离线 | **预期行为** | 等 Agent 自动重连（几秒到十几秒） |
| 平台重启后 uptime 很大 | 已修复的历史缺陷 | 确认代码是最新版；正常应从重连时刻计时 |
| 出现重复节点 | 手工改了 `node_id` 或换了 `--name` | 每台设备保持固定的 `--name` |

### 10.3 数据展示异常

| 症状 | 原因 | 处置 |
|---|---|---|
| 指标全显示"—" | 该设备不暴露对应接口 | **预期行为**，见 §9.5 |
| 某节点"指标陈旧" | 心跳正常但采样超 15s 未更新 | 节点负载可能打满导致采集卡；该节点已排除出汇总，不会污染数据 |
| 曲线中间断开 | 那一刻采集不到 | **预期行为**，断点 ≠ 数值 0 |
| 汇总数字比实际少 | 只统计"在线且采样新鲜"的节点 | 检查 `resources.counted_nodes` 与 `online_nodes` 是否相等 |
| 运行中服务恒为 0 | 服务管理还没实现 | 正常，等 M4 |
| 最近部署任务为空 | 模型下发还没实现 | 正常，等 M3 |

### 10.4 控制台问题

| 症状 | 原因 | 处置 |
|---|---|---|
| 打开提示"前端未构建" | 没跑 `npm run build` | 构建后刷新；或 `npm run dev` 走 5173 |
| 页面白屏 | 平台未启动 / 端口不对 | 先 `curl http://127.0.0.1:8000/api/v1/system/health` |
| 右上角"实时通道"显示重连中 | 平台重启了，或网络抖动 | 会自动指数退避重连，恢复后自动转"已连接" |
| 数据不刷新 | 浏览器标签被后台挂起 | 切回标签页；或手动点刷新 |

---

## 11. 附录：配置项与状态枚举速查

### 11.1 平台配置（`server/config.toml`）

```toml
[server]
host = "0.0.0.0"                 # 监听地址（真机接入必须 0.0.0.0）
port = 8000                      # HTTP / WebSocket 端口
token = "edge-cluster-dev-token" # 共享 token（必须与 Agent 一致）
discovery_port = 37020           # UDP 发现端口
offline_threshold = 15           # 秒：心跳超时多久算离线
offline_confirm_times = 3        # 防抖：连续 N 次巡检超时才标 offline
heartbeat_interval = 5           # 秒：巡检周期
service_probe_interval = 15      # 秒：服务探活周期（M4 用）
metric_retention_days = 7        # 指标保留天数
metric_min_interval = 2          # 秒：同节点两次入库最小间隔
log_level = "INFO"

[data]
db_path = "data/edge_cluster.db"
models_dir = "data/models"       # 上传的模型存放目录（M3 用）
logs_dir = "data/logs"

[web]
serve_static = true              # 是否托管 web/dist（单端口交付）

[alerts]
enabled = true
cpu_usage_warn = 85.0
mem_usage_warn = 85.0
storage_usage_warn = 90.0
temperature_warn = 45.0
temperature_critical = 55.0
alarm_on_offline = true
probe_timeout_warn = 3
```

**环境变量覆盖**：`EDGE_<段名>__<键名>`（双下划线分隔），优先级高于配置文件。

```bash
EDGE_SERVER__PORT=9000 EDGE_SERVER__TOKEN=my-secret python3 run.py
```

### 11.2 Agent 配置（`node-agent/config.toml`）

```toml
[agent]
node_id = ""                     # 留空自动生成并持久化
name = "phone-02"                # 显示名（建议每台不同）
token = "edge-cluster-dev-token" # 必须与平台一致
heartbeat_interval = 5           # 心跳间隔（秒）
platform_url = ""                # 留空 = UDP 自动发现；填了 = 直连兜底
platform_port = 8000

[discovery]
enabled = true
port = 37020
broadcast_addr = "255.255.255.255"
attempts = 3                     # 广播次数
interval = 1.0                   # 广播间隔（秒）
timeout = 1.5                    # 单次等待响应超时

[reconnect]
base_delay = 1.0                 # 退避基数
max_delay = 60.0                 # 退避上限
jitter = 0.2                     # 抖动系数

[storage]
models_dir = "./models"          # 模型文件目录（M3 用）
logs_dir = "./logs"
log_max_bytes = 5242880          # 日志单文件 5MB 滚动
log_backup_count = 3

[service]                        # M4 用
health_timeout = 60
health_fail_threshold = 3
health_interval = 3
restart_backoff = 3
stop_grace = 5

[simulate]                       # 仅模拟模式
enabled = false
model = "Xiaomi 13"              # 模拟的设备型号
soc = "Snapdragon 8 Gen 2"
android_version = "14"
fail_download_once = false       # 模拟"下载到 50% 中断"（M3 验收用）
crash_service_after = 0          # 模拟服务崩溃（M4 验收用）
disconnect_after = 0             # 模拟运行 N 秒后断线（M1 验收用）
```

### 11.3 Agent 命令行参数

```bash
python3 agent.py [选项]

--config PATH              配置文件路径（默认 node-agent/config.toml）
--simulate                 模拟模式（不需要真机）
--name NAME                节点显示名
--node-id ID               指定 node_id（默认自动生成并持久化）
--platform-url URL         平台地址（覆盖自动发现）
--token TOKEN              共享 token
--heartbeat-interval N     心跳间隔（秒）
--log-level LEVEL          DEBUG / INFO / WARNING / ERROR
--log-tag TAG              日志文件名前缀（多实例并存时区分）
```

### 11.4 状态枚举释义（冻结）

**节点状态**

| 状态 | 判定条件 | 对应现实 |
|---|---|---|
| `never_seen` | 该节点从未有过心跳 | **没有上报**（不是离线！） |
| `offline` | 曾有心跳，但 `now - last_heartbeat > 15s` | **节点离线** |
| `online` | 心跳在阈值内 | 正常 |

**服务状态**（M4 起使用）

| 状态 | 判定条件 | 对应现实 |
|---|---|---|
| `stopped` | 子进程不存在 | **服务停止** |
| `starting` | 进程已拉起，健康检查未通过 | 启动中 |
| `running` | 进程存在 **且** 健康检查通过 | 正常 |
| `error` | 进程存在 **但** 健康检查连续失败 | **服务异常**（进程在但没干活） |

> **`stopped` 和 `error` 必须分开**：前者是"没跑"，后者是"跑了但坏了"。
> 排查方向完全不同。同样，`never_seen` 和 `offline` 混一起会把排查带偏。

### 11.5 端口速查

| 端口 | 归属 | 协议 | 用途 |
|---|---|---|---|
| **8000** | 平台 | TCP | REST API、`/ws/agent`、`/ws/dashboard`、控制台 |
| **37020** | 平台 + Agent | UDP | 局域网自动发现 |
| **8080+** | 模型服务 | TCP | 由服务定义指定（M4，非硬编码） |
| 5173 | 前端 dev | TCP | 仅 `npm run dev` 时使用 |

### 11.6 文件与目录速查

| 路径 | 内容 |
|---|---|
| `server/data/edge_cluster.db` | SQLite 数据库（节点、指标、模型、服务） |
| `server/data/models/` | 平台侧模型库（M3） |
| `server/data/logs/` | 平台日志 |
| `node-agent/state/by-name-<name>.json` | 节点身份（node_id 持久化），**别删** |
| `node-agent/models/` | 手机上的模型文件（M3） |
| `node-agent/logs/agent.log` | 手机上的 Agent 日志 |
| `web/dist/` | 前端构建产物（单端口交付时被平台托管） |
| `.running.pids` | 一键脚本记录的进程号，供 `stop_all` 回收 |
