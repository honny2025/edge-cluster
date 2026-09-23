# 更新日志

本文件记录所有值得注意的变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> **版本号约定**：`0.<里程碑>-<补丁>`。当前 `0.2.0-m2` 表示「已交付里程碑 2」。

---

## [Unreleased]

### 计划中
- 里程碑 3：模型库 + 模型下发（上传 / sha256 校验 / HTTP Range 断点续传 / 批量进度 / 失败重试）
- 里程碑 4：服务管理 + API 暴露（子进程托管 / 健康检查 / 崩溃拉起 / OpenAI 兼容调用）
- 里程碑 5：稳定性收尾（指令幂等 / 状态防抖 / 日志体系 / 最终验收报告）

详见 [ROADMAP.md](ROADMAP.md)。

---

## [0.2.0-m2] — 2026-09-16

**里程碑 2：完整状态监控 + 首页大屏** 已达成。

### 新增

**平台服务端**
- `app/services/metrics_query.py` —— 指标聚合层。各节点最新采样用窗口函数
  `ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY ts DESC)` 一次查出（避免 N+1）；
  新增**采样新鲜度判定**（落后 > `heartbeat_interval × 3` 判 stale）
- `app/services/alerting.py` —— 告警判定。规则全部读 `[alerts]` 配置，代码中无硬编码阈值
- `app/services/overview.py` —— 大屏装配：`build_overview()` 快照 + `build_cluster_series()` 时间序列
- `GET /api/v1/system/overview` —— 首页大屏聚合接口
- `GET /api/v1/system/cluster-metrics?minutes=` —— 集群资源时间序列接口
- `server/config.toml` 新增 `[alerts]` 段（7 项阈值）

**节点 Agent**
- CPU 频率采集：`/sys/devices/system/cpu/cpuN/cpufreq/scaling_cur_freq` 各核平均，
  兼容只暴露 `cpuinfo_cur_freq` 的内核
- 电池采集：`termux-battery-status` 优先、sysfs 降级
- GPU 候选路径 3 → 5 条，区分 `gpu_busy_percentage`（已是百分比）
  与 `gpubusy`（busy/total 需相除）
- 补充字段（`cpu_freq_avg_mhz` / `battery_percent` / `battery_status`）统一进 `extra` JSON，
  **不污染已冻结的主字段表**

**前端**
- `components/MetricChart.tsx` —— 通用时序折线图。`null` 画断点、关动画 + merge 增量更新（防闪烁）
- `utils/theme.ts` —— 配色常量 + `withGaps()`（null 安全）+ UTC 时间解析
- `Dashboard.tsx` 重写为大屏：6 汇总卡 + 使用率条 + 告警面板 + 趋势图 + 异常节点 + 带指标列的节点表
- `NodeDetail.tsx` 重写：当前指标卡组 + 4 张历史曲线 + 时间范围切换 + 离线提示
- `index.css` 新增 `.row-offline`（离线行压暗）、`tabular-nums`（数值等宽防抖动）

### 修复

- **平台重启后节点 `uptime` 不归零（旧数据冒充在线）**
  现象：平台重启后 `uptime_seconds` 仍在累加（实测显示 39734s ≈ 11 小时），
  而该节点实际早已随关机消失。
  根因：`online_since` 持久化在库里。重启后若 Agent 在心跳巡检标 offline **之前**就重连，
  `register()` 看到 `prev_status == online` 便跳过了 uptime 重置，导致在线时长跨重启累积。
  修复：新增 `NodeRegistry.reset_online_on_boot()`，在 `lifespan` 启动时把所有 `online`
  强制降为 `offline` 并清空 `online_since` / `uptime_seconds`。
  理由：平台刚启动不可能持有任何连接，库里留 `online` 就是假在线。
  验证：重启后 uptime 实测 18s / 16s，从重连时刻重新计数。

### 验收结果（实测）

| 项 | 结果 |
|---|---|
| `m2_check.py --phase online` | **17/17**（连跑 3 次稳定） |
| `m2_check.py --phase offline` | **16/16** |
| `m1_check.py --phase online` 回归 | **11/11** |
| `config_check.py` | **8/8** |
| `npm run typecheck` | 零错误 |

离线场景实测数据：
- 停掉 phone-02 后，`counted_nodes` 从 **2 降到 1**，`nodes_metrics[phone-02]` 为 **null**（不泄漏旧值）
- `abnormal_nodes` 出现该节点，`alerts` 产生 1 条 `node_offline`（critical）
- 离线节点历史曲线仍可查（`count=99`）
- `never_seen` 未被误报为 `offline`（区分"没上报"与"离线"）

### 已知取舍

