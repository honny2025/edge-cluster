# 文档索引

## 先看这两份

| 文档 | 什么时候看 |
|---|---|
| [user-manual.md](user-manual.md) | **怎么用**。要准备什么、操作步骤、注意事项、故障排查。第 0 章是「实现状态总表」，按需求逐条标注 ✅ 现在就能用 / 🟡 能用但有折扣 / ❌ 明确不做 |
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

> M3~M5 的验收**脚本**已经写好并全部实测通过（见下方「验收脚本」一节），
> 逐里程碑的验收报告文档尚未补写 —— 验收数字以
> [`../CHANGELOG.md`](../CHANGELOG.md) 的 `0.5.0-m5` 一节和
> [`实现情况报告.md`](reports/实现情况报告.md) 为准。

---

## 部署

| 文档 | 内容 |
|---|---|
| [deployment.md](deployment.md) | 部署文档（PC + Termux 真机），含 Linux systemd 与防火墙说明 |

---

## 验收脚本

| 脚本 | 检查项 | 用法 |
|---|---|---|
| [acceptance/config_check.py](acceptance/config_check.py) | 13 项配置层回归自检（含旧键兼容优先级） | `python docs/acceptance/config_check.py`（秒级，不用起服务） |
| [acceptance/m1_check.py](acceptance/m1_check.py) | 11 项在线 / 8 项离线通信闭环验收 | `python docs/acceptance/m1_check.py --phase online` |
| [acceptance/m2_check.py](acceptance/m2_check.py) | 17 项在线 / 16 项离线监控验收 | `python docs/acceptance/m2_check.py --phase online` |
| [acceptance/m3_check.py](acceptance/m3_check.py) | 22 项模型库验收（含 `Range` / `ETag` / sha256） | 见 [acceptance/README.md](acceptance/README.md)（需隔离实例） |
| [acceptance/m3_deploy_check.py](acceptance/m3_deploy_check.py) | 11 项模型下发全流程 | 同上（需隔离实例） |
| [acceptance/m2_ip_check.py](acceptance/m2_ip_check.py) | 3 项节点 IP 取值来源 | 同上（需隔离实例） |
| [acceptance/agent_metrics_check.py](acceptance/agent_metrics_check.py) | 41 项采集降级链 | `python docs/acceptance/agent_metrics_check.py` |
| [acceptance/files_check.py](acceptance/files_check.py) | 文件管理（沙箱逃逸、令牌边界） | `python docs/acceptance/files_check.py` |
| [acceptance/m5_selfheal_check.py](acceptance/m5_selfheal_check.py) | 服务自恢复（prepare / verify 两步） | 见 [acceptance/README.md](acceptance/README.md) |
| [acceptance/m6_model_deploy_check.py](acceptance/m6_model_deploy_check.py) | 模型登记 / 兼容预检 / 部署 / Self Test / 取消 | `python docs/acceptance/m6_model_deploy_check.py` |
| [acceptance/metric_tiers_check.py](acceptance/metric_tiers_check.py) | 37 项指标分层保留（聚合正确性 / 顺序安全 / 查询分层不重叠） | `python docs/acceptance/metric_tiers_check.py`（自建临时库，跑完删干净） |
| [acceptance/backup_check.py](acceptance/backup_check.py) | 42 项备份与恢复（快照自包含 / 坏包识别 / WAL 不回流） | `python docs/acceptance/backup_check.py`（自建临时库，跑完删干净） |
| [acceptance/rollback_check.py](acceptance/rollback_check.py) | 42 项部署失败回滚（真跑 `_deploy_model`，逐字节比对设备文件） | `python docs/acceptance/rollback_check.py`（自建临时库 + 本机 HTTP 服务） |

**跑验收的前置条件**：平台与节点已在跑（`.\start_all.ps1` 或 `./start_all.sh`）。
`--phase offline` 的两支需要**先停掉至少 1 个 Agent**；
`m3_check` / `m3_deploy_check` / `m2_ip_check` 会写入数据库，**必须起隔离实例再跑**。
完整说明见 [acceptance/README.md](acceptance/README.md)。

---

## 根目录文档

| 文档 | 内容 |
|---|---|
| [../README.md](../README.md) | 项目总入口：为什么做、现在能用什么、快速开始、项目结构、关键技术点、常见问题 |
| [../ROADMAP.md](../ROADMAP.md) | 路线图：M1~M5 的实际交付、与原计划的偏差及原因、长期方向、**明确不做的事** |
| [../CHANGELOG.md](../CHANGELOG.md) | 更新日志：`0.5.0-m5` 的完整变更记录与实测数据（M1 / M2 的历史条目也在这里） |
| [../功能清单.md](../功能清单.md) | **「已经实现了什么 / 还缺什么」的直接答案**。按需求 §1~§9 逐条列 ✅/🟡/❌，缺的部分拆成「可做没做 / 环境做不了 / 决定不做」三类 |
| [实现情况报告.md](reports/实现情况报告.md) | **一页纸看清实现了什么、没实现什么**，含 13 支验收脚本的实测数字 |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | 贡献指南：必须跑的验收、几条项目特有的纪律、Windows 脚本编码规则 |
| [../SECURITY.md](../SECURITY.md) | 安全策略：已知的设计取舍（明文 token 等）、部署安全清单 |
| [../快速开始.txt](../快速开始.txt) | 极简三步指南（面向"拿到这个包就想跑起来"的人） |
| [../系统迁移与使用指南.md](../系统迁移与使用指南.md) | **拿到包先读这份**：换电脑怎么搬、三条命令跑起来、界面导览、配置速查、排错顺序、验收脚本 |
| [../平台使用指南.md](../平台使用指南.md) | **界面每一块是干什么的**，`—` 到底是什么意思，名词表，最容易搞错的几个点 |
| [../手机端部署指南.md](../手机端部署指南.md) | **可以直接丢给一个 AI 代理让它自己完成手机端部署**，内含可复制粘贴的提示词 |

### 根目录的其余文档

| 文档 | 内容 |
|---|---|
| [真机测试检查单.md](guides/真机测试检查单.md) | **接真机时照这份做**。准备清单、防火墙、保活配置、T1~T10 测试项、指标探针、故障速查、可照抄的测试记录表 |
| [迁移说明.txt](guides/迁移说明.txt) | 迁移包的一页速览（详细版见「系统迁移与使用指南.md」） |
| [工作室局域网实施步骤.md](guides/工作室局域网实施步骤.md) | 从零到跑通的分步操作，含迁移后验证与真机测试顺序 |
| [真机接入-Termux端.md](guides/真机接入-Termux端.md) | 手机侧（Termux）接入的详细步骤与排查 |
| [CODEX_TERMUX_GUIDE.md](guides/CODEX_TERMUX_GUIDE.md) | 给 AI 代理（Codex 等）自主执行的手机接入指南 |
| [平台设计文档.md](design/平台设计文档.md) | 平台定位与边界、分层设计、当前实现与缺口 |
| [需求覆盖评估与实施计划.md](design/需求覆盖评估与实施计划.md) | 需求 §1~§9 逐条覆盖评估 + 真机暴露出的硬约束（含 121 条真实采样统计） |
| [做不了功能解决方案.md](design/做不了功能解决方案.md) | 五类"做不了"的现象、真因与解决办法（含 root / Shizuku 提权路线） |
| [ADB接入决策评估与优化建议.md](design/ADB接入决策评估与优化建议.md) | ADB 与 Shizuku 的架构取舍：**ADB 该出现在装机脚本里，不该出现在架构图里** |

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
