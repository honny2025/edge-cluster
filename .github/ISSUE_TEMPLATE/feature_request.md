---
name: 功能建议
description: 提议一个新功能或改进
title: "[Feature] "
labels: ["enhancement", "needs-triage"]
body:
  - type: markdown
    attributes:
      value: |
        感谢建议！在填写之前，**请先看一眼这两份文档**：

        - [ROADMAP.md](https://github.com/honny/edge-cluster/blob/main/ROADMAP.md) —— 后续计划与已识别的待决策项
        - [需求评审与优化建议.md](https://github.com/honny/edge-cluster/blob/main/docs/design/requirement-review.md) —— 10 条待决策项（O1~O10）

        你的建议可能已经在里面了。如果确实在里面，欢迎直接在对应条目的 Issue 下补充想法。

  - type: checkboxes
    id: preflight
    attributes:
      label: 提交前确认
      options:
        - label: 我已读过 ROADMAP.md，这个建议不在已规划范围内
          required: true
        - label: 我已搜过现有 Issue，没有重复
          required: true

  - type: dropdown
    id: type
    attributes:
      label: 建议类型
      options:
        - 新功能（现在完全没有的能力）
        - 改进现有功能
        - 文档改进
        - 开发体验（脚本 / CI / 测试）
        - 性能优化
    validations:
      required: true

  - type: textarea
    id: problem
    attributes:
      label: 你想解决什么问题
      description: |
        **请先讲问题，再讲方案。** 描述你实际遇到的困难，
        而不是一上来就说"应该加个 XXX 功能"。
        讲清问题，方案可能有很多种；只讲方案，讨论就只能围着你的方案转。
      placeholder: |
        我有 5 台手机要下发同一个模型，现在得一台一台点。
        但我的网络带宽只有 100Mbps，同时下发会不会把心跳挤掉导致集体误判离线？
    validations:
      required: true

  - type: textarea
    id: solution
    attributes:
      label: 你设想的方案
      description: 如果你有想法就写，没有也没关系 —— 讲清问题本身已经很有价值
    validations:
      required: false

  - type: textarea
    id: alternatives
    attributes:
      label: 考虑过的替代方案
      description: 你试过或用过什么别的做法？为什么不够好？
    validations:
      required: false

  - type: dropdown
    id: milestone
    attributes:
      label: 你认为应该放在哪个阶段
      options:
        - M3（模型库 + 模型下发）
        - M4（服务管理 + API 暴露）
        - M5（稳定性收尾）
        - 长期（需要先讨论架构）
        - 不确定
    validations:
      required: true

  - type: textarea
    id: extra
    attributes:
      label: 补充信息
      description: 参考实现、相关 Issue、截图等
    validations:
      required: false
