---
name: Bug 报告
description: 报告一个可复现的问题
title: "[Bug] "
labels: ["bug", "needs-triage"]
body:
  - type: markdown
    attributes:
      value: |
        感谢反馈！为了让问题能被定位，**请尽量填完下面的信息**。
        信息不全的 Issue 往往只能来回问好几轮，拖慢修复。

        ⚠️ **提交日志前请先脱敏**：删掉内网 IP、token、设备型号等你不想公开的信息。

  - type: checkboxes
    id: preflight
    attributes:
      label: 提交前确认
      options:
        - label: 我已经读过 [README](https://github.com/honny/edge-cluster#常见问题) 的常见问题章节
          required: true
        - label: 我已经搜过现有 Issue，没有重复
          required: true
        - label: 我提供的内容已经脱敏（无 token / 无内网 IP / 无个人设备信息）
          required: true

  - type: textarea
    id: what-happened
    attributes:
      label: 问题描述
      description: 发生了什么？你期望发生什么？
      placeholder: |
        实际：起 2 个模拟节点后，kill 掉 phone-02，等了 60s 节点仍显示 online
        期望：阈值内应标记为 offline
    validations:
      required: true

  - type: textarea
    id: steps
    attributes:
      label: 复现步骤
      description: 越具体越好，包括用的命令行参数
      placeholder: |
        1. .\start_all.ps1
        2. 等 2 个节点显示 online
        3. 用 Get-CimInstance 杀掉 phone-02 的 agent.py 进程
        4. 打开 http://127.0.0.1:8000 观察节点状态
    validations:
      required: true

  - type: input
    id: version
    attributes:
      label: 版本
      description: 运行 `curl -H "X-Auth-Token: <你的token>" http://127.0.0.1:8000/api/v1/system/health` 的输出
      placeholder: '{"status":"ok","version":"0.2.0-m2"}'
    validations:
      required: true

  - type: dropdown
    id: platform
    attributes:
      label: 运行平台
      description: 平台服务端跑在哪？
      options:
        - Windows
        - Linux
        - macOS
        - WSL
    validations:
      required: true

  - type: dropdown
    id: node-type
    attributes:
      label: 节点类型
      description: 出问题的节点是模拟器还是真机？
      options:
        - 模拟节点（--simulate）
        - Termux 真机
        - 两者都有
        - 不涉及节点（平台侧问题）
    validations:
      required: true

  - type: input
    id: python-version
    attributes:
      label: Python 版本
      description: 运行 `python -V` 的输出
      placeholder: "Python 3.13.12"
    validations:
      required: true

  - type: textarea
    id: logs
    attributes:
      label: 相关日志
      description: |
        - 平台日志：`server/data/logs/server.log`
        - Agent 输出：启动 Agent 的那个终端
        请只贴**相关片段**，不要贴几千行。
      render: text
    validations:
      required: false

  - type: checkboxes
    id: phase
    attributes:
      label: 涉及的里程碑
      description: 你遇到的问题属于哪个阶段？（可多选）
      options:
        - label: M1 通信闭环（发现 / 注册 / 心跳 / 状态机）
        - label: M2 状态监控（指标 / 大屏 / 告警 / 曲线）
        - label: M3 模型下发（尚未实现）
        - label: M4 服务管理（尚未实现）
        - label: 一键脚本 / 部署 / 打包

  - type: textarea
    id: extra
    attributes:
      label: 补充信息
      description: 其他你认为有用的线索（截图、你的猜测、已经试过什么）
    validations:
      required: false
