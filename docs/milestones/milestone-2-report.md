# 里程碑 2 验收报告

- **日期**：2026-09-16
- **范围**：完整状态监控 + 首页大屏
- **结论**：**已达成**。在线场景 **17/17** 通过，离线场景 **16/16** 通过，M1 回归 **11/11** 仍绿。
- **下一步**：里程碑 3（模型库 + 模型下发），入口见文末。

---

## 1. 本次交付

### 后端

| 文件 | 作用 |
| --- | --- |
| `app/services/metrics_query.py` | 指标聚合查询层：各节点最新采样（窗口函数一次查出）、**采样新鲜度判定**、使用率计算 |
| `app/services/alerting.py` | 告警判定。规则全部读配置，指标为 null 一律不告警 |
| `app/services/overview.py` | 大屏数据装配：集群快照 + 资源时间序列 |
| `app/api/routes_system.py` | 新增 `/system/overview`、`/system/cluster-metrics` |
| `app/config.py` + `config.toml` | 新增 `[alerts]` 配置段（7 个阈值项） |
| `app/services/node_registry.py` | 新增 `reset_online_on_boot()`（修真实缺陷，见 §4） |

### Agent

`agent_metrics.py` 增强（真机 + 模拟双路）：

- **CPU 频率**：`/sys/devices/system/cpu/cpuN/cpufreq/scaling_cur_freq` 各核平均（kHz→MHz），
  兼容只暴露 `cpuinfo_cur_freq` 的内核
- **电池**：`termux-battery-status` 优先，`/sys/class/power_supply/battery/*` 降级；
  同一次采集只调用一次 termux-api，温度与电量复用同一次结果
- **GPU**：候选路径从 3 条扩到 5 条，覆盖 Adreno（kgsl）与 Mali 两类；
  区分 `gpu_busy_percentage`（直接是百分比）与 `gpubusy`（busy/total 需相除）
- **补充字段统一落在 `extra` JSON**（`cpu_freq_avg_mhz` / `battery_percent` /
  `battery_status` / `battery_temperature`），不污染 §4 冻结的主字段表
- 模拟模式同步生成上述字段，便于前端联调

### 前端

| 文件 | 作用 |
| --- | --- |
| `components/MetricChart.tsx` | 通用时序折线图：null 画断点、关动画 + merge 更新（防闪烁） |
| `utils/theme.ts` | 配色常量 + `withGaps()`（null 安全）+ 单端口 UTC 时间解析 |
| `utils/format.ts` | 补 `parseUtc` / `formatClock` / `formatDateTime` |
| `pages/Dashboard.tsx` | 重写为大屏：6 张汇总卡 + 使用率条 + 告警面板 + 集群趋势图 + 异常节点 + 节点列表（带指标列） |
| `pages/NodeDetail.tsx` | 重写：当前指标卡组 + 4 张历史曲线 + 时间范围切换 + 离线提示 |
| `index.css` | 离线行压暗、数值等宽（防刷新抖动） |

---

## 2. 验收结果

### 在线场景（`m2_check.py --phase online`）— 17/17

| 组 | 用例 | 结果 |
| --- | --- | --- |
| B1 | overview 字段完整（8 个顶层字段） | PASS |
| B1 | resources 汇总字段完整（13 项） | PASS |
| B1 | cluster-metrics 可读 | PASS |
| B1 | 无 token 访问被拒（401） | PASS |
| B2 | 参与统计节点数 ≤ 在线节点数 | PASS |
| B2 | 未参与统计的节点被标记 stale（口径可解释） | PASS |
| B2 | **无样本时均值为 null 而非 0（不造假）** | PASS |
| B2 | 汇总使用率与 used/total 自洽 | PASS |
| B4 | **最新采样持续推进**（12s 内前进 10.0s） | PASS |
| B4 | 采样时间单调递增 | PASS |
| B4 | **最近 1 分钟采样间隔稳定**（最小 5.0s / 最大 5.0s） | PASS |
| B7 | 拿不到的功耗保持 null | PASS |
| B7 | 补充指标落在 extra JSON | PASS |
| B7 | 新增采集项（CPU 频率/电池）已上报 | PASS |
| B6 | dist 存在 / 根路径返回 HTML / 非占位 JSON | PASS ×3 |

