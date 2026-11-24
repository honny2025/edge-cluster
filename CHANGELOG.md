# 更新日志

本文件记录所有值得注意的变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> **版本号约定**：`0.<里程碑>-<补丁>`。当前 `0.5.0-m5` 表示「已交付里程碑 5」。

---

## [Unreleased]

### 计划中
- 真实公网连接层（P2P 打洞 / 中继）—— 需要 STUN/TURN 或中继服务器，本机无法验证
- 请求统计的完整网关（当前只有转发 + 计时 + 计数）

详见 [ROADMAP.md](ROADMAP.md)。

---

## [0.5.0-m5] — 2026-10-05

**里程碑 3 / 4 / 5 已达成**，并补上需求书里此前只写了一半的两块：
**设备文件管理**与**操作审计查询**。版本号从 `0.2.0-m2` 跳到 `0.5.0-m5`。

### 新增 —— 里程碑 3：模型库 + 模型下发

**平台**
- `POST /models` 上传：multipart **流式落盘 + 流式 sha256**，大文件不进内存；
  `name + version` 唯一
- `GET /models` / `GET /models/{id}` / `DELETE /models/{id}`（删除同时清掉文件与附属物）
- `GET /models/{id}/download` —— Agent 下载口，支持 **HTTP `Range` 断点续传** + `ETag`
- `PATCH /models/{id}` —— 模型**登记信息**（框架 / 量化 / 目标 SoC / QNN 版本 /
  后端 / 依赖 / 运行参数 / 自检命令与超时）。**增量更新**：清空某项即改回「未登记」
- `POST /models/{id}/compat` —— 下发前**兼容性预检**，逐维度返回
  `pass / fail / unknown`。核心纪律：**拿不到 ≠ 不兼容**，
  没登记元数据一律判 `unknown` 而不是 `fail`
- 附属物（**Deployment Package**）：`POST /models/{id}/manifest` 上传配置文件 /
  tokenizer 等旁路文件，`DELETE /models/{id}/manifest/{name}` 删除
- 部署任务：`POST /deployments`（多模型 × 多节点）、`GET /deployments`、
  `GET /deployments/{id}`、`GET /deployments/queue/stats`
- `POST /deployments/precheck` —— 建任务**之前**先告诉你哪些节点会被卡住
- `POST /deployments/{id}/retry` —— 只重投失败节点，成功节点不受影响
- `POST /deployments/{id}/cancel` —— 需求 §6 的 `cancelled` 态。
  已结束的任务再取消返回 **409**，不假装成功
- 逐节点 **9 态**：`pending / uploading / downloading / verifying / installing /
  testing / ready / failed / cancelled`；
  任务聚合状态由 `models/enums.py:aggregate_deploy_status()` **一处**决定
- `server/app/services/deploy_queue.py` —— 下发并发治理（默认并发 2）

**Agent**
- 收到 `deploy_model` → HTTP 下载到 `.part` → **`Range` 续传** → 流式 sha256 比对
  → 不符则 `failed`（`checksum_mismatch`）并可重试
- **每 1s 上报一次真实进度**（已写字节数 / 总大小，不是估算值）
- 附属物落位到设备模型目录
- **Self Test**：按模型的 `self_test_command` 跑一次，
  **不通过就不许判 `ready`**（这是需求 §6 明确要求的 Testing 环节）

### 新增 —— 里程碑 4：服务管理 + API 暴露

**Agent（`ServiceSupervisor`）**
- `subprocess` 进程组托管（Termux 免 root 唯一可行方案，**不假设 systemd**）
- 状态机 **7 态**：`stopped / starting / running / restarting / stopping /
  crashed / error`（比原计划的 3 态多出 4 态，因为「停止中」和「崩溃」必须能区分）
- 启停**幂等**：已是 `stopped` 再停直接 no-op
- 停止序列 `terminate → 等 5s → kill`，不留僵尸
- 崩溃看门狗（`auto_restart`）+ 日志滚动落盘

**平台**
- `GET /services` / `POST /services` / `GET /services/{id}` / `DELETE /services/{id}`
- `POST /services/{id}/{action}` —— `start` / `stop` / `restart`
- 资源准入：`mem_required_mb` + `mutex_group`。不满足时 **409**，
  且 `detail` 写的是人话（「可用内存 300MB < 需求 800MB」），前端原样展示
