# 里程碑 1 详细任务拆分：骨架 + 通信闭环

> 对应开发任务书 §10 要求第三项。
>
> **验收目标**：PC 上起平台 + 2 个 simulate 节点 → 自动发现并显示；kill 掉一个 Agent → 15s 内标记 offline；重启 Agent → 自动恢复 online。

---

## 任务分解

### T1.1 平台工程骨架（后端）
- [ ] `server/pyproject.toml` + `requirements.txt`：fastapi / uvicorn[standard] / sqlalchemy / pydantic / pydantic-settings / toml / python-multipart / httpx
- [ ] `server/app/config.py`：读 `config.toml`，支持 `EDGE_*` 环境变量覆盖，缺省值开箱即用
- [ ] `server/app/core/logging_conf.py`：结构化日志（时间/级别/模块/消息），同时输出控制台与 `data/logs/server.log`（轮转）
- [ ] `server/app/database.py`：SQLAlchemy 2.x engine + SessionLocal + `Base`；SQLite 开启 WAL
- [ ] `server/run.py`：uvicorn 启动入口
- [ ] `server/app/main.py`：FastAPI 应用装配，lifespan 内启动/关闭后台任务

**产出**：`python run.py` 能起服务，`GET /api/v1/system/health` 返回 ok。

### T1.2 数据模型与建表（对应 §4 + tech-decisions §2）
- [ ] `app/models/node.py`：Node（含三态 status、last_heartbeat、online_since）
- [ ] `app/models/metric.py`：MetricSample
- [ ] `app/models/model.py`：Model（骨架，M3 完善）
- [ ] `app/models/service.py`：Service（骨架，M4 完善）
- [ ] `app/models/deployment.py`：DeployTask + DeployTaskNode（骨架，M3 完善）
- [ ] `app/models/enums.py`：**冻结的状态枚举**（三端共用语义）
- [ ] 首次启动自动 `create_all`；`data/` 目录自动创建

**产出**：启动后 `data/edge_cluster.db` 生成，表齐全。

### T1.3 鉴权与连接层抽象（对应 §6 预留）
- [ ] `app/core/security.py`：共享 token 校验（WS query / `X-Auth-Token` 头）
- [ ] `app/core/transport.py`：`Transport` 抽象基类（`send_command` / `is_connected` / `close`）+ `LocalWsTransport` 实现
- [ ] `app/core/connection.py`：`ConnectionManager` 维护 `node_id → Transport`，提供 `send(node_id, msg)`、`broadcast(msg)`；发送失败不抛断业务

**产出**：连接层可替换；业务层只依赖抽象。

### T1.4 WebSocket 接入端点
- [ ] `app/api/ws.py`：`/ws/agent`
  - 握手校验 token（失败直接 close 1008）
  - 收 `register` → upsert Node，status=`online`，`online_since=now`，记 IP（优先上报值，兜底 WS peer）
  - 收 `heartbeat` → 更新 `last_heartbeat`、写 MetricSample、必要时 offline→online 恢复
  - 断连 → 标记连接失效（**不立即置 offline**，交给心跳巡检判定，避免抖动误判）
  - 异常隔离：单节点消息解析失败只记日志，不断开其他连接
- [ ] 命令下行封装：`send_command(node_id, type, payload, cmd_id)`，带超时与结果等待（M4 用）

**产出**：模拟节点能连上、能看到心跳进库。

### T1.5 心跳巡检（状态机核心）
- [ ] `app/services/heartbeat_monitor.py`：后台任务每 `heartbeat_interval`(5s) 扫描
  - `now - last_heartbeat > offline_threshold`(15s) → 计数 +1
  - **连续 `offline_confirm_times`(3) 次超时** → status=`offline`（防抖，M5 预埋）
  - 心跳恢复 → 同步计数清零、status=`online`
  - 任何状态变化必须打日志（§8 要求）
- [ ] 状态变化通过 `event_bus` 推给 Dashboard

**产出**：kill Agent 后 15s 内（+ 防抖余量）节点变 offline，重启后立即 online。

### T1.6 UDP 广播自动发现
- [ ] `app/services/discovery.py`：
  - UDP socket 绑定 `0.0.0.0:37020`，`SO_REUSEADDR`，`SO_BROADCAST`
  - 收 `EDGE_DISCOVER{token, agent_version}` → 校验 token → 回 `EDGE_DISCOVER_ACK{token, server_http, server_ws, server_name}`
  - 收到非法/无 token 包：静默丢弃 + debug 日志
  - 该任务独立线程/任务，异常自恢复，绝不拖垮主进程

**产出**：Agent 无需配置平台 IP 即可发现。

### T1.7 节点 REST API（最小集）
- [ ] `app/api/routes_nodes.py`：`GET /nodes`（支持 `?status=`）、`GET /nodes/{id}`、`PATCH /nodes/{id}`（改名）、`DELETE /nodes/{id}`
- [ ] `app/api/routes_system.py`：`/system/health`、`/system/info`（节点统计：总数/在线/离线/从未上线）
- [ ] `app/schemas/`：NodeOut / NodeListOut / SystemInfoOut

