# 技术细节决策（字段表 / 状态枚举 / API 清单 / 目录结构）

> 对应开发任务书 §10 要求第二项。**本文档一经确认即为三端共同契约**。
>
> 📖 其他文档见 [文档索引](../README.md) ｜ [架构设计](architecture.md) ｜ [优化规格](optimized-spec.md)

---

## 1. 最终目录结构

> 注：本节的目录树是**原始设计**。仓库开源后 `docs/` 做了子目录归并
> （`design/` / `milestones/` / `acceptance/`），并新增了
> `LICENSE`、`CONTRIBUTING.md`、`CHANGELOG.md`、`ROADMAP.md`、`SECURITY.md` 等开源文件。
> **以 [README 的项目结构章节](../../README.md#项目结构) 为准。**

```
edge-cluster/
├── README.md                     # 总入口：快速开始 + 里程碑状态
├── start_all.ps1                 # PC 端一键启动（Windows）
├── start_all.sh                  # PC 端一键启动（Linux/macOS）
├── server/                       # ① FastAPI 平台服务端
│   ├── pyproject.toml
│   ├── requirements.txt
│   ├── config.toml               # 平台配置（地址/端口/token/路径）
│   ├── run.py                    # 启动入口
│   ├── data/                     # 运行时数据（SQLite / 模型文件 / 日志）
│   │   ├── edge_cluster.db
│   │   ├── models/
│   │   └── logs/
│   └── app/
│       ├── main.py               # FastAPI 应用装配 + 生命周期
│       ├── config.py             # 配置加载（toml + 环境变量覆盖）
│       ├── database.py           # SQLAlchemy engine/session
│       ├── models/               # ORM 表定义（§4 字段表落地）
│       │   ├── node.py  metric.py  model.py
│       │   ├── service.py  deployment.py
│       ├── schemas/              # Pydantic 出入参
│       ├── core/
│       │   ├── logging_conf.py   # 结构化日志
│       │   ├── security.py       # 共享 token 校验
│       │   ├── transport.py      # ★ 连接层抽象（预留 P2P/中继）
│       │   └── connection.py     # ConnectionManager（WS 连接池 + 指令下发）
│       ├── services/
│       │   ├── node_registry.py      # 节点注册 / 上下线
│       │   ├── heartbeat_monitor.py  # 心跳超时巡检 → offline
│       │   ├── discovery.py          # UDP 广播发现监听
│       │   ├── model_store.py        # 模型库（上传/校验/存储）
│       │   ├── deploy_manager.py     # 部署任务编排
│       │   ├── service_manager.py    # 服务记录 + Endpoint 登记 + 探活
│       │   └── event_bus.py          # 内部事件 → 推送 Dashboard
│       └── api/
│           ├── deps.py
│           ├── routes_nodes.py
│           ├── routes_models.py
│           ├── routes_deployments.py
│           ├── routes_services.py
│           ├── routes_system.py      # 健康检查 / 概览统计 / 日志
│           └── ws.py                 # /ws/agent  +  /ws/dashboard
├── web/                          # ② React Dashboard
│   ├── package.json  vite.config.ts  tsconfig.json  index.html
│   └── src/
│       ├── main.tsx  App.tsx  api.ts  types.ts
│       ├── components/  (StatCard / NodeStatusTag / ServiceStatusTag ...)
│       └── pages/       (Dashboard / Nodes / NodeDetail / Models / Deployments / Services)
├── node-agent/                   # ③ 手机端 Agent（可整目录拷进 Termux）
│   ├── agent.py                  # 主程序（真机 & --simulate 共用）
│   ├── config.toml               # Agent 配置
│   ├── requirements.txt
│   ├── install.sh                # Termux 一键安装 + wake-lock + boot 自启
│   ├── service_defs/             # 服务定义样例（配置驱动，非硬编码）
│   │   └── mock_openai.json
│   └── mock_openai_server.py     # OpenAI 兼容 mock 服务（验收用）
└── docs/
    ├── architecture.md           # 架构图 + 通信 + 端口
    ├── tech-decisions.md         # 本文档
    ├── milestone-1-plan.md       # 里程碑 1 任务拆分
    ├── deployment.md             # 部署文档（PC + Termux）
    └── acceptance-report.md      # 验收报告（里程碑 5 输出）
```

---

## 2. 字段表（三端共用契约，严格按 §4）

### 2.1 Node 节点

| 字段 | 类型 | 说明 | 来源 |
|---|---|---|---|
| `node_id` | TEXT PK | 唯一编号，Agent 首次生成后持久化 | Agent |
| `name` | TEXT | 显示名，可在平台改 | Agent 注册 / 平台编辑 |
| `model` | TEXT | 型号，如 `Xiaomi 13` | Agent |
| `soc` | TEXT | SoC，如 `Snapdragon 8 Gen 2` | Agent |
| `android_version` | TEXT | Android 版本，如 `14` | Agent |
| `agent_version` | TEXT | Agent 版本 | Agent |
| `ip` | TEXT | 最后可见 IP | Agent 上报 / WS peer 兜底 |
| `status` | ENUM | `never_seen` / `online` / `offline` | 平台 |
| `last_heartbeat` | DATETIME | 最后心跳时间 | 平台 |
| `online_since` | DATETIME | 本次在线起始（用于算在线时长） | 平台 |
| `uptime_seconds` | INT | 在线时长（秒） | 平台计算 |
| `created_at` / `updated_at` | DATETIME | 审计 | 平台 |

### 2.2 MetricSample 指标

| 字段 | 类型 | 说明（拿不到则 NULL，**不造假**） |
|---|---|---|
| `id` | INT PK | |
| `node_id` | TEXT FK | |
| `ts` | DATETIME | 采样时间（索引）|
| `cpu_usage` | FLOAT | CPU 使用率 % |
| `gpu_usage` | FLOAT | GPU % |
| `npu_usage` | FLOAT | NPU % |
| `mem_used` / `mem_total` | INT | 字节 |
| `storage_used` / `storage_total` | INT | 字节 |
| `temperature` | FLOAT | ℃ |
| `power_watts` | FLOAT | W（拿不到留空）|
| `net_rx_bytes` / `net_tx_bytes` | INT | 累计流量（平台算速率）|
| `extra` | JSON | 扩展位（预留）|

### 2.3 Model 模型

| 字段 | 类型 | 说明 |
|---|---|---|
| `model_id` | TEXT PK | |
| `name` | TEXT | |
| `version` | TEXT | 版本管理用，`name+version` 唯一 |
| `file_size` | INT | 字节 |
| `framework` | ENUM | `llama.cpp` / `onnx` / `mnn` / `qnn` / `other` |
| `format` | TEXT | `gguf` / `onnx` / `mnn` / `bin` … |
| `sha256` | TEXT | 上传时平台流式计算 |
| `description` | TEXT | |
| `uploaded_at` | DATETIME | |
| `storage_path` | TEXT | 平台侧相对路径 `models/<id>/<filename>` |

### 2.4 Service 服务

| 字段 | 类型 | 说明 |
|---|---|---|
| `service_id` | TEXT PK | |
| `node_id` | TEXT FK | 所属节点 |
| `model_id` | TEXT FK NULL | 所用模型 |
| `name` | TEXT | 服务名（配置驱动） |
| `status` | ENUM | `stopped` / `starting` / `running` / `error` |
| `port` | INT | 监听端口 |
| `endpoint` | TEXT | `http://<node_ip>:<port><path>` |
| `api_type` | ENUM | `llm` / `vlm` / `vision` / `ocr` / `other` |
| `pid` | INT NULL | |
| `started_at` | DATETIME NULL | |
| `last_error` | TEXT NULL | 最近一次错误信息 |
| `last_probe_at` | DATETIME | 平台最近探活 |
| `probe_ok` | BOOL | 探活结果 |
| `command` / `args` | TEXT / JSON | 启动命令与参数（平台可改） |
| `health_url` | TEXT | 健康检查 URL |
| `log_path` | TEXT | 节点侧日志路径 |
| `auto_restart` | BOOL | 崩溃自动拉起 |
| `gateway_meta` | JSON | ★ 预留 Gateway 调度元信息 |

### 2.5 DeployTask 部署任务

**DeployTask**：`task_id` PK、`model_id`、`target_count`、`status`（聚合）、`created_at`、`created_by`、`note`

**DeployTaskNode**（逐节点子状态）：
`id` PK、`task_id` FK、`node_id` FK、`status` ENUM、`progress` INT 0-100、`error` TEXT、`retry_count` INT、`bytes_downloaded`、`started_at`、`finished_at`

---

## 3. 状态枚举（冻结）

```python
# 节点
class NodeStatus(str, Enum):
    NEVER_SEEN = "never_seen"
    ONLINE     = "online"
    OFFLINE    = "offline"

# 服务
class ServiceStatus(str, Enum):
    STOPPED  = "stopped"
    STARTING = "starting"
    RUNNING  = "running"
    ERROR    = "error"

# 部署（逐节点）
class DeployNodeStatus(str, Enum):
    PENDING     = "pending"
    DOWNLOADING = "downloading"
    VERIFYING   = "verifying"
    INSTALLING  = "installing"
    READY       = "ready"
    FAILED      = "failed"

# 部署（任务聚合）
class DeployTaskStatus(str, Enum):
    PENDING   = "pending"
    RUNNING   = "running"
    PARTIAL   = "partial"     # 部分成功
    SUCCEEDED = "succeeded"
    FAILED    = "failed"

# API 类型
class ApiType(str, Enum):
    LLM="llm"; VLM="vlm"; VISION="vision"; OCR="ocr"; OTHER="other"

# 框架
class Framework(str, Enum):
    LLAMA_CPP="llama.cpp"; ONNX="onnx"; MNN="mnn"; QNN="qnn"; OTHER="other"
```

**状态判定规则**

| 状态 | 判定条件 |
|---|---|
| `never_seen` | 该 node_id 从未有过心跳 |
| `offline` | 曾有心跳，`now - last_heartbeat > offline_threshold`（默认 15s） |
| `online` | `now - last_heartbeat <= offline_threshold` |
| `stopped` | 子进程不存在（含人工停止） |
| `starting` | 进程已拉起，健康检查尚未通过（超时前） |
| `running` | 进程存在 **且** 健康检查通过 |
| `error` | 进程存在 **但** 健康检查连续失败 ≥ N 次（默认 3）或启动超时 |

---

## 4. API 清单（/api/v1）

### 4.1 系统

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/v1/system/health` | 存活探针（无需鉴权） |
| GET | `/api/v1/system/info` | 版本、启动时间、节点统计 |
| GET | `/api/v1/system/overview` | ★ 首页大屏聚合（节点统计 + 资源汇总 + 告警 + 异常节点 + 各节点指标） |
| GET | `/api/v1/system/cluster-metrics?minutes=30` | ★ 集群资源时间序列（大屏趋势图） |
| GET | `/api/v1/system/server-logs?lines=200` | 平台自身日志 |

### 4.2 节点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/v1/nodes` | 列表（支持 status 过滤） |
| GET | `/api/v1/nodes/{node_id}` | 详情 |
| PATCH | `/api/v1/nodes/{node_id}` | 改名等 |
| DELETE | `/api/v1/nodes/{node_id}` | 删除节点记录 |
| GET | `/api/v1/nodes/{node_id}/metrics?minutes=30&limit=500` | 指标历史（曲线用） |
| GET | `/api/v1/nodes/{node_id}/metrics/latest` | 最新指标 |
| GET | `/api/v1/nodes/{node_id}/services` | 节点上服务列表 |
| GET | `/api/v1/nodes/{node_id}/logs?lines=200` | 节点 Agent 日志（M5） |

> **实现进度**：4.1 / 4.2 已于里程碑 1、2 实现；4.3~4.5 分别属里程碑 3、4。
> `overview.nodes_metrics` 字段对**离线或采样陈旧**的节点返回 `null` —— 这是
> 「不残留旧数据冒充在线」在数据源头的落地方式。

### 4.3 模型库

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/v1/models` | 列表 |
| POST | `/api/v1/models` | 上传（multipart，流式算 sha256） |
| GET | `/api/v1/models/{model_id}` | 详情 |
| DELETE | `/api/v1/models/{model_id}` | 删除（含文件） |
| GET | `/api/v1/models/{model_id}/download` | ★ Agent 下载口，支持 `Range` |

### 4.4 部署

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/v1/deployments` | 创建任务 `{model_id, node_ids[]}` |
| GET | `/api/v1/deployments` | 任务列表 |
| GET | `/api/v1/deployments/{task_id}` | 任务详情（逐节点状态） |
| POST | `/api/v1/deployments/{task_id}/retry` | 重投失败节点 |
| POST | `/api/v1/deployments/{task_id}/nodes/{node_id}/retry` | 单节点重试 |

### 4.5 服务管理

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/v1/services` | 服务列表 |
| POST | `/api/v1/services/start` | 启动 `{node_id, service_def, model_id}` |
| POST | `/api/v1/services/{service_id}/stop` | 停止（幂等） |
| POST | `/api/v1/services/{service_id}/restart` | 重启（幂等语义：以最新参数重启） |
| PATCH | `/api/v1/services/{service_id}/params` | 改启动参数 |
| GET | `/api/v1/services/{service_id}/logs?lines=200` | 服务日志 |
| POST | `/api/v1/services/{service_id}/probe` | 手动探活 |
| POST | `/api/v1/services/{service_id}/invoke` | ★ OpenAI 兼容调用（端到端验收） |
| GET | `/api/v1/service-defs` | 可用服务定义模板（配置驱动） |

### 4.6 WebSocket

| 路径 | 方向 | 说明 |
|---|---|---|
| `/ws/agent?token=` | Agent ↔ 平台 | 注册/心跳/指令下发/结果上报 |
| `/ws/dashboard?token=` | 浏览器 ↔ 平台 | 实时节点变动、部署进度、服务状态推送 |

### 4.7 WS 消息协议（`type` 字段区分）

**Agent → 平台**：`register`、`heartbeat`、`metrics`、`deploy_progress`、`service_state`、`log_chunk`、`ack`、`pong`

**平台 → Agent**：`register_ack`、`heartbeat_ack`、`ping`、`deploy_model`、`cancel_deploy`、`start_service`、`stop_service`、`restart_service`、`update_service_params`、`fetch_logs`、`fetch_agent_logs`、`list_services`、`collect_metrics`、`simulate`（模拟器专用：注入崩溃/断线/慢下载）

---

## 5. 关键技术决策与理由

| 决策点 | 选择 | 理由 |
|---|---|---|
| 通信主通道 | Agent 主动 WS | 手机在 NAT/省电后，反向连接不可靠（任务书 §3 锁定） |
| 大文件传输 | 走 HTTP + Range | WS 传大文件会阻塞心跳；HTTP 天然支持断点续传 |
| 发现方式 | UDP 广播 | 比 mDNS 依赖少，Termux 下更可控；配手工 IP 兜底 |
| 数据库 | SQLite + SQLAlchemy 2.x | MVP 单文件，任务书 §8 锁定；后续可换 PG |
| 指标历史 | 定长采样表 + 保留上限清理 | 避免无限膨胀；默认保留 7 天 |
| 状态防抖 | offline 判定加「连续 3 次超时」 | 心跳抖动容错（M5），避免网络抖动误判离线 |
| 指令幂等 | 每条指令带 `cmd_id`，Agent 侧 LRU 去重 | 重复下发不产生副作用（M5） |
| 服务进程管理 | `subprocess.Popen` + 进程组 | Termux 免 root 唯一可行方案；不假设 systemd |
| 模型服务 | 配置驱动 `service_defs/*.json` | 任务书 §3 硬约束：**禁止硬编码任何模型** |
| 模拟器 | 同一份 `agent.py` + `--simulate` | 避免两套代码漂移；真机/模拟器协议完全一致 |
| 前端 | Vite dev + 平台静态托管生产 | 开发方便，交付时单端口，无额外部署负担 |

---

## 6. 配置文件项（全部可调，默认开箱即用）

**server/config.toml**
```toml
[server]
host = "0.0.0.0"
port = 8000
token = "edge-cluster-dev-token"
discovery_port = 37020
offline_threshold = 15        # 秒，心跳超时判离线
offline_confirm_times = 3     # 防抖：连续 N 次超时才标 offline
heartbeat_interval = 5        # 期望心跳间隔（秒，平台侧巡检周期）
service_probe_interval = 15   # 服务探活周期（秒）
metric_retention_days = 7
log_level = "INFO"

[data]
db_path = "data/edge_cluster.db"
models_dir = "data/models"
logs_dir = "data/logs"

[web]
serve_static = true           # 是否托管 web/dist
```

**node-agent/config.toml**
```toml
[agent]
node_id = ""                  # 留空则自动生成并回写
name = ""
token = "edge-cluster-dev-token"
heartbeat_interval = 5
platform_url = ""             # 留空则走 UDP 发现；填了则直连
platform_port = 8000

[discovery]
enabled = true
port = 37020
broadcast_addr = "255.255.255.255"

[reconnect]
base_delay = 1.0
max_delay = 60.0
jitter = 0.2

[storage]
models_dir = "./models"
logs_dir = "./logs"
log_max_bytes = 5242880       # 5MB 滚动
log_backup_count = 3

[service]
health_timeout = 60           # 启动健康检查等待上限（秒）
health_fail_threshold = 3     # 连续失败 N 次判 error
restart_backoff = 3           # 崩溃拉起退避（秒）

[simulate]
enabled = false               # --simulate 时置 true
fail_download_once = false    # 模拟一次下载中断
crash_service_after = 0       # >0 时服务启动 N 秒后模拟崩溃
```

---

## 7. 验收命令约定（每个里程碑复用）

```bash
# 平台
cd server && python run.py

# 模拟节点
cd node-agent && python agent.py --simulate --name phone-01
cd node-agent && python agent.py --simulate --name phone-02

# 断言脚本
python docs/acceptance/m1_check.py
```