1. **离线判定约 20–25s**（15s 阈值 + 2×5s 防抖确认），与需求文档写的"15s 内"存在张力。
   这是为满足「状态机防抖」有意引入的，已做成配置项
   （`offline_confirm_times = 1` 即严格 15s）。详见 [README](README.md) 与
   [`docs/design/requirement-review.md`](docs/design/requirement-review.md) O9。
2. **集群趋势图与汇总只覆盖在线节点**。若集群整体掉线，趋势图显示"无数据"而非冻结最后曲线 ——
   这是刻意的，冻结曲线容易被误读为实时数据。
3. `recent_deployments` 恒为空数组（M3 起填充，结构已定，前端无需改）
4. 节点详情的服务列表恒为空（M4 才有数据）

---

## [0.1.0-m1] — 2026-09-15

**里程碑 1：骨架 + 通信闭环** 已达成。

### 新增

- **三端骨架**：FastAPI 平台服务端 / React Dashboard / Termux 节点 Agent
- **通信闭环**：UDP 广播自动发现（37020）→ Agent 主动建立 WebSocket → 注册 → 每 5s 心跳
- **状态机**：严格区分 `never_seen` / `online` / `offline`，禁止"没数据 = 离线"
- **指标入库**：心跳携带指标，落 `MetricsSample` 表；拿不到的字段保持 `NULL`
- **连接层抽象**：`app/core/transport.py` 定义 `Transport` 抽象基类 + `LocalWsTransport`
  （为后续替换 P2P / 中继预留，业务层无感）
- **Gateway 预留**：`services` 表预留 `gateway_meta` JSON 字段
- **断线重连**：指数退避（1, 2, 4, 8 … 最大 60s，带 ±20% 抖动）
- **模拟器**：`agent.py --simulate` 与真机**共用同一份代码**，避免两套代码漂移
- **可移植交付**：`tools/pack_portable.py` 白名单式打包 + 自检
- **一键脚本**：`start_all.ps1/.sh`、`stop_all.ps1/.sh`、`启动.bat` / `停止.bat`
  （不写死任何本机路径，自动探测 Python、项目内建 `.venv`、pip 失败自动换清华镜像）
- **验收脚本**：`docs/acceptance/m1_check.py`（11 项）

### 修复

- **多实例共用 `config.toml` 导致 `node_id` 串用**
  现象：第二个实例读到第一个实例的 `node_id`，两者互相顶掉 WS 连接
  （日志表现为反复 `received 1000 (OK)`）。
  修复：按 `--name` 存独立状态文件 `node-agent/state/by-name-<name>.json`，
  仅单实例默认场景才回写 `config.toml`。
- **心跳指标消息层级不一致**：Agent 把 metrics 放在顶层，平台读 `payload["metrics"]`，
  导致指标一直为空。修复：统一为 `payload.metrics` 结构。
- **`websockets` 走系统代理导致连接被拒**：本机设了 `HTTP_PROXY` 时，
  连 `127.0.0.1` 会被代理拒绝（`HTTP 502`）。修复：连接时显式 `proxy=None`。

### 验收结果（实测）

| 用例 | 结果 |
|---|---|
| `m1_check.py --phase online` | **11/11** |

手工验收实测：

| 步骤 | 期望 | 实测 |
|---|---|---|
| 起平台 + 2 模拟节点 | 自动发现并显示 | 2 个节点 `online` ✅ |
| `kill` 掉 phone-02 的 Agent | 阈值内标记 `offline` | T+20s 变 `offline` ✅ |
| 离线节点数据 | 明确显示离线，不残留旧数据 | `last_heartbeat` 保留、`uptime_seconds=0`、状态 `offline` ✅ |
| 重启该 Agent | 自动恢复 `online` 且 `node_id` 复用 | T+3s 恢复 `online`，节点总数仍为 2（无幽灵节点）✅ |
| UDP 广播发现 | 平台响应 `EDGE_DISCOVER_ACK` | 平台日志有响应记录 ✅ |
| 断线重连退避 | 指数退避序列 | 实测 1.0 → 2.4 → 3.7 → 8.8 → 15.2s ✅ |

---

## [0.0.1] — 2026-09-15

### 新增
- 设计文档三件套：`docs/design/architecture.md`（架构图 + 通信方式 + 端口规划）、
  `docs/design/tech-decisions.md`（目录结构 + 字段表 + 状态枚举 + API 清单）、
  `docs/milestones/milestone-1-plan.md`(里程碑 1 任务拆分)

---

[Unreleased]: ../../compare/v0.2.0-m2...HEAD
[0.2.0-m2]: ../../releases/tag/v0.2.0-m2
[0.1.0-m1]: ../../releases/tag/v0.1.0-m1
