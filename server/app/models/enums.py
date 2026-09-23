"""状态枚举定义 —— 三端共用契约，禁止随意改动。

区分「没有上报 / 节点离线 / 服务停止 / 服务异常」四种情况是本项目的硬要求：
禁止用"没数据就等于离线"糊弄。
"""

from __future__ import annotations

from enum import Enum


class NodeStatus(str, Enum):
    """节点在线状态（三态）。"""

    NEVER_SEEN = "never_seen"  # 从未收到过心跳 —— "没有上报"
    ONLINE = "online"          # 最近一次心跳在阈值内
    OFFLINE = "offline"        # 曾上线，但心跳超时 —— "节点离线"


class ServiceStatus(str, Enum):
    """模型服务状态（四态）。"""

    STOPPED = "stopped"    # 进程不存在（含人工停止）
    STARTING = "starting"  # 进程已拉起，健康检查尚未通过
    RUNNING = "running"    # 进程存在 且 健康检查通过
    ERROR = "error"        # 进程存在 但 健康检查连续失败 —— "服务异常"


class DeployNodeStatus(str, Enum):
    """单个节点上的部署进度状态。"""

    PENDING = "pending"
    DOWNLOADING = "downloading"
    VERIFYING = "verifying"
    INSTALLING = "installing"
    READY = "ready"
    FAILED = "failed"


class DeployTaskStatus(str, Enum):
    """部署任务的聚合状态。"""

    PENDING = "pending"
    RUNNING = "running"
    PARTIAL = "partial"      # 部分成功、部分失败
    SUCCEEDED = "succeeded"
    FAILED = "failed"


class ApiType(str, Enum):
    """模型服务对外提供的 API 类型。"""

    LLM = "llm"
    VLM = "vlm"
    VISION = "vision"
    OCR = "ocr"
    OTHER = "other"


class Framework(str, Enum):
    """模型框架。"""

    LLAMA_CPP = "llama.cpp"
    ONNX = "onnx"
    MNN = "mnn"
    QNN = "qnn"
    OTHER = "other"


# 部署任务的终态，用于判断任务是否还在跑
DEPLOY_TERMINAL_NODE_STATES = {DeployNodeStatus.READY, DeployNodeStatus.FAILED}


def aggregate_deploy_status(node_statuses: list[str]) -> str:
    """根据逐节点状态聚合出任务状态。

    - 全部 ready                 → succeeded
    - 全部 failed                → failed
    - 有 ready 也有 failed       → partial
    - 有还在进行中的              → running / pending
    """
    if not node_statuses:
        return DeployTaskStatus.PENDING.value

    states = set(node_statuses)
    done = states <= {DeployNodeStatus.READY.value, DeployNodeStatus.FAILED.value}
    if not done:
        return (
            DeployTaskStatus.PENDING.value
            if states == {DeployNodeStatus.PENDING.value}
            else DeployTaskStatus.RUNNING.value
        )

    ready = DeployNodeStatus.READY.value in states
    failed = DeployNodeStatus.FAILED.value in states
    if ready and failed:
        return DeployTaskStatus.PARTIAL.value
    return DeployTaskStatus.SUCCEEDED.value if ready else DeployTaskStatus.FAILED.value
