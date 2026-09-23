# 手机边缘计算集群管理平台 — 架构设计（第一阶段 MVP）

> 本文档对应开发任务书 §10 要求的第一项：**文字版架构图（三端 + 通信方式 + 端口规划）**。

---

## 1. 总体架构图（文字版）

```
                    ┌──────────────────────────────────────────────────────┐
                    │        平台服务端 Platform Server (PC / Linux)        │
                    │        单进程 · FastAPI + uvicorn · SQLite            │
                    │                                                      │
                    │  ┌────────────┐ ┌────────────┐ ┌──────────────────┐  │
                    │  │ REST API   │ │ WebSocket  │ │ UDP 发现监听器    │  │
                    │  │ /api/v1/** │ │ /ws/agent  │ │ :37020 broadcast │  │
                    │  └─────┬──────┘ └─────┬──────┘ └────────┬─────────┘  │
                    │        │              │                 │            │
                    │  ┌─────▼──────────────▼─────────────────▼─────────┐  │
                    │  │              Service Layer (业务服务层)          │  │
                    │  │  NodeRegistry  HeartbeatMonitor  ModelStore    │  │
                    │  │  DeployManager  ServiceManager   API_REGISTRY  │  │
                    │  └─────────────────────┬─────────────────────────┘  │
                    │                        │                            │
                    │  ┌─────────────────────▼─────────────────────────┐  │
                    │  │  Connection Layer (连接层抽象，可替换)          │  │
                    │  │  ConnectionManager / Transport ABC            │  │
                    │  │  └─ LocalWsTransport（MVP 实现）               │  │
                    │  │     [预留] P2PTransport / RelayTransport       │  │
                    │  └─────────────────────┬─────────────────────────┘  │
                    │                        │                            │
                    │  ┌─────────────────────▼─────────────────────────┐  │
                    │  │  SQLite  data/edge_cluster.db                  │  │
                    │  │  + 模型文件库  data/models/                     │  │
                    │  │  + 平台日志    data/logs/server.log            │  │
                    │  └───────────────────────────────────────────────┘  │
                    └───────────┬──────────────────────┬──────────────────┘
                                │                      │
        ┌───────────────────────┘                      └───────────────────────┐
        │                                                                      │
        │ ① WebSocket 长连接（Agent 主动拨出，平台永不主动连手机）                 │
        │    心跳上行 / 指令下行                                                 │
        │ ② HTTP（平台 → Agent 无；Agent → 平台有）                              │
        │    模型文件下载（Range 断点续传）                                      │
        │ ③ UDP 广播（平台 ↔ Agent 双向，仅用于自动发现）                          │
        │                                                                      │
        ▼                                                                      ▼
┌───────────────────────────────┐                    ┌───────────────────────────────┐
│  节点 Agent（Android/Termux）  │                    │  节点模拟器（PC，--simulate）  │
│  node-agent/agent.py          │                    │  同一份代码，同一套协议         │
│                               │                    │                               │
│  ┌─────────────────────────┐  │                    │  ┌─────────────────────────┐  │
│  │ HeartbeatLoop  5s       │  │                    │  │ 生成合理假指标           │  │
│  │ MetricsCollector        │  │                    │  │ 可模拟 崩溃/断线/慢下载  │  │
│  │  /proc · termux-api ·   │  │                    │  │ 服务进程 = 真实子进程    │  │
│  │  dumpsys（拿不到留空）    │  │                    │  │ (OpenAI 兼容 mock)      │  │
│  └─────────────────────────┘  │                    │  └─────────────────────────┘  │
│  ┌─────────────────────────┐  │                    └───────────────────────────────┘
│  │ CommandDispatcher       │  │
│  │  deploy_model /         │  │
│  │  start|stop|restart_svc /│  │
│  │  update_params / logs   │  │
│  └───────────┬─────────────┘  │
│  ┌───────────▼─────────────┐  │
│  │ ServiceSupervisor       │  │
│  │  子进程管理 + 健康检查    │  │
│  │  崩溃自动拉起 + 日志滚动  │  │
│  └─────────────────────────┘  │
└───────────────┬───────────────┘
                │
                │ ④ 模型服务监听端口（如 8080）
                ▼
   ┌──────────────────────────────────────────┐
   │ 模型服务进程（配置驱动，非硬编码）          │
   │ llama-server / mock-openai / 任意 HTTP    │
   │ 平台通过 http://节点IP:端口/v1/chat/... 调用│
   └──────────────────────────────────────────┘

                ┌───────────────────────────────┐
                │  Web Dashboard (React + Vite)  │
                │  dev: 5173 → proxy /api /ws    │
                │  prod: 构建产物由平台静态托管    │
                └───────────────────────────────┘
                             │ HTTP /api/v1/** + WS /ws/dashboard
                             ▼
                       平台服务端 :8000
```

