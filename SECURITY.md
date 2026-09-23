# 安全策略

## 支持范围

本项目处于早期阶段（MVP），只对**最新的里程碑版本**提供安全修复：

| 版本 | 支持状态 |
|---|---|
| `0.2.x`（最新） | ✅ 接受安全报告 |
| `< 0.2` | ❌ 不再维护 |

## 报告安全问题

**请不要开公开 Issue。**

推荐用 GitHub 的
[Private vulnerability reporting](../../security/advisories/new)
（仓库 → Security → Report a vulnerability），只有维护者能看到。

如果该入口不可用，请开一个**不含细节**的公开 Issue，只写"我有一个安全问题想私下报告"，
维护者会与你建立私密联系渠道。

### 请在报告中包含

1. 问题类型（例如：命令注入、路径穿越、鉴权绕过、信息泄漏）
2. 受影响的文件 / 接口 / 版本
3. 复现步骤（最小可复现优先）
4. 影响评估：攻击者能做什么？需要什么前提？
5. 如果你有修复思路，欢迎一并说明

### 响应预期

这是个人维护的开源项目，**没有 SLA 承诺**。正常情况下：

- 3 天内确认收到
- 7 天内给出初步判断（是否确认为漏洞、严重程度）
- 修复后会在 [CHANGELOG](CHANGELOG.md) 中致谢（除非你要求匿名）

---

## 已知的设计取舍（不是漏洞，但你需要知道）

以下都是**刻意为之**的 MVP 取舍，在提报告前请先确认不是你已知的边界。

### 1. 共享 token 明文传输，无 HTTPS

**现状**：平台与 Agent 共用一个 token（`server/config.toml` 的 `token` 与
`node-agent/config.toml` 的 `token` 必须一致）。WebSocket 握手在 query 里带 token，
REST 走 `X-Auth-Token` 头，UDP 发现包也带 token。
**所有这些都是明文，没有 TLS。**

**后果**：同一局域网内可抓包的人能拿到 token。

**为什么这么做**：MVP 面向的是**自己家的局域网**。引入 HTTPS 需要证书签发与分发流程，
对"几台自己手机的集群"来说是过度设计。

**⚠️ 关键风险点**：**里程碑 4（服务管理）之后，这个 token 等于"在节点上执行命令"的能力。**
如果你在任何不完全可信的网络上部署，请先：
- 换掉默认 token（`server/config.toml` 与 `node-agent/config.toml` 都要改，且保持一致）
- 用防火墙限制 8000 / 37020 端口的访问来源
- 不要把这套东西直接暴露到公网

相关讨论见 [`docs/design/requirement-review.md`](docs/design/requirement-review.md) 的 O7。

### 2. 无用户体系，无权限分级

只有"持有 token"和"没有 token"两种状态。任何人都能做任何操作（改节点名、删记录、下发模型、启停服务）。
这是原需求明确排除的范围（"不做用户系统"）。

### 3. 模型下载接口不做路径穿越防护之外的额外校验

`GET /api/v1/models/{model_id}/download` 按 `model_id` 查库拿 `storage_path` 后读取文件。
`model_id` 由平台生成，不是用户直接提供的路径。
但如果你修改了这部分代码，请确保**不要把用户输入直接拼进文件路径**。

### 4. `platform_url` 配置项接受任意主机

Agent 的 `--platform-url` / `config.toml` 里的 `platform_url` 可以指向任何地址。
如果攻击者能改这个配置文件，就能把 Agent 指向自己的"平台"并窃取 token。
这是**配置文件的信任边界问题**，不是代码漏洞 —— 保护好你的配置文件。

### 5. 子进程托管会执行配置里的命令（M4 起）

服务定义里的 `command` / `args` 会被 Agent 用 `subprocess.Popen` 执行。
平台侧能改这些参数的任何人都等于能在这台手机上执行任意命令。
这**是功能本身**（"让用户自己写启动命令，平台不必预先支持任何引擎"），
但意味着 token 的保密性至关重要。

---

## 部署安全清单

上线前请逐条确认：

- [ ] `server/config.toml` 与 `node-agent/config.toml` 里的 `token` 都已改成自己的强随机值，且**两者一致**
- [ ] 8000 端口（REST/WS）**没有**映射到公网
- [ ] 37020 端口（UDP 发现）只在本网段可达
- [ ] 不用的节点记录已从平台删除（省得残留旧 token 对应的记录）
- [ ] 了解"离线节点不下发旧值"是设计而非故障
- [ ] 知道日志里会包含内网 IP 与设备型号，分享日志前先脱敏

## 依赖安全

Python 依赖版本在 `server/requirements.txt`（及 `pyproject.toml`）与
`node-agent/requirements.txt` 中声明。前端依赖见 `web/package.json`。

本项目会在 CI 中跑基础验收（见 `.github/workflows/ci.yml`），
但**不包含**依赖漏洞扫描。如果你在意，建议本地跑：

```bash
pip install pip-audit && pip-audit -r server/requirements.txt
cd web && npm audit
```