- `POST /services/{id}/check` —— 主动探活 `health_url`
- `GET /services/{id}/logs` —— 拉取子进程 stdout
- 删除运行中的服务返回 **409**：直接删记录会在设备上留下平台看不见的孤儿进程
- 最简网关：转发 `node.ip:port` + 记录耗时（last / avg）+ 请求数与失败数，
  `GET /gateway/{service_id}/stats` 查询

### 新增 —— 里程碑 5：稳定性收尾

- **指令幂等**：每条指令带 `cmd_id`，Agent 侧 LRU 去重，重复下发不产生副作用
- **服务自恢复**（需求 §7）：
  - `auto_start` —— Agent / 设备重启后，自动把标记了自启的服务拉起来
  - `auto_restart` —— 进程自己死了，看门狗把它拉回来
  - 两者**默认关闭**：重启后要不要自己起来，应由人明确决定
- 平台重启时把所有 `online` 强制降级为 `offline`
  （重启后没有任何连接，留着 `online` 就是假在线）
- 三层日志（平台 / Agent / 服务），支持按行数拉取

### 新增 —— 设备文件管理

需求书里「文件管理」只写了一句，这次做完整了：

- `GET /nodes/{id}/files/roots` —— 可访问的根目录（含剩余空间）
- `GET /nodes/{id}/files` —— 浏览目录
- `GET /nodes/{id}/files/stat` / `GET /nodes/{id}/files/du` —— 属性 / 占用统计
- `POST .../files/mkdir|delete|copy|move` —— 增删改（删目录需**显式递归**）
- `POST .../files/pull` —— 从设备取回文件到平台
- `POST .../files/push` —— 从平台推文件到设备
- `GET /files/transfers` —— 传输记录；`DELETE /files/transfers/{id}` 取消并清理

三条硬约束：

1. **沙箱根目录白名单** —— 路径穿越 / 越界访问一律拒绝（验收脚本专门测了逃逸）
2. **一次性传输令牌** —— 每次传输单独签发，绑定 `transfer_id` + TTL，用后即废
3. **半截文件不给下载** —— 只有 `status == done` 的传输才提供下载入口。
   半个模型文件比没有文件更危险

### 新增 —— 操作审计查询（需求 §8）

- `GET /audit` —— 按动作前缀 / 节点 / 操作者 / 成功失败过滤，倒序分页
- `GET /audit/actions` —— 出现过的动作名（供前端做下拉）
- **刻意不提供任何写接口**：谁都能塞一条的记录，事后就不能当依据。
  失败的写操作同样留痕 —— 「谁试过但没成功」也是审计信息，甚至更重要

### 新增 —— 指标分层保留（原 A 类缺口 A5，本版本补齐）

此前只有一条线：采样 7 天后**直接删除**，历史无法回填。现在拆成三层。

**存储**（`metric_rollups` 表，用 `kind` 区分粒度；列名与原始采样表保持一致）

| 层 | 粒度 | 默认保留 | 每节点量级 |
|---|---|---|---|
| 原始 `raw` | 5 秒 | 2 天 | 17,280 行/天 |
| 分钟级 `minute` | 60 秒 | 30 天 | 43,200 行/月 |
| 小时级 `hour` | 3600 秒 | 365 天 | 8,760 行/年 |

**服务** `services/metric_rollup.py`

- 一轮 = **先聚合 → 后清理**，顺序写死在同一个函数里，且**聚合抛异常则整轮跳过、一条不删**。
  宁可磁盘多占一点，不可历史断档。
- **只对真正产出新值的桶**做 `INSERT ... ON CONFLICT DO UPDATE`。这一点是刻意的：
  原始数据被剪掉之后重跑聚合，如果写法是「先删整段再插」，删完就什么都插不回来了。
  验收脚本 E 段专门钉死这条。
- 递增策略：进程内**首轮全量回溯**（让从旧版本升级上来的库一次性压完），
  之后按 `metric_rollup_interval_minutes`（默认 60）走增量（回溯 3 倍周期，盖住一次执行失败）。
- 字段语义分明：数值型取**算术平均**；`net_rx_bytes` / `net_tx_bytes` 是**累计计数器**，
  取桶内**最后一个值**（求平均会被误读成速率）；`battery_status` 取最后一个非空值；
  CPU / 内存 / 存储等峰值字段额外记 `min` / `max`。