### 1.1 通信方式总表

| 编号 | 方向 | 协议 | 用途 | 谁主动 |
|---|---|---|---|---|
| ① | Agent → 平台 | WebSocket | 注册、心跳、指令结果上行 | **Agent 主动拨出** |
| ① | 平台 → Agent | WebSocket（复用同一连接） | 部署/启停/改参/取日志指令下行 | 平台（借已建连接） |
| ② | Agent → 平台 | HTTP(S) GET | 模型文件下载，支持 `Range` 断点续传 | Agent |
| ③ | 平台 ↔ Agent | UDP 广播 | 局域网自动发现（平台回包告知地址+端口） | 双方均可广播，平台响应 |
| ③ | 人工兜底 | — | 手工填平台 IP，跳过发现 | 运维 |
| ④ | 平台 → 节点 | HTTP | 探活模型服务 + OpenAI 兼容调用 | 平台 |
| ⑤ | 浏览器 → 平台 | HTTP + WebSocket | Dashboard 数据与实时推送 | 浏览器 |

**关键约束**：平台**永远不主动连接手机**。手机可能在 NAT / 省电策略后，只有 Agent 拨出方向是可靠的。平台→Agent 的一切指令都复用 Agent 拨出的那条 WS 连接。

### 1.2 端口规划

| 端口 | 归属 | 协议 | 可配置项 | 说明 |
|---|---|---|---|---|
| **8000** | 平台 | TCP HTTP/WS | `server.port` | REST API、`/ws/agent`、`/ws/dashboard`、静态托管 Web |
| **37020** | 平台 | UDP | `server.discovery_port` | 发现服务监听（收广播 + 回包） |
| **37020** | Agent | UDP | `discovery.port` | Agent 广播发现请求；与平台同端口号便于记忆 |
| **37100+** | Agent | TCP | `agent.metrics_port` | 节点指标拉取兜底口（预留，MVP 仅注册不启用） |
| **8080 / 8081 / ...** | 模型服务 | TCP | 服务定义 `port` 字段（**非硬编码**） | 由平台分配/校验冲突，Agent 拉起进程监听 |
| **5173** | Web 开发态 | TCP | Vite 默认 | 仅 `npm run dev` 时；已配 proxy 到 8000 |

**端口冲突策略**：Agent 启动服务前先做本地端口占用探测；平台在分配端口时校验「节点内」唯一性（同一节点不同服务端口不重复）。

---

## 2. 数据流说明

### 2.1 节点上线（里程碑 1）

```
Agent 启动
  → 读 config.toml
  → UDP 广播 DISCOVER(platform_port_hint) 到 255.255.255.255:37020  【3 次，间隔 1s】
  → 平台 UDP 监听器收到 → 回 DISCOVER_ACK{server_http, server_ws}
  → Agent 得到平台地址（或直接用配置里的 platform_url，跳过发现）
  → WS 连接 ws://<平台>/ws/agent?token=<共享token>
  → 首帧 register{node_id, name, model, soc, android_version, agent_version, ip}
  → 平台 upsert 节点 → 状态 never_seen → online
  → 进入心跳循环：每 5s 发 heartbeat{metrics}
  → 平台每次心跳：更新 last_heartbeat、写 MetricsSample、标记 online
断线：指数退避重连（1,2,4,8,...最大 60s，带 ±20% 抖动）
平台侧：HeartbeatMonitor 每 5s 扫描，now - last_heartbeat > 15s → offline
```

