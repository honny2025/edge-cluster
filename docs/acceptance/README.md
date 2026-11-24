# 验收脚本统一入口

这一目录放的是「跑一遍就知道对不对」的脚本。每个都是**独立可执行**的，
不依赖 pytest，用项目自带的 venv 解释器就能跑。

> 文档里每一项通过数都标了 **[实测]** 或 **[未实测]**。
> **[实测]** 是在本机真跑出来的数字；**[未实测]** 表示这次没跑，请以脚本实际输出为准 ——
> 不把没验证过的数字写进来充数。

---

## 一、先看清解释器路径

**不要用系统 `python`**，要用项目自己的 venv（依赖都在里面）：

```bash
# Windows / Git Bash（venv 在仓库根目录）
.venv/Scripts/python.exe

# Linux / macOS
.venv/bin/python
```

下面所有命令都用 `<PY>` 代表它，**并且默认你在仓库根目录执行**。

如果 venv 还不存在：

```bash
cd server
<PY> -m venv ../.venv        # 或 python -m venv ../.venv
../.venv/bin/pip install -r requirements.txt   # Windows: ..\.venv\Scripts\pip
```

---

## 二、脚本清单

跑任何脚本之前，**先确保平台在跑、且至少有 1 个 Agent 在线**（`--phase offline` 除外）。

| 脚本 | 验什么 | 跑法 | 引用值 | 状态 |
|---|---|---|---|---|
| `config_check.py` | 配置层：env 覆盖、类型转换、默认值、旧键兼容优先级 | `<PY> docs/acceptance/config_check.py` | **13/13** | [实测] |
| `m1_check.py` | M1：节点发现、注册、心跳、状态机 | `<PY> docs/acceptance/m1_check.py --phase online` | **11/11** | [实测] |
| `m1_check.py --phase offline` | M1：离线判定、离线不清零 | 先停掉 ≥1 个 Agent | **8/8** | [实测] |
| `m2_check.py` | M2：大屏聚合、指标、告警 | `<PY> docs/acceptance/m2_check.py --phase online --nodes 1` | **17/17** | [实测] |
| `m2_check.py --phase offline` | M2：离线不残留旧值、告警一致性 | 先停掉 ≥1 个 Agent | **16/16** | [实测] |
| `agent_metrics_check.py` | Agent 采集降级链（Android 封锁下的行为） | `<PY> docs/acceptance/agent_metrics_check.py` | **41/41** | [实测] |
| `files_check.py` | 文件管理：沙箱逃逸、令牌边界、传输状态 | `<PY> docs/acceptance/files_check.py` | **失败 0 项** | [实测] |
| `m5_selfheal_check.py` | 服务自恢复：Agent 被杀 → 重启 → 服务回来 | 见脚本头部两步用法 | **失败 0 项** | [实测] |
| `m6_model_deploy_check.py` | 模型登记 / 兼容预检 / 部署 / Self Test / 取消 | `<PY> docs/acceptance/m6_model_deploy_check.py` | **全部通过** | [实测] |
| `metric_tiers_check.py` | 指标分层：桶对齐 / 聚合正确性 / 顺序安全 / 查询分层不重叠 | `<PY> docs/acceptance/metric_tiers_check.py` | **37/37** | [实测] |
| `backup_check.py` | 备份与恢复：快照自包含 / 坏包识别 / dry-run 不动数据 / WAL 不回流 | `<PY> docs/acceptance/backup_check.py` | **42/42** | [实测] |
| `rollback_check.py` | 部署失败回滚：真跑 `_deploy_model`，让 Self Test 失败后逐字节比对设备文件；另含「回滚失败必须如实报」与「落位前失败则一个字节都不动」 | `<PY> docs/acceptance/rollback_check.py` | **42/42** | [实测] |
| `m3_check.py` | M3 模型库：上传/列表/详情/删除/下载（`Range` 续传、`ETag`、sha256） | 见下方「隔离实例」 | **22/22** | [实测] |
| `m3_deploy_check.py` | M3 下发全流程 | 见下方「隔离实例」 | **11/11** | [实测] |
| `m2_ip_check.py` | 节点 IP 是否取平台观测值（而非 Agent 自报） | 见下方「隔离实例」 | **3/3** | [实测] |

### `--phase` 怎么选

`m1_check.py` / `m2_check.py` 支持 `--phase online|offline|all`（默认 `all`）：

- `online` —— 只跑在线场景，**任何时候都能跑**
- `offline` —— 需要**先停掉至少一个 Agent**，否则会在等待离线时超时失败
- `all` —— 两个都跑，等于自动要求你先停 Agent

**推荐做法**：平时跑 `--phase online`；要验证离线行为时，
专门停掉一个 Agent 跑 `--phase offline`，跑完再把它起回来。

### 跑完记得清残留