- **不造假**：整桶没采到的字段写 `NULL`，不是 `0`；`samples` 与 `field_samples`
  如实记录参与平均的条数，前端据此判断这个点可不可信。

**查询**（`services/metrics_query.py`）

- `tier_plan()` 按窗口自动选层，保证各段**首尾相接、不重叠、不留缝、不重复计数**。
  内容包括「原始层不清理时聚合层必须为空」这类只会在肉眼看图时才发现的错误。
- 大屏回看上限由 7 天提到 **365 天**（由小时级保留期决定，不再是原始采样）。
- 顺带修掉一个**既有的集群内存聚合 bug**：原先按桶内所有采样求和，
  内存总量被采样条数放大（百分比恰好正确，所以一直没被发现）。改为「先按节点在时间上平均，
  再跨节点求和」。

**配置**（新键，旧键仍兼容）

```toml
metric_raw_retention_days = 2
metric_minute_retention_days = 30
metric_hour_retention_days = 365
metric_rollup_interval_minutes = 60
```

- 保留期设 **0 = 该层不清理**（而不是「立刻删光」）。
- 已废弃的 `metric_retention_days` 仍生效，等价于原始层保留天数。
  优先级 `环境变量新键 > 环境变量旧键 > toml 新键 > toml 旧键 > 内置默认值` ——
  **先比来源再比新旧**；写成「新键只要出现过就一律赢」会让环境变量输给配置文件。

**接口**

- `GET /api/v1/system/metric-tiers` —— 三层各自的保留期、行数、时间跨度
- `POST /api/v1/system/metric-tiers/run?full=` —— 手动跑一轮（幂等）

### 新增 —— 备份与恢复（原 A 类缺口 A4，本版本补齐）

换机器时，代码可以直接拷走，但节点记录、模型登记、部署历史和审计是数据 —— 此前只能手工复制
`server/data/`，而且**复制出来的东西很可能是坏的**（见下）。

**服务** `services/backup.py`

- **快照走 SQLite 的一致性快照（`VACUUM INTO`），不是复制文件。** 数据库开着 WAL，
  最近的提交还在 `edge.db-wal` 里没合并进主文件 —— 直接 `copy edge.db` 会得到一个
  「少了最后一截、还不报错」的库。这一点在模块开头和代码注释里都写死了。
- **恢复前自动另存当前数据，而且那份安全备份也走同样的快照方式。**
  开发时这里先写成了 `shutil.copy`，验收脚本立刻抓到：复制出来的 `.bak` 连 `nodes` 表
  都还在 WAL 里没落地，探针一句 `no such table: nodes` 把整个恢复流程挡死了。
  **救命绳本身必须以同样的方式生成** —— 这个 bug 被 42 条断言里的 F 段永久钉住。
- **恢复时先删 `-wal` / `-shm`**。不删的话 SQLite 会拿旧 WAL 往新库上重放，
  刚恢复的数据被悄悄改回去。验收脚本 G 段是**真的换一套数据再恢复**，
  用结果本身证明旧 WAL 没有回流。
- 恢复是破坏性操作，四道闸：① 必须显式 `confirm=true`；② 先留可读的安全备份；
  ③ 上传文件必须是合法 zip + 含 `edge.db` + SQLite 魔数正确 + sha256 对得上；
  ④ 只有走到最后一步才 `os.replace`，中途任何失败都不动现有数据。
- 默认**不打包模型权重**（动辄几 GB），manifest 里逐条记了 sha256 与
  `file_present`，需要连权重一起搬的人照着清单核对即可；要一起搬就打开 `include_models`。

**接口**

- `GET /api/v1/system/backup?include_models=` —— 生成并下载备份 zip
- `POST /api/v1/system/backup/inspect` —— 只校验，**不改动任何数据**
- `POST /api/v1/system/backup/restore?confirm=` —— 恢复（不带 confirm 只返回"将要发生什么"）
- `GET /api/v1/system/backup/status` —— 库大小、WAL 是否未合并、已有备份清单

**前端**

- 新增「系统」页：备份现状、一键备份下载、两步式恢复（**先校验，通过才允许恢复**）、
  数据规模、历史备份列表。

### 新增 —— 部署失败回滚（原 A 类缺口 A1，本版本补齐）