### 离线场景（`m2_check.py --phase offline`）— 16/16

停掉 phone-02 的 Agent 后：

| 用例 | 结果 | 实测数据 |
| --- | --- | --- |
| B3.1 存在 offline 节点 | PASS | — |
| **B3.2 离线节点当前指标为 null（后端不下发旧值）** | PASS | 离线 1 台，无指标泄漏 |
| **B3.3 离线节点不参与汇总** | PASS | counted **2→1**，online 1 |
| B3.4 离线节点进入 abnormal_nodes | PASS | abnormal=1 |
| B3.5 历史曲线仍可查（历史保留） | PASS | count=99 |
| B5.1 告警结构完整 | PASS | 1 条告警 |
| **B5.2 离线触发 node_offline 告警** | PASS | 1 条 |
| **B5.3 never_seen 不误报为 offline** | PASS | 0 误报 |
| B5.4 alert_summary 与明细一致 | PASS | `{critical:1, warning:0, total:1}` |

### 里程碑 1 回归 — 11/11

M2 的全部改动没有破坏 M1 的通信闭环、状态机与指标入库。

---

## 3. 关键设计决策（及其实测佐证）

### 3.1 「离线不残留旧数据」是**数据源头**的保证，不只靠前端

任务书 §4 要求区分四态、禁止"没数据就等于离线"糊弄；M2 验收要求
"对离线节点明确显示离线，不残留旧数据冒充在线"。

落地方式是**后端根本不下发旧值**：`/system/overview` 的 `nodes_metrics` 字段对
离线节点或采样陈旧（超过 `heartbeat_interval × 3 = 15s`）的节点直接返回 `null`。
前端拿不到旧数据，结构上就不可能把它显示成当前值 —— 而不是依赖前端"记得别显示"。

**佐证**：B3.2 实测"离线 1 台，无指标泄漏"；B3.3 实测 counted 从 2 降到 1。

### 3.2 「拿不到」与「值为 0」严格区分

- 后端：`_float_or_none` / `_int_or_none` 对空值与解析失败一律返回 `None`；
  `_avg()` 无样本返回 `None`；`percent()` 分母为 0 返回 `None`。
- 前端：`MetricCell` / 表格指标列对 `null` 显示「—」并置灰；
  图表用 `withGaps()` + `connectNulls: false` 画**断点**。

**佐证**：B2.3、B7.1；模拟器同样把 `power_watts` 设为 `None`（模拟数据也不编造功耗）。

### 3.3 防闪烁靠"关动画 + 增量更新 + 等宽数字"

- ECharts `animation: false` + `notMerge={false}`（merge 而非整图重绘）+ `lazyUpdate`
- KPI 卡手写大数字而不用 `Statistic` —— 后者的数字滚动动画在秒级刷新时观感是"闪"
- 数值列 `font-variant-numeric: tabular-nums`，等宽避免宽度跳动

**佐证**：B4.1/B4.3 —— 12 秒内最新采样前进 10.0s（连跑 3 次结果一致），
最近 12 个点间隔恒为 5.0s。

### 3.4 采样新鲜度（stale）单独判定

心跳正常不代表采集中断。若 Agent 主机负载打满导致采集卡住，心跳仍在但指标是断层的。
因此除"节点是否在线"外，另加"采样是否新鲜"（落后 >15s 即 stale），
stale 节点同样不进汇总、不下发指标，并在 `stale_nodes` 里列出，让口径可解释。

**佐证**：B2.2（口径可解释）；前端节点列表右上角会显示"N 台指标陈旧"。

### 3.5 告警阈值全部配置驱动，且"没数据 ≠ 异常"

`config.toml` 的 `[alerts]` 段含 7 项阈值（CPU/内存/存储/温度警告/温度严重/离线开关/探活次数）。
代码中无任何硬编码阈值。指标为 `None` 时不产生告警 —— 手机拿不到 NPU 使用率是常态，
不能因此刷屏。

**佐证**：B5.1~B5.4。

---

## 4. 本次修复的真实缺陷

**平台重启后节点 uptime 不归零（旧数据冒充在线的一种）**

