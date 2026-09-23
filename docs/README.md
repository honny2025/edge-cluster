# 文档索引

## 先看这两份

| 文档 | 什么时候看 |
|---|---|
| [user-manual.md](user-manual.md) | **怎么用**。要准备什么、操作步骤、注意事项、故障排查。第 1 章是「实现状态总表」，按需求逐条标注 ✅ 已可用 / ⚠️ 部分 / ⏳ 规划中 |
| [design/requirement-review.md](design/requirement-review.md) | **打算改代码前必读**。10 条待决策项（O1~O10），每条标注 `[实测]` 或 `[判断]`。其中 O1/O2/O4/O6 会改变数据库表结构或架构走向 |

---

## 设计与决策

| 文档 | 内容 |
|---|---|
| [design/architecture.md](design/architecture.md) | 文字版架构图、5 类通信方式总表、端口规划、数据流时序、安全与鉴权、架构预留点 |
| [design/tech-decisions.md](design/tech-decisions.md) | **三端共同契约**：最终目录结构、5 张字段表全量、冻结的状态枚举代码、完整 API 清单、WS 消息协议、配置项全文、验收命令约定 |
| [design/optimized-spec.md](design/optimized-spec.md) | 优化后的产品与技术规格：版本路线（V0.2~V0.6）、统一核心对象、状态集合、分阶段验收标准、建议开发顺序 |

---

## 里程碑记录

| 文档 | 内容 |
|---|---|
| [milestones/milestone-1-plan.md](milestones/milestone-1-plan.md) | M1 任务拆分（T1.1~T1.12）+ 依赖图 + 验收脚本设计表 |
| [milestones/milestone-1-report.md](milestones/milestone-1-report.md) | M1 验收报告：交付物清单、11/11 验收结果、手工闭环实测数据、离线判定取舍、环境坑汇总 |
| [milestones/milestone-2-report.md](milestones/milestone-2-report.md) | M2 验收报告：17/17 + 16/16 验收结果、5 条关键设计决策与实测佐证、修的真实缺陷 |

> M3~M5 的验收报告将在对应里程碑完成后加入本目录。

---

## 部署

| 文档 | 内容 |
|---|---|
| [deployment.md](deployment.md) | 部署文档（PC + Termux 真机），含 Linux systemd 与防火墙说明 |

---

## 验收脚本

| 脚本 | 检查项 | 用法 |
|---|---|---|
| [acceptance/config_check.py](acceptance/config_check.py) | 8 项配置层回归自检 | `python docs/acceptance/config_check.py`（秒级，不用起服务） |
| [acceptance/m1_check.py](acceptance/m1_check.py) | 11 项通信闭环验收 | `python docs/acceptance/m1_check.py --phase online` |
| [acceptance/m2_check.py](acceptance/m2_check.py) | 17 项在线 / 16 项离线监控验收 | `python docs/acceptance/m2_check.py --phase online` |

**跑验收的前置条件**：平台与节点已在跑（`.\start_all.ps1` 或 `./start_all.sh`）。
`m2_check.py --phase offline` 需要**先停掉至少 1 个 Agent**。

---

## 根目录文档

| 文档 | 内容 |
|---|---|
| [../README.md](../README.md) | 项目总入口：为什么做、现在能用什么、快速开始、项目结构、关键技术点、常见问题 |
| [../ROADMAP.md](../ROADMAP.md) | 路线图：M3~M5 的交付物与验收场景，每个阶段动手前需先决策的事项 |
| [../CHANGELOG.md](../CHANGELOG.md) | 更新日志：M1 / M2 的完整变更记录与实测数据 |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | 贡献指南：必须跑的验收、几条项目特有的纪律、Windows 脚本编码规则 |
| [../SECURITY.md](../SECURITY.md) | 安全策略：已知的设计取舍（明文 token 等）、部署安全清单 |
| [../快速开始.txt](../快速开始.txt) | 极简三步指南（面向"拿到这个包就想跑起来"的人） |

---

## screenshots/

放界面截图。目前为空 —— 如果你跑起来了愿意帮忙截几张，欢迎提 PR。

建议命名的文件：

| 文件名 | 内容 |
|---|---|
| `dashboard.png` | 首页大屏（含汇总卡、告警面板、趋势图、节点列表） |
| `node-detail.png` | 节点详情（含 4 张历史曲线） |
| `nodes.png` | 节点列表 |
| `offline-state.png` | 离线节点状态（展示「—」与压暗行） |
| `chart-gaps.png` | 曲线断点示例（展示"拿不到 ≠ 0"） |

截图请**脱敏**：不要包含内网 IP、真实设备型号等你不想公开的信息。