**问题**：落盘纪律本来没问题（`.part` + 流式 sha256 + 原子改名），所以「下载失败」从不会污染设备。
漏的是落位**之后**：`下载 → 校验 → 落位 → 拉附属物 → Self Test`，Self Test 排在最后 ——
它一挂，部署被判 failed，可新模型**已经顶替了旧模型**、附属物也已经覆盖过。设备上留下的是
「一份连自检都过不了的新版本」，原来那份能跑的旧版本**已经没有了**。界面上写「部署失败」，
设备实际是「半新半旧且不可用」。

**新模块** `node-agent/agent_rollback.py`

- **落位之前先把旧文件挪走**。同一文件系统内 `os.replace` 只是改个名：常数时间、不占额外空间，
  而模型动辄几个 GB，复制一份等于把手机存储占用翻倍。
- **拿不到后悔药就不动手**。建不了回滚点一律中止部署 —— 覆盖一个救不回来的文件，
  比这次部署失败严重得多。
- **回滚之后必须验证**。而且验证不能只看「原位的文件在不在」：这次部署刚写过那个位置，
  它**必然**有个文件（就是那个新的），存在性检查会被它骗过去，把「根本没还原」报成「回滚成功」。
  所以真正的判据是**进 `_apply` 时备份副本还在不在**。这条是验收脚本 F 段钉住的 ——
  写完第一版时只查存在性，F 段当场判失败。
- **回滚失败不删日志目录**。里面还有救得回来的文件，且那是唯一的证据；同时日志打 ERROR。
- **崩溃可恢复**。Agent 会被 Android 省电策略杀掉，如果正好死在「旧模型已挪走、新的还没落位」，
  只存在内存里的记录会随进程消失，那份旧模型就永久丢了。所以清单每步落盘成 `journal.json`，
  下一轮 `prepare()` **先把上一轮遗留的还原掉再开工**。
- 回滚失败过一次就记住结论，重复调用返回同一份，不再重试 —— 重试会把「已经还原成功」的
  误判成「备份丢了」，报出假的故障。

**接入** `agent.py`

- 模型本体与**附属物**都记入回滚日志（只保护本体，回滚后附属物还是新的，等于没回滚）。
- 取消也回滚：用户要的是「当没发生过」，不是「装了一半、任务显示已取消」。

**平台侧**

- `deploy_task_nodes` 新增 `rollback_note` 列（老库自动补列），与 `error` **分开存**：
  错误原因是「为什么失败」，回滚结果是「设备现在什么样」，用户要的常常是后者。
- `ws.py` 回执处理里**无条件**写入回滚结论 —— 被取消的节点同样可能已经动过设备文件。
  回滚未完整完成时额外打一条 ERROR 日志。
- 重投（retry）时清空上一轮的 `rollback_note`，免得让人以为这一轮也回滚了。
- `Deployments.tsx` 在失败节点旁显示回滚结论标签。

**验收** `docs/acceptance/rollback_check.py`（**42 条断言**）

- 前半段测日志模块本身；后半段**真跑一遍 `_deploy_model`** ——
  起一个本机 HTTP 服务当「平台的文件服务」，让 Self Test 失败，然后逐字节比对设备上的文件
  是否回到了旧版本。只测模块不测接线，等于赌它被人调用了。
- 专门钉住两个「最坏的失败」：回滚没生效却报成功（F 段）、
  以及「不该动的东西动了」（G 段：落位前就失败的部署，旧文件一个字节都没被动过）。

### 新增 —— 交付包隐私自检（`tools/pack_portable.py`）

打包给别人的包里**不该出现打包这台机器的身份信息**。这类泄露看目录清单发现不了
（用户名藏在文档正文里），只能读内容才查得出，所以做成打包时的一道自检：

- 动态取本机用户名 / 主机名 / 出口 IP（**不写死关键词** —— 换台机器打包同样拦得住），
  逐文件扫描包内文本（含前端 JS bundle），命中即 **FAIL 并返回非零退出码**；
- 独立复核确认：包里用户名 / 主机名 / 内网 IP / WPS 账号 ID **均为 0 处**。

顺带清掉仓库里四处硬编码的本机痕迹：`C盘瘦身与迁移到E盘指南.md` 的用户名与
WPS 账号 ID（改为 `<用户名>` / `<账号ID>` 占位符）、`真机测试检查单.md` 里指向
旧副本的绝对路径、`docs/acceptance/m5_selfheal_check.py` 写死的解释器路径，
以及 `项目详细报告.html`（不在包内，一并清掉）。