### 2.2 模型下发（里程碑 3）

```
平台 UI 选模型 + 选多节点 → POST /deployments
  → 建 DeployTask + 每节点 DeployTaskNode (状态 pending)
  → 经 WS 下发 deploy_model{task_id, model_id, download_url, sha256, size, dest}
Agent 收到
  → 上报 downloading(pct)  ← 真实进度，来自已写字节数 / 总大小
  → HTTP GET download_url，断点续传（本地 .part 文件 + Range）
  → 下载完 → verifying → 流式算 sha256 比对
  → sha256 不符 → failed（原因：checksum_mismatch），允许重试
  → 通过 → installing（落位到 models_dir）→ ready(100%)
  → 过程中每 1s 上报一次进度
平台：DeployManager 持久化各节点状态；部分失败不影响其他节点；RETRY 接口只重投失败节点
```

### 2.3 服务启停（里程碑 4）

```
平台 → WS 下发 start_service{service_id, model_id, command, args[], port, health_url, api_type}
Agent ServiceSupervisor
  → 状态 starting → Popen(子进程) → 轮询 health_url
  → 健康检查通过 → running，上报 {pid, started_at, endpoint}
  → 健康检查超时(默认 60s) → error，上报 last_error
  → 子进程崩溃 → 若 restart_policy=always → 自动拉起 + 记日志；否则 → stopped/error
  → 上报 service_state
平台 ServiceManager
  → 落库；running → 登记 Endpoint（http://ip:port + api_type）
  → 定时探活（默认 15s）→ 更新调用状态
  → 平台自身提供 OpenAI 兼容调用入口 POST /api/v1/services/{id}/invoke 做端到端验证
stop → terminate → 等 5s → kill → stopped（幂等：已是 stopped 直接返回 no-op）
```

### 2.4 状态机（§4 重点，四态严格区分）

```
节点 Node.status         : never_seen → online ↔ offline
                            never_seen = 从未收到过心跳（“没有上报”）
                            online     = 最近一次心跳在 timeout 内
                            offline    = 曾上线，但心跳超时（“节点离线”）

服务 Service.status      : stopped → starting → running
                                     ↘         ↘
                                      error ←———┘
                            stopped = 进程不在（“服务停止”，含人工停止）
                            starting= 已拉起进程，健康检查尚未通过
                            running = 进程在 且 健康检查通过
                            error   = 进程在 但 健康检查连续失败（“服务异常”）
```

**明确禁止**：用「没数据」直接等同于「离线」。节点离线时 Dashboard 保留最后心跳时间并显式标注离线，不把旧指标当实时值展示。

---

## 3. 安全与鉴权（MVP）

- 共享 token：平台 `server.token` 与 Agent `agent.token` 一致；WS 握手 query 带 token，REST 用 `X-Auth-Token` 头；UDP 发现包带 token，不一致不回包。
- 不做用户系统、不做 HTTPS（§7 明确排除）。

---

## 4. 架构预留（只做抽象，不实现，§6）

| 预留点 | 实现方式 | 未来替换 |
|---|---|---|
| 连接层 | `app/core/transport.py` 定义 `Transport` 抽象基类 + `LocalWsTransport` | P2PTransport / RelayTransport 直接替换，业务层无感 |
| API Gateway | `services` 表预留 `gateway_meta` JSON 字段（协议、路径模板、限流占位、权重、标签） | 后续接入统一转发与调度 |
| 硬件电源控制 | **不涉及**，代码中任何位置不得假设硬件存在 | — |
```
