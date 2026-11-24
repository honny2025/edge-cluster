# Codex 手机端（Termux）接入指南

> **这是给 AI 代理（Codex / 其他 coding agent）看的自主执行指南。**
> 你（Codex）会被放进某部 Android 手机的 **Termux** shell 里，任务是把这部手机的
> `node-agent` 接入局域网内的边缘计算平台，让它在平台控制台显示为「在线节点」。
>
> 这份文档是自包含的——你不需要看任何聊天记录。平台侧（服务器 + 网页控制台）已由人在电脑上
> 跑好，你只负责**手机这一侧**。

---

## 0. 你的目标（一句话）

让 `python3 agent.py` 在 Termux 里常驻运行，并通过 WebSocket 向平台注册成功。

完成后，人在电脑浏览器打开 `http://<平台IP>:8000` → 节点页 → 能看到这部手机、状态在线、
型号 / SoC / Android 版本有值。

> 这是**第一阶段**（节点上线）。第二阶段——部署模型 / 跑推理服务——还需要装推理运行时
> llama.cpp（`llama-cli` / `llama-server`），见第 4 节。

---

## 1. 前置条件（需要「人」先做完，你才能开始）

下面这几步 Codex **做不了**（需要碰手机 GUI / 从电脑传文件），所以开始前先确认人都弄好了：

| # | 前置项 | 谁做 | 完成后你应该能看到 |
|---|---|---|---|
| 1 | 手机装好 **Termux**（F-Droid 版，非 Google Play 版） | 人 | Termux 能打开 |
| 2 | Termux 内执行过 `termux-setup-storage`，弹窗点了「允许」 | 人 | `ls ~/storage/downloads` 有内容 |
| 3 | **node-agent 代码已经在手机上**（见下方 3 种获取方式之一） | 人 / 你 | `find` 能找到 `agent.py` |
| 4 | 人告诉你三件事：**平台地址 / 共享 token / 这部手机的节点名** | 人 | 你手里拿到这 3 个值 |

> 如果第 3 步代码不在手机上、且人也没法立刻传，你**先停下来**问人「请把 node-agent 目录弄到手机上」，
> 不要自己瞎编路径或伪造文件。

### 获取 node-agent 代码的 3 种方式（按优先级）

1. **人从电脑传**（最稳）：把电脑上 `edge-cluster-latest/node-agent/` 整个目录，通过数据线 / QQ / 微信
   发到手机 `Download` 任意子目录。你后面用 `find` 自动定位。
2. **你直接 git clone**（若最新代码已推到 GitHub）：
   ```bash
   pkg install -y git >/dev/null 2>&1
   git clone --depth 1 https://github.com/honny2025/edge-cluster ~/edge-cluster-src
   # 之后 --src 指向 ~/edge-cluster-src/node-agent
   ```
3. **下载发布包**（若平台提供了 zip 直链 `<ZIP_URL>`）：
   ```bash
   curl -L <ZIP_URL> -o /sdcard/Download/edge-cluster.zip
   cd /sdcard/Download && unzip edge-cluster.zip
   ```

---

## 2. 环境假设（你现在的位置）

- 你在 Android + Termux 的 bash 里，`$HOME` 是 `/data/data/com.termux/files/home`。
- 手机和电脑在**同一个局域网**（电脑开热点、或同一 Wi-Fi）。
- 你有网络访问（能 `curl` 到平台地址）。
- Termux 里可能还没装 Python——脚本会自动装，你不用手动。

---

## 3. 执行流程（逐步做，每步把真实输出贴出来）

### Step 0 — 确认平台可达（拿不到就停）

```bash
curl -s --max-time 5 http://<平台IP>:8000/api/v1/system/health
```

- 期望看到包含 `"status":"ok"` 的 JSON。
- **如果超时 / 连接拒绝 / 不是 ok**：停下，报告「平台不可达」，让人确认平台已经启动、IP 没变、端口 8000 放行。
  **不要**继续往下走。

### Step 1 — 定位 node-agent 代码

```bash
find /sdcard/Download /sdcard "$HOME" -maxdepth 5 -name agent.py 2>/dev/null | head -n 3
```

