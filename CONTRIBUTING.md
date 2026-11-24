# 贡献指南

感谢你愿意参与这个项目。在动手之前，请先花几分钟读完本文档。

## 1. 这个项目现在处于什么阶段

**早期阶段（MVP）**，当前版本 `0.5.0-m5`，里程碑 1~5 全部完成：

- ✅ 里程碑 1：骨架 + 通信闭环（节点自动发现、注册、心跳、状态机）
- ✅ 里程碑 2：完整状态监控 + 首页大屏（指标采集、聚合、告警、历史曲线）
- ✅ 里程碑 3：模型库 + 模型下发（流式 sha256、`Range` 续传、兼容预检、9 态部署、取消与重试）
- ✅ 里程碑 4：服务管理 + API 暴露（7 态状态机、资源准入、看门狗、最简网关）
- ✅ 里程碑 5：稳定性收尾（指令幂等、服务自恢复、日志体系、全量验收）

另外补上了需求书里此前只写了一句的两块：**设备文件管理**与**操作审计查询**。

**明确不做**的四件事（不是没做完，是环境上做不了或不该做）：
公网 P2P/中继、设备重启关机、内建 HTTPS、用户权限体系 —— 原因见 [ROADMAP.md](ROADMAP.md)。

**在动手写代码之前，请先读 [`docs/design/requirement-review.md`](docs/design/requirement-review.md)。**
那份文档里有 10 条待决策项（O1~O10），其中 O1 / O2 / O4 / O6 会**改变数据库表结构或架构走向**，
改晚了要迁移数据。如果你要动的正好是这几块，请先在 Issue 里讨论清楚。

## 2. 提 Issue 之前

| 你想做的事 | 去处 |
|---|---|
| 报告 Bug | [Bug 报告](../../issues/new?template=bug_report.md) —— **请附上日志与复现步骤** |
| 提新功能 / 优化建议 | [功能建议](../../issues/new?template=feature_request.md) |
| 问"怎么用" | 先读 [`docs/user-manual.md`](docs/user-manual.md)（操作说明书），再开 Discussion |

**报告 Bug 时请附上这四样**，否则很难定位：

1. 平台版本（`curl http://127.0.0.1:8000/api/v1/system/health`）
2. 操作系统 + Python 版本（Windows 还是 Termux 真机？）
3. `server/data/logs/server.log` 的相关片段
4. 复现步骤（越具体越好，比如"起 2 个模拟节点，kill 掉 phone-02 后 20s 内查 overview"）

> ⚠️ **提交日志前请先脱敏**：删掉内网 IP、token、设备型号等你不想公开的信息。
> `server/config.toml` 里的 `token` 默认值是公开的示例值，如果你改过，别贴出来。

## 3. 提 PR 的流程

```bash
# 1. Fork + 克隆
git clone https://github.com/<你的用户名>/edge-cluster.git
cd edge-cluster

# 2. 建分支（分支名用 feat/ fix/ docs/ 前缀）
git checkout -b feat/model-store

# 3. 改代码

# 4. 跑验收（必做，见下节）
# 5. 提交
git commit -m "feat(server): 模型上传支持流式 sha256"
git push origin feat/model-store
```

然后在 GitHub 上开 Pull Request，说明你改了什么、为什么这么改、怎么验证的。

### 提交信息格式