- **现象**：平台重启后立即查节点列表，`uptime_seconds` 仍在累加（实测 phone-02 显示
  39734s ≈ 11 小时），而实际上它的 Agent 早就随关机一起没了。
- **根因**：`online_since` 存在库里。平台重启后若 Agent 在心跳巡检将其标 offline
  **之前**就重连上，`register()` 看到的 `prev_status` 仍是 `online`，于是跳过
  `online_since`/`uptime` 重置，在线时长跨重启继续累积。
- **修复**：新增 `NodeRegistry.reset_online_on_boot()`，在 `lifespan` 启动阶段
  把库里所有 `online` 强制降级为 `offline` 并清空 `online_since` / `uptime_seconds`。
  理由很直接：**平台刚启动时不可能持有任何 Agent 连接**，库里若还留着 online 就是假在线。
- **验证**：重启后实测 uptime 为 18s / 16s（从重连时刻重新计），不再累积历史值。

**验收脚本自身的一个判据错误（脚本缺陷，不是产品缺陷）**

诚实记录：`m2_check.py` 的 B4.1 最初写的是"12 秒内采样点数应增长"，连跑 3 次有 2 次失败：

```
[FAIL] B4.1 采样点持续增长（12s 内新增至少 1 个） — 114 → 114
[FAIL] B4.1 采样点持续增长（12s 内新增至少 1 个） — 114 → 113
```

- **根因**：`minutes=10` 是**滑动窗口**。稳态下新采样进来的同时，旧采样正滑出窗口，
  窗口内点数基本恒定；再叠加平台的 2s 降采样保护，点数还会小幅波动甚至减少。
  用"点数是否增长"来判断"是否还在刷新"，在稳态下必然随机失败。
- **修复**：判据改为**最新采样的时间戳是否推进**（12s 内前进 ≥5s）。
  这才是"持续刷新"的正确度量。
- **验证**：连跑 3 次全部 PASS，每次均前进 10.0s。

**产品本身没有问题** —— 采样一直在正常刷新，是断言方式选错了指标。
按 §8 要求区分"实测事实"与"判断"：上面两条都附了可复现的原始输出。

---

## 5. 已知取舍与遗留

1. **离线判定约 20–25s**（15s 阈值 + 2×5s 防抖），与任务书 §5「15s 内」存在张力。
   这是为满足 §5 里程碑 5「状态机防抖」有意引入的，已做成配置项
   （`offline_confirm_times = 1` 即严格 15s）。M1 起就在 README 写明，未擅自改数字。
2. **集群趋势图与汇总只覆盖在线节点**。若集群整体掉线，趋势图会显示无数据而非"最后曲线冻结"，
   这是刻意的（冻结曲线容易被误读为实时）。
3. **单文件 JS 体积 2.16MB**（引入 ECharts 后）。目前是局域网内网使用，未做 code-split；
   若后续要压，可用 `manualChunks` 把 echarts 拆出独立 chunk。
4. **服务列表恒为空**：M4 才实现服务托管，NodeDetail 的服务列表接口已就绪但无数据。
5. **`recent_deployments` 恒为空数组**：M3 起填充，结构已定，前端无需改。

---

## 6. 里程碑 3 入口（模型库 + 模型下发）

- 平台：模型上传 / 列表 / 删除 / 版本管理，显示大小/类型/框架
- 下发到多节点：Agent 走 HTTP 下载（**断点续传**、失败重试），下载后校验 sha256，上报真实进度
- 批量进度：逐节点独立状态，允许部分成功部分失败
- **验收**：上传大文件当"模型" → 下发 3 个模拟节点（其中一个模拟下载中断）→
  进度真实 → 失败节点可重试成功 → 成功节点不受影响

**开工第一步**：确认无残留进程 → 跑 `m1_check.py --phase online` 与
`m2_check.py --phase online` 确认前两个里程碑仍绿 → 开始 M3。

**本机操作提醒**（血泪经验）：停 Agent 必须用 PowerShell `Stop-Process` 直接杀
`python.exe`（按 CommandLine 精确匹配 `agent.py --name phone-0X`）；
如果用任务管理器的 stop，只会杀掉外层包装 shell，**Python 子进程会存活并继续发心跳**，
导致 `--phase offline` 恒失败。这是验收操作方式问题，不是产品缺陷。
