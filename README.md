# 手机边缘计算集群管理平台

[![CI](https://github.com/honny2025/edge-cluster/actions/workflows/ci.yml/badge.svg)](https://github.com/honny2025/edge-cluster/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue.svg)](https://www.python.org/downloads/)
[![Node 18+](https://img.shields.io/badge/node-18%2B-green.svg)](https://nodejs.org/)

把局域网内的 Android 手机（Termux 环境）当作独立的**边缘计算节点**，多台手机组成集群，
由一个统一平台完成：

> **设备自动发现 → 节点状态监控 → 模型下发 → 一键启动服务 → API 对外服务 → 运行监控**

---

## 目录

- [为什么做这个](#为什么做这个)
- [现在能用什么](#现在能用什么)
- [快速开始](#快速开始)
- [界面预览](#界面预览)
- [项目结构](#项目结构)
- [关键技术点](#关键技术点)
- [文档导航](#文档导航)
- [常见问题](#常见问题)
- [参与贡献](#参与贡献)
- [许可](#许可)

---

## 为什么做这个

手边攒了几台淘汰的 Android 手机，性能其实不差（ 8 核 CPU、8~12GB 内存、部分带 NPU），但它们的日常是躺在抽屉里吃灰。

想拿它们跑点本地推理（小语言模型、图像分类、OCR），会遇到几个绕不开的问题：

- **没有统一入口**：每台手机都要单独 adb / ssh 进去操作，3 台以上就乱了
- **模型分发麻烦**：几百 MB 到几 GB 的模型文件，靠 U 盘或网盘来回拷
- **看不到状态**：哪台在跑、哪台挂了、温度多少、内存还剩多少，全靠一个个问
- **进程保不住**：Termux 一崩溃退出，服务就没了；被系统省电策略杀掉也无人知晓

这个项目的目标就是把这件事**做得像样一点**：一个网页控制台，看到所有节点的实时状态，
点几下就能把模型推下去、把服务拉起来，然后通过统一的 API 调用它们。

**它不是**生产级的 Kubernetes-for-phones。它面向的是**自己家里的局域网 + 自己手里的几台设备**这个规模。

---

## 现在能用什么

> ⚠️ **请先读这一节**。当前版本 `0.5.0-m5`，**里程碑 1~5 全部完成并实测通过**。
> 下面每一项都标了它对应的验收脚本；数字都是本机真跑出来的，不是估计。

### ✅ 已实现并实测通过

**M1 — 骨架 + 通信闭环**

| 能力 | 说明 | 验收 |
|---|---|---|
| **设备自动发现** | UDP 广播（37020），Agent 主动拨出；发现失败可手工填 IP 兜底 | M1 ✅ |
| **节点注册与心跳** | Agent 主动建立 WebSocket，每 5s 心跳；断线指数退避重连 | M1 ✅ |
| **状态机（严格区分）** | `never_seen` / `online` / `offline` **严格区分**，禁止「没数据 = 离线」 | M1 ✅ |
| **模拟器** | `--simulate` 与真机**共用同一份代码**，无需真实设备即可跑通全流程 | M1 ✅ |
| **可移植部署** | 自动探测 Python、项目内建 venv、pip 失败自动换镜像、端口占用检测 | M1 ✅ |

**M2 — 完整状态监控 + 首页大屏**

| 能力 | 说明 | 验收 |
|---|---|---|
| **指标采集** | CPU / GPU / NPU / 内存 / 存储 / 温度 / 网络；拿不到的**留空不造伪** | M2 ✅ |
| **指标扩展采集** | CPU 频率、电池电量与状态、分项温度（`termux-api` 优先、sysfs 降级） | M2 ✅ |
| **首页大屏** | 6 汇总卡 + 使用率条 + 告警面板 + 集群趋势图 + 异常节点 + 节点实时列表 | M2 ✅ |
| **集群时序聚合** | CPU/GPU/NPU 在线节点均值曲线，15 分钟 ~ 24 小时 | M2 ✅ |
| **历史曲线** | 节点详情 4 张曲线 + 时间范围切换，**`null` 画断点不补 0** | M2 ✅ |
| **告警** | 阈值全部配置驱动；`null` 不告警、`never_seen` 不误报离线 | M2 ✅ |
| **采样新鲜度判定** | 心跳正常但采集卡住 > 15s 判 `stale`，排除出汇总 | M2 ✅ |

**M3 — 模型库 + 模型下发**

| 能力 | 说明 | 验收 |
|---|---|---|
| **模型库** | 流式上传 + 流式 sha256；列表 / 详情 / 删除（含文件与附属物） | m3 ✅ 22/22 |
| **断点续传下载** | `GET /models/{id}/download` 支持 `Range` + `ETag` | m3 ✅ |
| **模型登记信息** | 框架 / 量化 / 目标 SoC / QNN 版本 / 后端 / 依赖 / 运行参数 / 自检命令，可增量修改 | m6 ✅ |
| **兼容性预检** | 逐维度 `pass / fail / unknown`；**拿不到 ≠ 不兼容** | m6 ✅ |
| **附属物（Deployment Package）** | 配置 / tokenizer 等跟着模型一起下发 | m6 ✅ |
| **部署编排** | 多节点、逐节点 9 态、真实进度、部分成功部分失败 | m3_deploy ✅ 11/11 |
| **失败重试 / 取消** | 只重投失败节点；`cancelled` 态；已结束再取消返回 409 | m6 ✅ |
| **Self Test** | 跑模型自检命令，**不通过不许判 `ready`** | m6 ✅ |
| **下发并发治理** | 默认并发 2，避免批量下发打满局域网导致集体误判离线 | m6 ✅ |

**M4 — 服务管理 + API 暴露**

| 能力 | 说明 | 验收 |
|---|---|---|
| **服务托管** | `subprocess` 进程组、幂等启停、停止序列不留僵尸 | m6 ✅ |
| **状态机 7 态** | `stopped / starting / running / restarting / stopping / crashed / error` | m6 ✅ |
| **资源准入** | `mem_required_mb` + 互斥组；不满足返回 409 且原因是人话 | m6 ✅ |
| **主动探活** | `POST /services/{id}/check` 打 `health_url` | m6 ✅ |
| **日志** | 子进程 stdout 落盘，按行数取回 | m6 ✅ |
| **最简网关** | 转发 + 计时（last/avg）+ 计数（请求数/失败数） | m6 ✅ |

**M5 — 稳定性收尾**

| 能力 | 说明 | 验收 |
|---|---|---|
| **指令幂等** | 每条指令带 `cmd_id`，Agent 侧 LRU 去重 | m6 ✅ |
| **服务自恢复** | `auto_start`（重启后拉起）+ `auto_restart`（崩溃看门狗），**默认关闭** | m5 ✅ |
| **平台重启不假在线** | 重启时把全部 `online` 强制降级为 `offline` | m2 ✅ |
| **日志体系** | 平台 / Agent / 服务三层 | m6 ✅ |

**需求书里只写了一句、这次做完整的两块**

| 能力 | 说明 | 验收 |
|---|---|---|
| **设备文件管理** | 浏览 / 属性 / 占用 / 新建 / 删除 / 复制 / 移动；平台 ↔ 设备双向传输 | files ✅ |
| **文件安全约束** | 沙箱根目录白名单 + 一次性传输令牌 + 半截文件不给下载 | files ✅ |
| **操作审计查询** | `GET /audit` 过滤分页；**只有读接口，没有任何写接口** | audit ✅ |
| **四类令牌** | 共享 / 设备 / 传输 / 模型下载，用途隔离互不通用 | m6 ✅ |
| **远程命令默认关闭** | 默认不可执行任意命令；开启也需白名单 + 审计 | ✅ |
| **部署失败回滚** | 落位前把旧文件移入回滚日志（同文件系统改名，零额外占用），后续任何一步失败则**逐字节还原**；建不了回滚点就不动手 | rollback ✅ |

**前端**：`总览 / 节点 / 节点详情 / 模型库 / 部署 / 服务 / 文件管理 / 操作审计 / 系统` 九个页面，
`npx tsc --noEmit` 无错误，`npm run build` 通过，`web/dist` 已构建进交付包。

**实测验收汇总**

| 脚本 | 结果 |
|---|---|
| `config_check.py` | **8/8** |
| `m1_check.py --phase online` / `--phase offline` | **11/11** / **8/8** |
| `m2_check.py --phase online` / `--phase offline` | **17/17** / **16/16** |
| `m3_check.py` | **22/22** |
| `m3_deploy_check.py` | **11/11** |
| `m2_ip_check.py` | **3/3** |
| `agent_metrics_check.py` | **41/41** |
| `files_check.py` | **失败 0 项** |
| `m5_selfheal_check.py` | **失败 0 项** |
| `m6_model_deploy_check.py` | **全部通过** |
| `npx tsc --noEmit` + `npm run build` | 通过 |

> 跑法见 [docs/acceptance/README.md](docs/acceptance/README.md)。
> `m3_check` / `m2_ip_check` / `m3_deploy_check` 会真往数据库写东西，
> 要起一个**隔离实例**再跑（同一份文档里写了命令）。

### ❌ 明确不做（不是没做完，是不做）

| 项 | 原因 |
|---|---|
| **真实公网 P2P / 中继** | 需要 STUN/TURN 或中继服务器，本机没有可验证的环境。连接层抽象已就位 |
| **设备重启 / 关机** | Termux 免 root 下没有可用的关机接口，强行做只会是假的 |
| **内建 HTTPS** | 面向局域网；Agent 已支持 `https://`→`wss://`，需要时前置反向代理即可 |
| **用户 / 权限体系** | 只有四类令牌，用途隔离 |
| **QNN 推理实现** | 本项目只做**手机管理平台**，不做推理引擎。模型怎么跑是设备侧自己的事 |

详见 [ROADMAP.md](ROADMAP.md) 与 [CHANGELOG.md](CHANGELOG.md)。

---

## 快速开始

### 前置要求

| 项 | 要求 | 说明 |
|---|---|---|
| **Python** | **3.10+** | 脚本会自动探测并校验版本 |
| **Node.js** | 18+（可选） | 仅前端开发时需要；`web/dist` 已随仓库提供，不装 Node 也能用控制台 |
| **操作系统** | Windows / Linux / macOS / WSL | 三种都有对应脚本 |

无需 root、无需 Docker、无需数据库服务（用 SQLite）。

### 3 分钟跑通闭环

**Windows**

```powershell
.\start_all.ps1 -BuildWeb
```

> Windows 默认禁止运行 `.ps1`，**最省事的方式是直接双击 `启动.bat`**
> （内部已用 `-ExecutionPolicy Bypass` 绕过）。
> 想在命令行跑就用：`powershell -ExecutionPolicy Bypass -File .\start_all.ps1 -BuildWeb`

**Linux / macOS / WSL**

```bash
BUILD_WEB=1 ./start_all.sh
```

然后浏览器打开 <http://127.0.0.1:8000>，就能看到 2 个模拟节点实时在线。

**停止**

```powershell
.\stop_all.ps1        # Windows
```
```bash
./stop_all.sh         # Linux / macOS
```

### 换端口

```powershell
.\start_all.ps1 -Port 8001        # Windows：模拟节点会自动连新端口
```
```bash
PORT=8001 ./start_all.sh          # Linux / macOS
```

### 打包带走 / 换一台电脑跑

```bash
python tools/pack_portable.py     # → edge-cluster-portable-<版本>.zip
```

一键脚本**不写死任何本机路径**：自动探测 Python（并校验 ≥ 3.10）、在项目内建 `.venv`、
pip 装不动时自动换清华镜像、启动前检查端口占用。目标机器只需要有 **Python 3.10+**。

### 手工分步启动（调试用）

```bash
# 终端 1：平台
cd server
python -m venv .venv && .venv/bin/pip install -r requirements.txt   # Windows: .venv\Scripts\pip
python run.py
# → http://127.0.0.1:8000   接口文档 http://127.0.0.1:8000/docs

# 终端 2：模拟节点 1
cd node-agent
pip install -r requirements.txt
python agent.py --simulate --name phone-01 --platform-url http://127.0.0.1:8000

# 终端 3：模拟节点 2
python agent.py --simulate --name phone-02 --platform-url http://127.0.0.1:8000
```

> **真实局域网**中不需要传 `--platform-url`：Agent 会通过 UDP 广播（37020）自动发现平台，
> 发现失败时才需要手工填 IP 兜底。
>
> ⚠️ **同一台机器跑多个模拟节点，务必给每个实例传不同的 `--name`**，
> 否则会串用同一个 `node_id` 导致互相顶掉连接。

### 真机接入（Termux）

```bash
# 手机端（Termux）
pkg install python
# 把 node-agent/ 整个目录拷到手机上，然后：
bash install.sh
```

`install.sh` 会处理免 root 运行、wake-lock 保活、开机自启。
详细步骤见 [`docs/deployment.md`](docs/deployment.md) 与
[`docs/user-manual.md`](docs/user-manual.md)（§真机接入七步）。

### 跑验收

```bash
python docs/acceptance/config_check.py               # 配置层自检（8 项，秒级）
python docs/acceptance/m1_check.py --phase online    # 里程碑 1（11 项）
python docs/acceptance/m2_check.py --phase online    # 里程碑 2（17 项）
python docs/acceptance/m2_check.py --phase offline   # 需先停掉至少 1 个 Agent
python docs/acceptance/m6_model_deploy_check.py      # 模型登记 / 预检 / 部署 / 取消（最大的一支）
```

---

## 界面预览

> 截图待补充。如果你跑起来了想帮忙截几张，欢迎提 PR 放进 `docs/screenshots/`。

**首页大屏包含**：

| 区块 | 内容 |
|---|---|
| 汇总卡 ×6 | 节点总数 / 离线节点 / 运行中服务 / CPU 均值 / 内存均值 / 存储均值 |
| 使用率条 | 集群内存、存储使用率（按阈值转暖色） |
| 告警面板 | 按 critical / warning 分色；无告警时明确显示「当前无告警」 |
| 集群趋势图 | 近 15 分钟 ~ 24 小时，CPU/GPU/NPU 在线节点均值曲线 |
| 异常节点 | 离线与「从未接入」**分开说明，文案不同** |
| 节点实时列表 | 含 CPU/内存/存储/温度/NPU/电量列；**离线行压暗且指标列显示「—」** |

**九个页面**：总览 / 节点 / 节点详情 / 模型库 / 部署 / 服务 / 文件管理 / 操作审计 / 系统（备份与恢复）。

**看图须知**：曲线出现**断点**表示该时刻采集不到该项（比如手机不暴露 NPU 使用率），
表示「无数据」，**不是数值为 0**。这是刻意的 —— 补 0 就是把没数据伪装成测量结果。

---

## 项目结构

```
edge-cluster/
├─ server/                     # FastAPI 平台服务端（Python 3.10+ / SQLite）
│  ├─ app/
│  │  ├─ models/               ORM 表定义 + 冻结的状态枚举（enums.py 是唯一真源）
│  │  ├─ core/                 日志 / 鉴权 / ★连接层抽象（预留 P2P·中继）/ 审计写入
│  │  ├─ services/             节点注册 · 心跳巡检 · UDP 发现 · 事件总线
│  │  │                        ★指标聚合 · 告警 · 大屏装配 · 兼容预检
│  │  │                        ★下发队列 · 文件传输 · 模型令牌 · 服务看门狗
│  │  └─ api/                  REST 路由 + /ws/agent + /ws/dashboard
│  │                           routes_system / nodes / models / deployments /
│  │                           services / gateway / files / audit
│  ├─ config.toml              全部可调项（含 [alerts] 告警阈值）
│  └─ run.py                   启动入口
├─ web/                        # React 18 + Vite + TS + Ant Design + ECharts
│  └─ src/
│     ├─ pages/                总览 · 节点 · 节点详情 · 模型库 · 部署 · 服务 · 文件 · 审计
│     ├─ components/           状态标签 · ★通用时序图 MetricChart
│     ├─ types.ts              ★与后端字段一一对齐的类型契约（改后端记得同步这里）
│     ├─ api.ts                统一请求封装 + errText()（把后端 detail 变成人话）
│     └─ utils/                单位格式化 · 图表配色主题
├─ node-agent/                 # 手机端 Agent（可整目录拷到 Termux）
│  ├─ agent.py                 主程序（真机 / --simulate 共用同一份代码）
│  ├─ agent_metrics.py         /proc · termux-api · sysfs 采集，拿不到留空
│  ├─ agent_files.py           文件沙箱与读写
│  ├─ agent_rollback.py        部署失败回滚：落位前先挪走旧文件，失败则还原并验证
│  ├─ agent_discovery.py       UDP 广播发现客户端
│  ├─ install.sh               Termux 一键安装（免 root + 保活 + 开机自启）
│  └─ config.toml
├─ docs/
│  ├─ user-manual.md           操作说明书（准备清单 / 操作步骤 / 注意事项）
│  ├─ design/                  需求评审 · 架构设计 · 技术决策
│  ├─ milestones/              各里程碑计划与验收报告
│  ├─ acceptance/              ★全部验收脚本（跑一遍就知道对不对）
│  └─ deployment.md            部署文档（PC + Termux 真机）
├─ tools/
│  └─ pack_portable.py         白名单式打包可移植交付包 + 自检
├─ start_all.ps1 / .sh         PC 端一键启动（自动探测运行时，可移植）
├─ stop_all.ps1 / .sh          一键停止
├─ 启动.bat / 停止.bat          Windows 双击入口（绕过 .ps1 执行策略限制）
├─ CONTRIBUTING.md / SECURITY.md
├─ CHANGELOG.md / ROADMAP.md
├─ 系统迁移与使用指南.md        ★拿到包先读：换电脑、日常使用、排错
├─ 平台使用指南.md              ★界面每一块是什么、怎么看懂「—」
├─ 手机端部署指南.md            ★可直接丢给 AI 代理的手机端部署提示词
├─ 功能清单.md                  已实现什么 / 还缺什么（A / B / C 三类）
├─ 真机测试检查单.md            插上手机后照着一项项打勾
└─ 快速开始.txt                 面向「拿到这个包的人」的三步指引
```

---

## 关键技术点

| 点 | 做法 | 原因 |
|---|---|---|
| **通信方向** | Agent 主动向平台建立 WebSocket | 手机可能处于 NAT / 省电策略后，平台无法主动连手机 |
| **大文件传输** | 走 HTTP 下载接口（支持 `Range` 断点续传） | WS 传大文件会阻塞心跳；HTTP 天然支持续传 |
| **节点发现** | UDP 广播（37020）+ 手工填 IP 兜底 | 比 mDNS 依赖少，Termux 下更可控 |
| **状态机** | 严格区分，**禁止「没数据 = 离线」** | 核心要求 |
| **指标留空** | 拿不到的指标存 `NULL`，前端显示「—」 | 明确要求**不造伪** |
| **连接层抽象** | `core/transport.py` 定义 `Transport` 抽象基类 | 后续可换 P2P / 中继，业务层无感 |
| **Gateway** | `services` 表预留 `gateway_meta` JSON 字段 + 最简转发 | 统计必须在数据路径上，事后补等于推倒重来 |
| **模型服务** | 配置驱动，**禁止硬编码任何模型** | 硬约束 |
| **离线不下发旧值** | overview 对离线 / stale 节点直接给 `nodes_metrics: null` | 从**数据源头**杜绝「旧数据冒充在线」 |
| **采样新鲜度** | 采样落后 > 15s 判 `stale`，同样排除出汇总 | 心跳正常 ≠ 采集中断 |
| **图表断点** | `connectNulls: false`，null 不补 0 | 补 0 就是把「没数据」伪装成测量结果 |
| **防闪烁** | 关动画 + ECharts merge 增量更新 + 等宽数字 | 秒级刷新下保持画面稳定 |
| **重启即归零** | 平台启动把所有 `online` 强制降为 `offline` | 重启后不可能有连接，留着 online 就是假在线 |
| **半截文件不给下载** | 只有传输状态 `done` 才提供下载入口 | 半个模型文件比没有文件更危险 |
| **审计只写不补** | 审计表只有系统写入路径，**没有任何写接口** | 谁都能塞一条的记录，事后不能当依据 |

### 状态判定规则（冻结）

**节点三态**

| 状态 | 判定条件 |
|---|---|
| `never_seen` | 该节点从未有过心跳 —— 对应「**没有上报**」 |
| `offline` | 曾有心跳，但 `now - last_heartbeat > 15s` —— 对应「**节点离线**」 |
| `online` | 心跳在阈值内 |

**服务 7 态**

| 状态 | 判定条件 |
|---|---|
| `stopped` | 子进程不存在（含人工停止） |
| `starting` | 进程已拉起，健康检查尚未通过 |
| `running` | 进程存在 **且** 健康检查通过 |
| `restarting` | 正在按重启序列操作（terminate → kill → 拉起） |
| `stopping` | 已下发停止，进程尚未完全退出 |
| `crashed` | 进程自行退出（非人工停止），退出码可查 |
| `error` | 进程存在 **但** 健康检查连续失败 |

> 原来的计划只有 3 态。多出来的 4 态是因为「正在停止」和「已经崩溃」
> 如果都塞进 `stopped`，运维看到的就永远是一团糊。

**部署逐节点 9 态**

`pending → uploading → downloading → verifying → installing → testing → ready`，
任一步失败转 `failed`，人工取消转 `cancelled`。

> 任务级的聚合状态由 `server/app/models/enums.py:aggregate_deploy_status()` **一处**决定，
> 别的地方不许再写一套判断。

### 端口规划

| 端口 | 归属 | 协议 | 说明 |
|---|---|---|---|
| **8000** | 平台 | TCP | REST API、`/ws/agent`、`/ws/dashboard`、托管控制台 |
| **37020** | 平台 + Agent | UDP | 局域网自动发现 |
| **8080+** | 模型服务 | TCP | 由服务定义指定（非硬编码） |
| 5173 | 前端 dev | TCP | 用 `npm run dev` 时使用 |

> 8000 被占用时不必改配置文件：`.\start_all.ps1 -Port 8001`（或 `PORT=8001 ./start_all.sh`）。

### API 清单

所有 REST 接口挂在 `/api/v1` 下，需要 `X-Auth-Token`（`/system/health` 除外）。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/system/health` | 存活探针（**无需鉴权**） |
| GET | `/system/info` | 平台运行信息 + 节点统计 |
| GET | `/system/overview` | **首页大屏聚合** |
| GET | `/system/cluster-metrics?minutes=` | **集群资源时间序列** |
| GET | `/system/server-logs?lines=` | 平台日志尾部 N 行 |
| GET | `/system/metric-tiers` | **指标三层现状**（保留期 / 行数 / 时间跨度） |
| POST | `/system/metric-tiers/run?full=` | 手动跑一轮降采样 + 分层清理（幂等） |
| GET | `/system/backup/status` | **备份现状**（库大小 / WAL 是否未合并 / 已有备份清单） |
| GET | `/system/backup?include_models=` | **下载备份包**（SQLite 一致性快照；默认不含模型权重） |
| POST | `/system/backup/inspect` | **校验备份包**（只读，一个字节都不改） |
| POST | `/system/backup/restore?confirm=` | 从备份恢复（不带 `confirm=true` 只返回「将要发生什么」） |
| GET | `/nodes?status=` | 节点列表（可按状态过滤） |
| GET | `/nodes/{id}` | 节点详情（含设备能力、Root 三态） |
| PATCH | `/nodes/{id}` | 改名 / 备注 |
| DELETE | `/nodes/{id}` | 删除节点记录 |
| GET | `/nodes/{id}/metrics?minutes=&limit=` | 节点指标历史 |
| GET | `/nodes/{id}/metrics/latest` | 节点最新指标 |
| GET | `/nodes/{id}/services` | 节点上的服务列表 |
| **模型库** | | |
| GET | `/models` | 模型列表 |
| POST | `/models` | 上传模型（流式落盘 + 流式 sha256 + 登记元数据） |
| GET | `/models/{id}` | 模型详情（含附属物清单） |
| PATCH | `/models/{id}` | 更新登记信息（增量：清空即回「未登记」） |
| DELETE | `/models/{id}` | 删除模型（含文件与附属物） |
| GET | `/models/{id}/download` | **Agent 下载口**（`Range` 续传 + `ETag`） |
| POST | `/models/{id}/compat` | **兼容性预检**（不落库、不下发） |
| POST | `/models/{id}/manifest` | 上传附属物（Deployment Package） |
| DELETE | `/models/{id}/manifest/{name}` | 删除一个附属物 |
| **部署** | | |
| POST | `/deployments` | 创建部署任务（多模型 × 多节点） |
| GET | `/deployments` | 任务列表 |
| GET | `/deployments/{id}` | 任务详情（逐节点状态与进度） |
| GET | `/deployments/queue/stats` | 下发队列统计 |
| POST | `/deployments/precheck` | 建任务**之前**先看哪些节点会被卡住 |
| POST | `/deployments/{id}/retry` | 只重投失败节点 |
| POST | `/deployments/{id}/cancel` | 取消（已结束的任务返回 409） |
| POST | `/deployments/{id}/start` | 启动一个 `pending` 态的任务 |
| **服务** | | |
| GET | `/services` | 服务列表 |
| POST | `/services` | 创建服务（**初始为已停止**） |
| GET | `/services/{id}` | 服务详情 |
| DELETE | `/services/{id}` | 删除（运行中返回 409） |
| POST | `/services/{id}/{action}` | `start` / `stop` / `restart` |
| GET | `/services/{id}/logs` | 拉取子进程 stdout |
| POST | `/services/{id}/check` | 主动探活 `health_url` |
| GET | `/gateway/{service_id}/stats` | 网关计时 / 计数 |
| **文件管理** | | |
| GET | `/nodes/{id}/files/roots` | 可访问根目录（含剩余空间） |
| GET | `/nodes/{id}/files` | 浏览目录 |
| GET | `/nodes/{id}/files/stat` | 属性 |
| GET | `/nodes/{id}/files/du` | 占用统计（字节 / 文件数 / 目录数） |
| POST | `/nodes/{id}/files/mkdir` | 新建目录 |
| POST | `/nodes/{id}/files/delete` | 删除（目录需显式递归） |
| POST | `/nodes/{id}/files/copy` | 复制 |
| POST | `/nodes/{id}/files/move` | 移动 / 重命名 |
| POST | `/nodes/{id}/files/pull` | 从设备取回文件 |
| POST | `/nodes/{id}/files/push` | 推送文件到设备 |
| GET | `/files/transfers` | 传输记录 |
| GET | `/files/transfers/{id}` | 单次传输详情 |
| GET | `/files/transfers/{id}/download` | 下载已取回的文件（**仅 `done` 态**） |
| DELETE | `/files/transfers/{id}` | 取消并清理一次传输 |
| **审计** | | |
| GET | `/audit` | 查询操作审计（倒序、可按动作/节点/操作者/成败过滤） |
| GET | `/audit/actions` | 出现过的动作名（供前端下拉） |
| **实时通道** | | |
| WS | `/ws/agent` | Agent 长连接（**由 Agent 拨出**） |
| WS | `/ws/dashboard` | 前端实时事件通道 |

接口文档（Swagger）：<http://127.0.0.1:8000/docs>

---

## 文档导航

**先看这三份**：

| 文档 | 什么时候看 |
|---|---|
| [系统迁移与使用指南.md](系统迁移与使用指南.md) | **换电脑怎么搬、日常怎么用、出问题查哪里** —— 拿到包先看这份 |
| [平台使用指南.md](平台使用指南.md) | **界面每一块是干什么的**、怎么看懂 `—`、名词表、最容易搞错的几个点 |
| [`docs/user-manual.md`](docs/user-manual.md) | **怎么做**、要准备什么、注意事项（含 §0 实现状态总表） |
| [`docs/design/requirement-review.md`](docs/design/requirement-review.md) | **打算改代码前必读** —— 10 条待决策项（O1~O10），每条标注 `[实测]` 或 `[判断]` |

**深入**：

| 文档 | 内容 |
|---|---|
| [`docs/design/architecture.md`](docs/design/architecture.md) | 文字版架构图、通信方式、端口规划、数据流 |
| [`docs/design/tech-decisions.md`](docs/design/tech-decisions.md) | 目录结构、字段表、状态枚举、完整 API 清单、配置项 |
| [`docs/deployment.md`](docs/deployment.md) | 部署文档（PC + Termux 真机） |
| [`docs/acceptance/README.md`](docs/acceptance/README.md) | **验收脚本清单与跑法**（含为什么要起隔离实例） |
| [`docs/milestones/`](docs/milestones/) | 各里程碑计划与验收报告 |
| [ROADMAP.md](ROADMAP.md) | 后续计划与待决策项 |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 贡献指南（含必须跑的验收命令） |
| [SECURITY.md](SECURITY.md) | 安全策略与已知设计取舍 |
| [手机端部署指南.md](手机端部署指南.md) | **直接丢给 AI 代理就能完成手机端部署**（含可复制粘贴的提示词） |
| [功能清单.md](功能清单.md) | 已经实现了什么 / 还缺什么（A/B/C 三类分开） |
| [真机测试检查单.md](docs/guides/真机测试检查单.md) | 插上手机后照着一项项打勾 |
| [C盘瘦身与迁移到E盘指南.md](docs/guides/C盘瘦身与迁移到E盘指南.md) | 与平台无关：这台机器 C 盘满了该怎么清 |

---

## 常见问题

**Q：Agent 报 `proxy rejected connection: HTTP 502`？**

A：本机设置了 `HTTP_PROXY` 环境变量，`websockets` 会自动走代理。
代码已在连接时显式 `proxy=None`；若仍出现，请设置 `NO_PROXY=127.0.0.1,localhost`。

**Q：控制台打开提示「前端未构建」？**

A：先构建：`cd web && npm install && npm run build`，再刷新 <http://127.0.0.1:8000>。
开发期可以用 `npm run dev` 开 5173（已配好 `/api` 与 `/ws` 代理）。
如果不想装 Node，用 `.\start_all.ps1 -BuildWeb` 让脚本自动处理。

**Q：同一台机器跑多个模拟节点，节点互相顶掉连接？**

A：已修复。每个实例按 `--name` 把身份存在 `node-agent/state/by-name-<name>.json`，
不会串用同一个 `node_id`。**务必给每个实例传不同的 `--name`。**

**Q：离线判定要 20 秒，不是说 15s？**

A：当前实现是 `心跳超时阈值 15s` + `连续 2 次巡检确认`（5s/次），实际约 20~25s 才落库为 `offline`。
这个「防抖」是为满足「心跳抖动容错、状态机防抖」而引入的，两个要求本身存在张力。
要严格对齐 15s，把 `server/config.toml` 里 `offline_confirm_times` 改成 `1` 即可
（代价是网络抖动时状态可能闪烁）。详见 [O9 建议](docs/design/requirement-review.md)。

**Q：大屏上某台节点显示「指标陈旧」，是什么意思？**

A：该节点心跳正常，但最后一次指标采样已超过 15s 未更新。此时它**不参与集群汇总**，
指标列显示「—」。原因是心跳与采集是两条路径：主机负载打满时心跳还能发，采集可能卡住。
宁可显示「没数据」，也不拿最后一次旧值当当前值。

**Q：曲线中间断了是怎么回事？**

A：那是**采样不到**，不是数值为 0。常见于手机不暴露 NPU/GPU 使用率，或温度传感器读不到。
断点是刻意的设计（`connectNulls: false`）。

**Q：杀掉 Agent 进程后节点一直不离线？**

A：先确认进程真的死了。用任务管理器式「停止」可能只杀掉外层包装 shell，
**Python 子进程会存活并继续发心跳**。用 PowerShell 精确校验：

```powershell
Get-CimInstance Win32_Process -Filter "name='python.exe'" |
  Where-Object { $_.CommandLine -like '*agent.py*' } |
  Select-Object ProcessId, CommandLine
```

**Q：解压覆盖后文件全没了？**

A：`Expand-Archive -Force` 覆盖解压已有目录会**清空目录且不报错**。
升级请解压到**新目录**，不要覆盖旧的。

---

## 参与贡献

欢迎提 Issue 和 PR。**动手前请先读 [`CONTRIBUTING.md`](CONTRIBUTING.md)**，特别是：

1. 这个项目对「**可验证**」有硬要求 —— 新增功能必须配验收用例
2. 「**拿不到 ≠ 0**」与「**禁止硬编码模型**」是有代价的纪律，不是风格偏好
3. Windows 脚本各有各的编码规则（`.ps1` 要 BOM、`.sh` 要无 BOM + LF、`.bat` 用 GBK + `chcp 936`），别「顺手统一」

安全相关问题请走 [SECURITY.md](SECURITY.md) 的私密渠道，不要开公开 Issue。

---

## 致谢

- 状态机设计与「不造伪」「禁止硬编码」原则来自项目原始需求文档
- 前端图表基于 [ECharts](https://echarts.apache.org/)，UI 组件基于 [Ant Design](https://ant.design/)
- 后端基于 [FastAPI](https://fastapi.tiangolo.com/)，ORM 基于 [SQLAlchemy](https://www.sqlalchemy.org/)

---

## 许可

[MIT License](LICENSE) © 2026 陈鸿彬 (GitHub: honny)

---

## Star 历史

如果这个项目对你有帮助，欢迎点个 Star ⭐