参考 [Conventional Commits](https://www.conventionalcommits.org/)：

```
feat(server): 新增模型上传接口
fix(agent): 修复多实例 node_id 串用
docs(readme): 补充 Termux 真机接入步骤
refactor(core): Transport 抽象抽出重连策略
test(acceptance): m2_check 增加 stale 节点用例
```

作用域常用值：`server` / `agent` / `web` / `docs` / `scripts` / `acceptance`。

## 4. 改完代码必须跑的验收

**这个项目对"可验收"有硬要求** —— 每个里程碑都必须有可复现的验收脚本。
提 PR 前请至少跑通前两条：

```bash
# ① 起平台 + 2 个模拟节点
#    Windows:  .\start_all.ps1
#    Linux/macOS:  ./start_all.sh

# ② 配置层自检（秒级，不用起服务也行）
python docs/acceptance/config_check.py

# ③ 里程碑 1 验收（11 项）
python docs/acceptance/m1_check.py --phase online

# ④ 里程碑 2 验收（17 项）
python docs/acceptance/m2_check.py --phase online
```

**新增功能请同时新增验收用例**，并保证：

- 脚本**退出码**能反映结果（全通过 = 0）
- 断言基于**可复现的实测数据**，不要用"看起来对"的模糊判断
- 拿不到的指标要断言为 `null`，**不要**断言成 0

## 5. 几条这个项目特有的纪律

这几条是从已完成的两个里程碑里被反复踩出来的，**不是风格偏好，是有代价的约定**：

### 5.1 数据真实性：拿不到 ≠ 0

手机拿不到 NPU 使用率、拿不到功耗，这是常态。此时指标必须写 `NULL`，
前端显示「—」，图表画**断点**。

**禁止**用 0 补齐 —— 补 0 就是把"没测量"伪装成"测量结果是 0"。
这条在 `agent_metrics.py`、`overview.py`、`MetricChart.tsx` 三处都有对应实现，
改动时别破坏它。

### 5.2 状态四态严格区分，禁止"没数据 = 离线"

| 状态 | 含义 |
|---|---|
| `never_seen` | 从未上报（设备还没接进来） |
| `offline` | 曾上报过，但心跳超时（设备掉了） |
| `online` | 心跳在阈值内 |
| `stopped` | 服务进程不在（服务停了） |

把 `never_seen` 报成 `offline` 是**误报**，会让告警失去可信度。

### 5.3 离线时不下发旧值

`/system/overview` 对离线或采样陈旧（stale）的节点，`nodes_metrics` 直接返回 `null`。
这是**数据源头**的保证，不是靠前端"记得别显示"。

### 5.4 禁止硬编码模型

平台不应假设任何具体的模型、推理引擎或文件格式。
`node-agent/service_defs/*.json` 是服务定义模板，配置驱动。
新增引擎支持请走 `runtime` 适配层（见 O1），不要写成一堆 `if model_name == "xxx"`。

### 5.5 平台永不主动连接手机

手机可能在 NAT / 省电策略后，只有 Agent 主动拨出这一条路可靠。
平台→Agent 的指令一律复用 Agent 拨出的那条 WebSocket 连接。
**不要**为了图方便在平台侧加"主动连手机"的逻辑。

### 5.6 Windows 脚本编码规则（踩过坑）

| 文件类型 | 编码要求 | 原因 |
|---|---|---|
| `.ps1` | **UTF-8 with BOM** | 无 BOM 的中文 .ps1 会被 PowerShell 5.1 解析失败 |
| `.sh` | UTF-8 无 BOM + **LF** 换行 | 有 CRLF 会报 `bad interpreter` |
| `.bat` | **纯 ASCII** + CRLF，中文只出现在**文件名** | 避免 `chcp` 与代码页问题 |

改这三个脚本时请保持现状，别"顺手统一编码"。

## 6. 代码风格

**Python**（`server/`、`node-agent/`、`tools/`、`docs/acceptance/`）

- 目标 Python 3.10+，用 `from __future__ import annotations` 保持向前兼容
- 类型注解尽量写全；函数写简短 docstring 说明**为什么**这么做（不只是做什么）
- 配置一律进 `config.toml`，**代码里不写魔法数字**
- 日志用 `logging`，不用 `print`（Agent 的交互式输出除外）

**TypeScript / React**（`web/`）

- 提交前跑 `npm run typecheck`，必须零错误
- 组件用函数式 + hooks
- 图表相关改动注意**防闪烁三件套**：`animation: false`、merge 增量更新、等宽数字

**通用**

- 提交的文件不要带 BOM（除 `.ps1`）
- 换行符统一 LF（除 `.bat` 用 CRLF）
- 不提交运行时产物（`.venv/`、`node_modules/`、`server/data/`、`node-agent/state/`）

## 7. 许可

本项目采用 [MIT License](LICENSE)。提交 PR 即表示你同意你的贡献以同样的许可发布。