最后一条不只是隐私问题：那个写死的 venv 解释器路径换台机器就不存在了，
脚本会直接跑不起来。现在改成 `os.environ.get("EDGE_PY") or sys.executable`，
与其它验收脚本一致。

### 新增 —— 前端

- 新增页面：**文件管理**（`Files.tsx`）、**操作审计**（`Audit.tsx`）、**系统**（`System.tsx`，备份与恢复）
- 菜单补上「接口文档」入口
- `Models.tsx` 重写：登记信息编辑、兼容性预检（多选节点逐维度展示）、
  Deployment Package 管理
- `Deployments.tsx` 重写：建任务时实时预检并警告被阻塞的节点、
  取消入口（只在可取消时出现）、9 态展示、`notified` 未送达如实提示
- `Services.tsx`：新增「自恢复」列（`auto_start` / `auto_restart` / `restart_count`）、
  状态变更时间、`last_exit_code`
- `NodeDetail.tsx`：设备能力三态标签（「未知」**不等于**「不支持」）、
  分项温度与频率曲线、电池信息、「管理文件」入口
- `Dashboard.tsx` / `Nodes.tsx`：补 NPU、电量、QNN 版本、Root 三态
- `types.ts` / `api.ts` 全面重写，与后端字段一一对齐；
  `errText()` 统一把后端 `detail` 变成人话，而不是前端编一句「操作失败」

### 变更

- 版本号 `0.2.0-m2` → `0.5.0-m5`
- 指标保留配置由单键 `metric_retention_days` 改为三层四键（见上）；旧键仍兼容
- 大屏趋势图与节点详情曲线的回看上限：7 天 → **365 天**

### 安全（需求 §8）

- **四类令牌**，用途隔离、互不通用：
  1. 共享 token —— 控制台 REST 调用
  2. 设备 token —— 每台设备独立，WS 握手时校验
  3. 传输令牌 —— 一次性，绑定单次文件传输
  4. 模型下载令牌 —— 一次性，绑定 `model_id + node_id`，内存态 + TTL
- **远程命令默认关闭**：默认配置下平台不能对设备执行任意命令；
  开启也需要白名单 + 全程审计
- **不做 Root Shell 暴露**：没有任何接口能把任意 shell 直接透到公网

### 验收结果（实测）

| 脚本 | 结果 |
|---|---|
| `config_check.py` | **13/13** |
| `m1_check.py --phase online` | **11/11** |
| `m1_check.py --phase offline` | **8/8** |
| `m2_check.py --phase online --nodes 1` | **17/17** |
| `m2_check.py --phase offline` | **16/16** |
| `m3_check.py`（隔离实例） | **22/22** |
| `m3_deploy_check.py`（隔离实例） | **11/11** |
| `m2_ip_check.py`（隔离实例） | **3/3** |
| `agent_metrics_check.py` | **41/41** |
| `files_check.py` | **失败 0 项** |
| `m5_selfheal_check.py` | **失败 0 项** |
| `m6_model_deploy_check.py` | **全部通过** |
| `metric_tiers_check.py`（自建临时库） | **37/37** |
| `backup_check.py`（自建临时库） | **42/42** |
| `rollback_check.py`（自建临时库 + 本机 HTTP） | **42/42** |
| `npx tsc --noEmit` | 无错误 |
| `npm run build` | 成功（`web/dist` 已重建） |

### 已知取舍

1. **离线判定约 20~25s**（15s 阈值 + 2 次 5s 心跳确认）。文档里写「15s 内」是不准确的，
   实际最坏约 25s。理由见 [ROADMAP](ROADMAP.md) 与 O9。
2. **不做 P2P / Relay**：连接层抽象（`core/transport.py`）已就位，
   但真实公网打洞需要 STUN/TURN 或中继服务器，本机无法验证，**没有实现**。
3. **不做 HTTPS**：面向局域网部署，且 Agent 侧已支持 `https://` → `wss://`，
   需要 TLS 时在前面挂一层反向代理即可。
4. **不做设备重启 / 关机**：Termux 免 root 下没有可用的关机接口，
   强行「实现」只会是假的。

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

[Unreleased]: ../../compare/v0.5.0-m5...HEAD
[0.5.0-m5]: ../../releases/tag/v0.5.0-m5
[0.2.0-m2]: ../../releases/tag/v0.2.0-m2
[0.1.0-m1]: ../../releases/tag/v0.1.0-m1
