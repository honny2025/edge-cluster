# Termux 端接入（手机上的操作）

> 配套：`真机测试检查单.md`（平台侧 + 验收项）、`node-agent/phone_setup.sh`（本文件的执行主体）
> 适用：Android + Termux，代码已经传到手机（放在 Download 任意子目录都行，脚本会自己找）

---

## 0. 先把脚本送进手机

`phone_setup.sh` 是新加的，你之前拷的 `node-agent` 里没有它。

- 电脑上：把 `node-agent/phone_setup.sh` 传到手机（QQ / 微信 / 数据线都行，落到 Download）
- 手机上 Termux：

```bash
cp /sdcard/Download/phone_setup.sh ~/phone_setup.sh
```

它跟 `agent.py` 不在同一个目录也没关系，脚本会自己去找 `agent.py`。

---

## 1. 最快路径：一条命令搞定

```bash
cd ~ && bash phone_setup.sh --platform http://10.86.33.190:8000 --name honor-x70
```

脚本会自动完成：**找 agent.py → 拷进 `~/node-agent` → 装 python 与依赖 → 写配置 → 启动 → 验证**，
全程无交互，30 秒到 3 分钟（首次装依赖会慢）。

跑完看最后几行：

- `[OK] Agent 运行中 PID=xxxxx`
- `[OK] 平台已确认注册 —— 接入成功`

看到这两行就结束了。

---

## 2. 想让 codex 代劳：把这段直接粘给它

> 提示词是自包含的（codex 看不到我们的对话），平台 IP 换成你当前的真实值。

```text
我要在 Android 手机的 Termux 里，把一个节点 Agent 接入局域网内的边缘计算平台。

已知事实：
- 平台地址：http://10.86.33.190:8000
- 共享 token：edge-cluster-dev-token
- 节点名：honor-x70（只能用字母/数字/下划线/连字符）
- 平台 health 接口无需鉴权：GET /api/v1/system/health 应返回 {"status":"ok",...}
- node-agent 代码已经传到手机，具体路径不确定，可能在 /sdcard/Download 下的任意子目录，
  也可能已经在 ~/node-agent。用 find /sdcard -maxdepth 5 -name agent.py 定位。
- 一键脚本在 ~/phone_setup.sh（如果不在，先 find /sdcard -maxdepth 4 -name phone_setup.sh 找到后拷到 ~）

任务：
1. 先执行 curl -s --max-time 5 http://10.86.33.190:8000/api/v1/system/health，
   拿不到 {"status":"ok"} 就停下来报告「平台不可达」，不要继续。
2. 执行：cd ~ && bash phone_setup.sh --platform http://10.86.33.190:8000 --name honor-x70
3. 等待脚本跑完，把 stdout 原样贴给我。

验收标准（必须全部满足，缺一条就报告失败原因）：
- pgrep -f agent.py 输出一个 PID
- ~/node-agent/logs/honor-x70.log 里出现「平台确认注册成功」
- 控制台（电脑浏览器打开平台）节点页能看到 honor-x70 且状态在线

约束：
- 不要修改 agent.py / agent_config.py / agent_metrics.py / config.toml 之外的任何代码逻辑
- 不要为了让验收通过而伪造日志、假造输出或跳过失败步骤
- 失败时最多重试 2 次，之后停在原地报告真实原因和完整日志
- 每一步的真实输出都要给我，不要只说「已完成」
```

> codex 默认会逐条命令要确认，嫌烦就用它的自动批准模式；
> 但说实话——**自己跑 §1 那一条命令更快**，codex 更适合用来排查卡住的问题。

---

## 3. 脚本参数

| 参数 | 必填 | 说明 |
|---|---|---|
| `--platform URL` | 建议填 | 平台地址，如 `http://10.86.33.190:8000`。**不填**则走 UDP 广播自动发现（需同网段且无客户端隔离） |
| `--name NAME` | 是 | 节点名，只能用字母/数字/下划线/连字符/点。多部手机不能同名 |
| `--token T` | 否 | 默认 `edge-cluster-dev-token`，与平台 `server/config.toml` 一致即可 |
| `--src PATH` | 否 | 手动指定 node-agent 所在目录（自动定位失败时用） |
| `--no-kill` | 否 | 不杀掉已运行的 agent.py |

幂等：可重复执行；`~/node-agent` 已存在时会保留 `state/`（节点身份）和 `logs/`，不会把你变成重复节点。

---

## 4. 验收（自己确认一遍）

```bash
pgrep -f agent.py                              # 输出一个 PID
tail -n 20 ~/node-agent/logs/honor-x70.log     # 有「平台确认注册成功」
cat ~/node-agent/config.toml | grep -E '^(name|platform_url)'   # 确认配置写对了
```

电脑浏览器打开 `http://127.0.0.1:8000` → 节点页 → 应出现 **honor-x70**，在线，型号/SoC/Android 版本有值。

---

## 5. 保活（**必须手动做，脚本做不了**）

| # | 位置 | 动作 |
|---|---|---|
| 1 | 设置 → 应用 → Termux → 省电策略 | 无限制 / 允许后台活动 |
| 2 | 设置 → 应用 → Termux | 允许自启动 |
| 3 | 最近任务列表 → Termux | 下拉加锁 |
| 4 | Termux 内 | `termux-wake-lock`（需 Termux:API，脚本会尝试自动申请） |

荣耀 / MagicOS 额外：设置 → 应用 → 应用启动管理 → Termux → 关掉「自动管理」，手动全开三个开关。

---

## 6. 排查

| 现象 | 原因 | 处置 |
|---|---|---|
| `找不到 agent.py` | 文件没传到位或路径太深 | `find /sdcard -maxdepth 5 -name agent.py` 查真实路径，用 `--src` 指定 |
| 有 PID 但日志没有「平台确认注册成功」 | token 不一致 / 平台地址错 / 平台没开 | `curl http://<平台IP>:8000/api/v1/system/health`；比对两端 token |
| `Agent 没起来` | 依赖没装好 | `tail -n 50 ~/node-agent/logs/nohup.log`；手动 `python3 -m pip install websockets httpx` |
| 连上一会儿就掉线 | 息屏被杀 | 完成第 5 节保活 |
| 节点名报错「只能含字母…」 | 含中文或空格 | 改成 `honor-x70` 这类纯 ASCII 名字 |
| 平台 IP 变了 | 手机热点重启后 DHCP 重新分配 | 电脑上 `ipconfig \| Select-String "IPv4"` 查 WLAN 新 IP，重跑脚本 |

---

## 7. 常用命令

```bash
tail -f ~/node-agent/logs/honor-x70.log     # 实时日志
pgrep -f agent.py                            # 进程号
pkill -f agent.py                            # 停 Agent（然后 pgrep 确认死透）
cd ~/node-agent && nohup python3 agent.py >> logs/nohup.log 2>&1 &   # 手动重启
```