- 有输出 → 记下它的目录（比如 `/sdcard/Download/node-agent`）。
- 没输出 → 回到第 1 节用 git clone 或让人传；仍不行就停下报告。

### Step 2 — 跑一键接入脚本

```bash
# 若 agent.py 在 ~/node-agent 已就位，直接：
cd ~ && bash phone_setup.sh --platform http://<平台IP>:8000 --name <节点名>

# 若代码在别的目录，用 --src 指定：
cd ~ && bash phone_setup.sh --src /sdcard/Download/node-agent --platform http://<平台IP>:8000 --name <节点名>
```

参数说明：
- `--platform`：平台地址，如 `http://192.168.x.x:8000`。不填则走 UDP 广播自动发现（需同网段且无客户端隔离，校园网常失败，**建议人给明确 IP**）。
- `--name`：**只能含字母/数字/下划线/连字符/点**，每部手机必须不同（如 `honor-x70`、`redmi-k60`）。
- `--token`：默认 `edge-cluster-dev-token`，与平台 `server/config.toml` 一致即可，一般不填。
- `--no-kill`：不杀已运行的 agent（调试用）。

脚本会自动完成：找 agent.py → 拷进 `~/node-agent` → 装 python 与依赖(websockets/httpx) → 写 `config.toml` → 后台启动 → 验证。
全程无交互，首次装依赖可能慢（30 秒~3 分钟）。**把脚本的完整 stdout 原样贴给人。**

### Step 3 — 验证注册成功

```bash
pgrep -f agent.py                              # 期望：输出一个 PID
tail -n 20 ~/node-agent/logs/<节点名>.log      # 期望：出现「平台确认注册成功」
cat ~/node-agent/config.toml | grep -E '^(name|platform_url|token)'
```

- 日志里出现「平台确认注册成功」= 注册 OK。
- 同时让人在电脑控制台确认节点页能看到 `<节点名>` 且在线。

### Step 4 — 申请唤醒锁（防息屏挂起）

```bash
command -v termux-wake-lock >/dev/null 2>&1 && termux-wake-lock || echo "termux-wake-lock 不可用（需 pkg install termux-api + 装 Termux:API 应用），不影响运行"
```

### Step 5 — 保活（必须「人」手动做，脚本做不了）

这一步不听话，手机息屏几分钟后 Agent 就会被系统杀掉。请转告人去设置：

| 位置 | 动作 |
|---|---|
| 设置 → 应用 → Termux → 省电策略 | 无限制 / 允许后台活动 |
| 设置 → 应用 → Termux | 允许自启动 |
| 最近任务列表 → Termux | 下拉加锁 |
| Termux 内 | `termux-wake-lock`（装了 Termux:API 才有效） |

荣耀 / MagicOS 额外：设置 → 应用 → 应用启动管理 → Termux → 关掉「自动管理」，手动全开三个开关。

---

## 4. 安装推理运行时（llama.cpp）—— 部署模型 / 跑服务才需要

> 上面 Step 0~5 只做到「节点在线」。但平台真正要干的事，是**把 GGUF 模型下发到这部手机、
> 拉起推理、开放端口给浏览器调用**。这件事靠 llama.cpp 提供的两个程序，平台**不负责安装**，
> 必须手机自己装好：
>
> - `llama-cli` —— 模型**自检**默认用它：下发完成后跑一次 `-p hi -n 4`，验证模型能加载。
> - `llama-server` —— 模型**服务**用它：常驻 HTTP 服务、开放端口，浏览器从这访问。
>
> 不装的话：节点能在线，但人一部署模型，自检会报「Self Test 未通过」，服务也起不来。

### Step A — 确认是否已装

```bash
command -v llama-cli llama-server 2>/dev/null || echo "未安装"
```

- 两个都有路径输出 → 已装好，直接跳到 Step D。
- 只有 `main` / `server`（llama.cpp 旧版命名）→ 直接看 Step C 的「命名兼容」。

### Step B — 安装

先试官方仓库（最快）：

```bash
pkg update -y && pkg install -y llama-cpp
command -v llama-cli llama-server 2>/dev/null || echo "仓库里没有/没装上，走源码编译"
```

若还没装上，源码编译（一定可行）：