**产出**：Dashboard 有数据可拉。

### T1.8 Dashboard 实时推送
- [ ] `app/services/event_bus.py`：进程内 pub/sub
- [ ] `app/api/ws.py`：`/ws/dashboard` — 订阅事件，推 `node_online` / `node_offline` / `node_updated`

**产出**：前端无需轮询即可实时反映上下线。

### T1.9 手机端 Agent 骨架
- [ ] `node-agent/agent.py`：
  - 配置加载（toml）+ 环境变量覆盖；`node_id` 留空自动生成（uuid4 短码）并回写文件
  - `--simulate` / `--name` / `--config` 命令行参数
  - 设备信息采集（真机：`getprop` 系列；模拟器：预设合理值）
  - UDP 发现（3 次广播，间隔 1s；有结果或超时后回落到配置地址）
  - WS 连接 + `register` 首帧
  - 心跳循环（5s）发送 `heartbeat`（M1 带基础指标占位）
  - 指数退避重连：1→2→4→8→…→60s，±20% 抖动
  - 收到 `ping` 回 `pong`
  - 结构化日志（中文注释，标识符英文）
- [ ] `node-agent/config.toml`
- [ ] `node-agent/requirements.txt`

**产出**：`python agent.py --simulate --name phone-01` 可跑。

### T1.10 模拟器基础能力
- [ ] `--simulate` 下：生成合理假指标（CPU/NPU 随负载波动、温度随 CPU 变化、内存缓慢变化）
- [ ] 模拟设备信息：随机/可指定型号（如 `Xiaomi 13` / `Snapdragon 8 Gen 2` / Android 14）
- [ ] 预留模拟开关（M3/M4 用）：`fail_download_once`、`crash_service_after`、`disconnect_after`（M1 仅解析配置不启用）
- [ ] 支持多实例并存（`node_id` 按 name 派生，或用户指定）

**产出**：可同时跑 2~3 个模拟节点。

### T1.11 Web Dashboard 工程 + 最小页
- [ ] `web/`：Vite + React 18 + TS + Ant Design + ECharts + axios
- [ ] `vite.config.ts`：`/api` 与 `/ws` proxy 到 `http://127.0.0.1:8000`
- [ ] `src/api.ts`：API 封装
- [ ] `src/pages/Dashboard.tsx`（M1 最小版）：节点实时列表（名称/型号/IP/状态标签/最后心跳/在线时长）
- [ ] `src/api/ws`：连 `/ws/dashboard`，收事件刷新列表
- [ ] `src/components/NodeStatusTag.tsx`：三态颜色区分（online 绿 / offline 红 / never_seen 灰）
- [ ] 离线节点**明确显示"离线"**，最后心跳时间照实显示（不残留旧数据显示为实时）

**产出**：浏览器打开即可看到自动发现的两个节点。

### T1.12 一键启动脚本 + README
- [ ] `start_all.ps1` / `start_all.sh`：建 venv、装依赖、起平台、（可选）起 N 个模拟节点
- [ ] `README.md`：项目说明 + 里程碑 1 快速验证步骤

---

## 依赖顺序

```
T1.1 ─┬─> T1.2 ─┬─> T1.4 ──> T1.5 ──┐
      │         │                    ├─> T1.11 ─> T1.12
      ├─> T1.3 ─┘                    │
      ├─> T1.6 ─────────────────────>│
      └─> T1.7 ──────────────────────┤
                     T1.8 ───────────┤
                                     │
      T1.9 ─> T1.10 ─────────────────┘
```

---

## 里程碑 1 验收脚本设计

`docs/acceptance/m1_check.py` 自动断言：

| 步骤 | 动作 | 断言 |
|---|---|---|
| A1 | 平台启动 | `/api/v1/system/health` == ok |
| A2 | 起 2 个 simulate 节点 | 15s 内 `/nodes` 返回 2 条，status 全 `online` |
| A3 | 校验字段 | 每条含 node_id/name/model/soc/android_version/ip/last_heartbeat |
| A4 | kill 掉 1 个 Agent | 20s 内该节点 status == `offline`，另一节点仍 `online` |
| A5 | 离线节点数据检查 | `last_heartbeat` 保留（不置空），`online_since` 不清零误用 |
| A6 | 重启该 Agent | 10s 内 status 回到 `online`，且**不产生重复节点记录**（node_id 复用） |
| A7 | UDP 发现 | 日志中存在 `UDP_DISCOVER_ACK` 记录（Agent 侧或平台侧） |
| A8 | 重连退避 | 日志中可见退避序列递增（1s→2s→4s...） |

**手工核验项**：
- Dashboard 页面上下线状态 5s 内刷新，无闪烁、无旧数据冒充在线。
- 平台日志中状态变化均有 INFO/WARN 级记录。