有三支脚本会**在仓库里留下文件**（已在 `.gitignore` 里，不会进 git，但会进交付包）：

| 脚本 | 留下什么 | 清法 |
|---|---|---|
| `m6_model_deploy_check.py` | `node-agent/models/m6-config-<pid>.json` | `rm -f node-agent/models/m6-config-*.json` |
| 同上（上传的测试模型） | `node-agent/models/probe.bin`（8 MB） | `rm -f node-agent/models/probe.bin` |
| `m5_selfheal_check.py` | `docs/acceptance/_m5_state.json` | `rm -f docs/acceptance/_m5_state.json` |

一条命令全清：

```bash
rm -f node-agent/models/m6-config-*.json node-agent/models/probe.bin docs/acceptance/_m5_state.json
```

> `tools/pack_portable.py` 只按**后缀**跳过文件（`.bin` 会被跳过），
> 认不出 `.json` —— 所以打交付包之前务必先清一遍。

### `m5_selfheal_check.py` 的两步用法

这个脚本要杀 Agent，所以拆成了两步：

```bash
<PY> docs/acceptance/m5_selfheal_check.py prepare    # 建服务并置 auto_start=True
# —— 此时手工 kill 掉那个 Agent，再重新启动它 ——
<PY> docs/acceptance/m5_selfheal_check.py verify     # 验证服务自己回来了
```

---

## 三、为什么要起「隔离实例」

`m3_check.py`、`m3_deploy_check.py`、`m2_ip_check.py` 会**真的往数据库里写东西**
（建模型记录、注册假节点）。直接打你正在用的平台有两个问题：

1. **污染数据** —— 你自己的节点列表和模型库里会多出验收残留
2. **看到假失败** —— 新代码要重启进程才生效，打运行中的老实例会得到误导性的失败

起一个临时的（**从 `server/` 目录执行**）：

```bash
cd server

EDGE_SERVER__PORT=8899 \
EDGE_SERVER__DISCOVERY_PORT=37998 \
EDGE_WEB__SERVE_STATIC=false \
EDGE_DATA__DB_PATH="$TMPDIR/iso/edge.db" \
EDGE_DATA__MODELS_DIR="$TMPDIR/iso/models" \
EDGE_DATA__LOGS_DIR="$TMPDIR/iso/logs" \
  <PY> -m uvicorn app.main:app --host 127.0.0.1 --port 8899
```

> 如果目标目录不存在，平台会在启动时报错。先 `mkdir -p` 一下即可。

然后开另一个终端：

```bash
<PY> docs/acceptance/m3_check.py         --base http://127.0.0.1:8899
<PY> docs/acceptance/m3_deploy_check.py  --base http://127.0.0.1:8899
<PY> docs/acceptance/m2_ip_check.py      --base http://127.0.0.1:8899
```

跑完 `Ctrl-C` 停掉临时实例即可。数据全在临时目录里，不影响正式库。

---

## 四、踩过的坑（真实发生过）

### 1. 本机代理导致连 `127.0.0.1` 报 502

系统设有 `HTTP_PROXY` 时，`127.0.0.1` 会被走代理，报 **502**，看起来像平台挂了。
实际是代理的问题。`m3_check.py` / `m2_ip_check.py` 内部已处理
（显式构造 `ProxyHandler({})`）—— **你自己写验收脚本时要留意**，否则会被坑很久。

### 2. WS 路径不带 `/api/v1` 前缀

真实路径是 **`/ws/agent`** 和 **`/ws/dashboard`**。`main.py` 里 `ws.router` 是单独 include 的；
按 REST 习惯去拼 `/api/v1/ws` 会拿到 **403**（Starlette 在 `accept()` 之前 `close()` 就返回 403），
很容易误判成 token 错误。**先确认路径，再看鉴权。**

### 3. REST 鉴权头是 `X-Auth-Token`，不是 `X-Token`

写错就是 401。也支持 query 参数 `?token=xxx`。

### 4. 用任务管理器「结束进程」杀不掉 Agent

图形界面的「结束任务」可能只杀掉外层包装 shell，**Python 子进程仍存活并继续发心跳**，
于是节点永远不离线。用 PowerShell 精确核对：

```powershell
Get-CimInstance Win32_Process -Filter "name='python.exe'" |
  Where-Object { $_.CommandLine -like '*agent.py*' } |
  Select-Object ProcessId, CommandLine
```

然后用 `taskkill /PID <pid> /F` **按 PID 精确杀**。
不要用通配符批量杀 —— 很容易把平台自己也带走。

---

## 五、退出码

全部脚本语义一致：全通过 → `0`；有失败 → `1`。可以直接串到 CI 里：

```bash
<PY> docs/acceptance/m3_check.py --base http://127.0.0.1:8899 && echo "M3 OK"
```