```bash
pkg install -y git cmake ninja clang make
git clone --depth 1 https://github.com/ggml-org/llama.cpp.git ~/llama.cpp
cd ~/llama.cpp
cmake -B build -G Ninja -DGGML_NATIVE=OFF
cmake --build build -j4
ls build/bin/   # 看产物名：llama-cli/llama-server 或 main/server
```

> 手机上编译可能要几分钟到几十分钟。若某个 cmake 选项报错，以仓库 README 当前构建说明为准，别死磕。

### Step C — 放进 PATH + 旧版命名兼容

```bash
mkdir -p ~/bin
for b in llama-cli llama-server main server; do
  [ -f "$HOME/llama.cpp/build/bin/$b" ] && cp "$HOME/llama.cpp/build/bin/$b" "$HOME/bin/" && echo "copied $b"
done
echo 'export PATH="$HOME/bin:$PATH"' >> ~/.bashrc
export PATH="$HOME/bin:$PATH"
```

**命名兼容（重要）**：平台侧默认命令用**新名字** `llama-cli`/`llama-server`；llama.cpp 老版本产物叫
`main`/`server`。若手机只有老名字，二选一：

1. 建软链（推荐，平台默认命令不用改）：
   ```bash
   command -v llama-cli >/dev/null 2>&1 || ln -sf "$(command -v main)" ~/bin/llama-cli
   command -v llama-server >/dev/null 2>&1 || ln -sf "$(command -v server)" ~/bin/llama-server
   export PATH="$HOME/bin:$PATH"
   ```
2. 不改手机，改**平台侧**：把模型「Self Test 命令」和服务「启动命令」里的
   `llama-cli`/`llama-server` 换成 `main`/`server`（由人在电脑控制台上改）。

### Step D — 验证并报告

```bash
command -v llama-cli llama-server
llama-cli --version 2>&1 | head -1
echo "$PATH"
```

期望：前两条各打印一个**路径 + 版本号**（不是 command not found / 空）。把这三条的真实输出贴给人；
没成功就停在原地，报告卡在哪一步、什么报错，**不要伪造「已装好」**。

---

## 5. 验收标准（必须全部满足，缺一条就报告失败原因）

- [ ] `pgrep -f agent.py` 输出一个 PID
- [ ] `~/node-agent/logs/<节点名>.log` 出现「平台确认注册成功」
- [ ] 电脑控制台节点页能看到 `<节点名>`，状态在线，型号/SoC/Android 版本有值
- [ ] `curl http://<平台IP>:8000/api/v1/system/health` 返回 `{"status":"ok",...}`

---

## 6. 故障排查表

| 现象 | 原因 | 处置 |
|---|---|---|
| `找不到 agent.py` | 文件没传到位或路径太深 | `find /sdcard -maxdepth 5 -name agent.py` 查真实路径，用 `--src` 指定 |
| 有 PID 但日志没有「平台确认注册成功」 | token 不一致 / 平台地址错 / 平台没开 | `curl http://<平台IP>:8000/api/v1/system/health`；比对两端 token |
| `Agent 没起来` | 依赖没装好 | `tail -n 50 ~/node-agent/logs/nohup.log`；手动 `python3 -m pip install websockets httpx` |
| 连上一会儿就掉线 | 息屏被杀 | 完成保活那一步（§3 Step 5） |
| 节点名报错「只能含字母…」 | 含中文或空格 | 改成 `honor-x70` 这类纯 ASCII 名字 |
| 平台 IP 变了 | 热点重启后 DHCP 重分配 | 人重查电脑 IP（`ipconfig`/手机热点页），重跑脚本 |
| `curl` 平台返回 502 / proxy | 手机设了 HTTP 代理 | `unset http_proxy https_proxy` 后重试 |

---

## 7. 常用命令（交给人 / 你自己后续维护）

```bash
tail -f ~/node-agent/logs/<节点名>.log   # 实时日志
pgrep -f agent.py                        # 进程号
pkill -f agent.py                        # 停 Agent（然后 pgrep 确认死透）
cd ~/node-agent && nohup python3 agent.py >> logs/nohup.log 2>&1 &   # 手动重启
```

---

## 8. 约束（务必遵守）

- **不要修改** `agent.py` / `agent_config.py` / `agent_metrics.py` / `config.toml` 之外的任何代码逻辑。
- **不要为了让验收通过而伪造日志、假造输出或跳过失败步骤。**
- 失败时最多重试 2 次，之后停在原地报告真实原因和完整日志。
- 每一步的真实输出都要给人，不要只说「已完成」。

---

## 9. 当前平台已具备的能力（你接好后就能用，不是画饼）

人已经在电脑上把平台跑到了较完整状态，你接好节点后这些都能用：

- **设备自动发现**：Agent 主动建 WebSocket，无需平台反向连手机。
- **节点注册与心跳**：每 5s 心跳，断线自动重连；状态严格区分 在线/离线/从未上线/停止。
- **指标采集**：内存 / 存储 / 温度 / 网络 / 进程存活 正常；CPU/GPU/NPU 使用率因 Android 权限拿不到真值
  （见 `做不了功能解决方案.md` 的 root/Shizuku 方案），但平台已留好接口，拿到即显示。
- **模型库**：人可在控制台上传模型、查看列表、删除。
- **部署闭环**：人选择模型 + 节点 → 平台下发 → Agent 后台下载（带进度）→ 拉起推理进程 → 状态回写。
- **服务管理**：人可在控制台启停/重启服务、看健康、取日志、配资源准入。
- **API 网关**：平台把请求转发到手机上的服务实例，记录耗时与计数（`/gateway/<service_id>/...`）。

---

## 附：可直接粘贴给 Codex 的提示词（自包含）

> 把下面的 `{{平台IP}}` `{{TOKEN}}` `{{节点名}}` 替换成真实值后，整段发给 Codex 即可。

```text
我要在 Android 手机的 Termux 里，把一个节点 Agent 接入局域网内的边缘计算平台。
请严格按这份指南执行：edge-cluster-latest/CODEX_TERMUX_GUIDE.md
（若你读不到该文件，下面是你必须知道的事实与步骤）

已知事实：
- 平台地址：http://{{平台IP}}:8000
- 共享 token：{{TOKEN}}
- 节点名：{{节点名}}（只能用字母/数字/下划线/连字符/点，且这部手机唯一）
- 平台 health 接口无需鉴权：GET /api/v1/system/health 应返回 {"status":"ok",...}
- node-agent 代码应该已经在手机上（可能在 /sdcard/Download 下任意子目录，也可能在 ~/node-agent）。
  用 find /sdcard/Download /sdcard $HOME -maxdepth 5 -name agent.py 定位。
- 一键脚本在 node-agent/phone_setup.sh（若不在当前目录，先 find 它并 cd 到所在目录）。

任务：
1. 先执行 curl -s --max-time 5 http://{{平台IP}}:8000/api/v1/system/health，
   拿不到 {"status":"ok"} 就停下来报告「平台不可达」，不要继续。
2. 执行：bash phone_setup.sh --platform http://{{平台IP}}:8000 --name {{节点名}}
   （若 agent.py 不在 ~/node-agent，加 --src <真实目录>）
3. 等待脚本跑完，把 stdout 原样贴给我。
4. （第二阶段，强烈建议顺手做）装推理运行时 llama.cpp，让 llama-cli 和 llama-server 都进 PATH
   ——按指南第 4 节 Step A~D 执行，把验证输出贴给我。

验收标准（必须全部满足，缺一条就报告失败原因）：
- pgrep -f agent.py 输出一个 PID
- ~/node-agent/logs/{{节点名}}.log 里出现「平台确认注册成功」
- 控制台（电脑浏览器打开平台）节点页能看到 {{节点名}} 且状态在线

第二阶段（部署模型 / 跑服务前，由人确认后再做）：
- command -v llama-cli llama-server 各打印一个路径

约束：
- 不要修改 agent.py / agent_config.py / agent_metrics.py / config.toml 之外的任何代码逻辑
- 不要为了让验收通过而伪造日志、假造输出或跳过失败步骤
- 失败时最多重试 2 次，之后停在原地报告真实原因和完整日志
- 每一步的真实输出都要给我，不要只说「已完成」
```
