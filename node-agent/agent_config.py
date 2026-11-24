"""Agent 配置加载。

优先级：命令行参数 > 环境变量（EDGE_SECTION__KEY） > config.toml > 内置默认值。
node_id 留空时自动生成并**回写配置文件**，保证重启后 node_id 稳定（不会产生重复节点）。
"""

from __future__ import annotations

import json
import os
import re
import socket
import uuid
from dataclasses import dataclass, field, fields
from pathlib import Path
from typing import Any

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

AGENT_VERSION = "0.5.0-m5"
BASE_DIR = Path(__file__).resolve().parent


@dataclass
class AgentCfg:
    node_id: str = ""
    name: str = ""
    token: str = "edge-cluster-dev-token"
    heartbeat_interval: int = 5
    platform_url: str = ""
    platform_port: int = 8000


@dataclass
class DiscoveryCfg:
    enabled: bool = True
    port: int = 37020
    broadcast_addr: str = "255.255.255.255"
    attempts: int = 3
    interval: float = 1.0
    timeout: float = 1.5


@dataclass
class ReconnectCfg:
    base_delay: float = 1.0
    max_delay: float = 60.0
    jitter: float = 0.2


@dataclass
class StorageCfg:
    models_dir: str = "./models"
    logs_dir: str = "./logs"
    log_max_bytes: int = 5 * 1024 * 1024
    log_backup_count: int = 3


@dataclass
class ServiceCfg:
    health_timeout: int = 60
    health_fail_threshold: int = 3
    health_interval: int = 3
    restart_backoff: int = 3
    stop_grace: int = 5


@dataclass
class SimulateCfg:
    enabled: bool = False
    model: str = "Xiaomi 13"
    soc: str = "Snapdragon 8 Gen 2"
    android_version: str = "14"
    fail_download_once: bool = False
    crash_service_after: int = 0
    disconnect_after: int = 0


@dataclass
class Config:
    agent: AgentCfg = field(default_factory=AgentCfg)
    discovery: DiscoveryCfg = field(default_factory=DiscoveryCfg)
    reconnect: ReconnectCfg = field(default_factory=ReconnectCfg)
    storage: StorageCfg = field(default_factory=StorageCfg)
    service: ServiceCfg = field(default_factory=ServiceCfg)
    simulate: SimulateCfg = field(default_factory=SimulateCfg)
    config_path: Path = field(default_factory=lambda: BASE_DIR / "config.toml")

    @property
    def models_path(self) -> Path:
        return _resolve(self.storage.models_dir)

    @property
    def logs_path(self) -> Path:
        return _resolve(self.storage.logs_dir)

    def ensure_dirs(self) -> None:
        for p in (self.models_path, self.logs_path):
            p.mkdir(parents=True, exist_ok=True)

    def ws_url(self, platform_url: str | None = None) -> str:
        base = (platform_url or self.agent.platform_url or "").rstrip("/")
        if not base:
            raise ValueError("平台地址为空")
        if base.startswith("http://"):
            base = "ws://" + base[len("http://"):]
        elif base.startswith("https://"):
            base = "wss://" + base[len("https://"):]
        elif not base.startswith(("ws://", "wss://")):
            base = "ws://" + base
        if ":" not in base.split("//", 1)[-1]:
            base = f"{base}:{self.agent.platform_port}"
        return f"{base}/ws/agent"


def _resolve(p: str) -> Path:
    path = Path(p)
    return path if path.is_absolute() else (BASE_DIR / path)


_SECTION_TYPES = {
    "agent": {f.name: type(f.default) for f in fields(AgentCfg)},
    "discovery": {f.name: type(f.default) for f in fields(DiscoveryCfg)},
    "reconnect": {f.name: type(f.default) for f in fields(ReconnectCfg)},
    "storage": {f.name: type(f.default) for f in fields(StorageCfg)},
    "service": {f.name: type(f.default) for f in fields(ServiceCfg)},
    "simulate": {f.name: type(f.default) for f in fields(SimulateCfg)},
}


def _coerce(section: str, key: str, value: Any) -> Any:
    want = _SECTION_TYPES.get(section, {}).get(key)
    if want is bool:
        return str(value).strip().lower() in {"1", "true", "yes", "on"} if not isinstance(value, bool) else value
    if want is int:
        try:
            return int(float(value))
        except (TypeError, ValueError):
            return value
    if want is float:
        try:
            return float(value)
        except (TypeError, ValueError):
            return value
    return value


def load_config(config_file: str | Path | None = None) -> Config:
    cfg = Config()
    path = Path(config_file).resolve() if config_file else (BASE_DIR / "config.toml")
    cfg.config_path = path

    raw: dict[str, Any] = {}
    if path.exists():
        try:
            with open(path, "rb") as f:
                raw = tomllib.load(f)
        except Exception as exc:
            print(f"[config] 解析 {path} 失败，使用默认配置: {exc}")

    for section, values in raw.items():
        obj = getattr(cfg, section, None)
        if obj is None or not isinstance(values, dict):
            continue
        for key, value in values.items():
            if hasattr(obj, key):
                setattr(obj, key, _coerce(section, key, value))

    for env_key, env_val in os.environ.items():
        if not env_key.startswith("EDGE_") or "__" not in env_key:
            continue
        section, _, key = env_key[4:].partition("__")
        section, key = section.lower(), key.lower()
        obj = getattr(cfg, section, None)
        if obj is not None and hasattr(obj, key):
            setattr(obj, section, getattr(obj, section))  # noqa: PLW0127 (保持可读)
            setattr(obj, key, _coerce(section, key, env_val))

    return cfg


def default_node_id(name: str = "") -> str:
    """生成稳定可读的 node_id：形如 phone-a1b2c3d4。"""
    suffix = uuid.uuid4().hex[:8]
    if name:
        slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")[:16]
        if slug:
            return f"{slug}-{suffix}"
    return f"phone-{suffix}"


def state_path(node_id: str) -> Path:
    """每个节点实例自己的状态文件路径（存放 node_id / name）。

    为什么不用共享的 config.toml：同一台 PC 上要跑多个模拟节点（phone-01、
    phone-02），如果把 node_id 回写到同一个 config.toml，第二个实例会读到
    第一个实例生成的 node_id，两个节点就会互相顶掉连接。
    因此每个实例把自己的身份写进 state/<node_id>.json 或 state/by-<name>.json，
    并在启动时优先按 --name 找回自己的身份。
    """
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", node_id) or "default"
    return BASE_DIR / "state" / f"{safe}.json"


def load_device_token(node_id: str) -> str:
    """读取平台上次签发的设备 token。没有 / 读不到一律返回空串。"""
    if not node_id:
        return ""
    try:
        data = json.loads(state_path(f"token-{node_id}").read_text(encoding="utf-8"))
        tok = data.get("device_token") if isinstance(data, dict) else None
        return str(tok) if tok else ""
    except Exception:
        return ""


def save_device_token(node_id: str, token: str) -> None:
    """持久化设备 token。

    **单独一个文件**（token-<node_id>.json），不并进 by-name / by-id 的实例状态：
    那两个文件是**身份**（我是谁），这个是**凭据**（我凭什么进门）。
    混在一起的话，一次身份回写就可能顺手把凭据抹掉，设备下次握手直接失败。

    写失败不影响运行 —— 共享 token 仍是有效退路，这里绝不因为存不下凭据
    就让 Agent 起不来。
    """
    if not (node_id and token):
        return
    try:
        p = state_path(f"token-{node_id}")
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(
            json.dumps({"node_id": node_id, "device_token": token}, indent=2),
            encoding="utf-8",
        )
        try:
            os.chmod(p, 0o600)  # 凭据文件不该对同机其他用户可读
        except Exception:
            pass  # Windows 上 chmod 语义有限，忽略
    except Exception as exc:
        print(f"[config] 保存设备 token 失败（不影响运行）: {exc}")


def load_instance_state(name: str) -> dict[str, Any]:
    """按名称读取实例状态（用于恢复上次的 node_id）。"""
    if not name:
        return {}
    p = state_path(f"by-name-{name}")
    try:
        if p.exists():
            data = json.loads(p.read_text(encoding="utf-8"))
            if isinstance(data, dict) and data.get("node_id"):
                return data
    except Exception:
        pass
    return {}


def save_instance_state(name: str, node_id: str, display_name: str) -> None:
    """持久化本实例身份，保证重启后 node_id 稳定（不会产生重复节点记录）。"""
    try:
        # 按名称索引（启动时按 --name 找回）
        p1 = state_path(f"by-name-{name or node_id}")
        p1.parent.mkdir(parents=True, exist_ok=True)
        p1.write_text(
            json.dumps({"name": display_name, "node_id": node_id}, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        # 按 node_id 索引（便于识别身份占用）
        p2 = state_path(f"by-id-{node_id}")
        p2.write_text(
            json.dumps({"name": display_name, "node_id": node_id}, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
    except Exception as exc:
        print(f"[config] 保存实例身份失败（不影响运行）: {exc}")


def update_config_node_id(cfg: Config, node_id: str, name: str) -> None:
    """把 node_id 回写到 config.toml —— 仅当配置里 node_id 为空、且用户没指定
    --name（即单实例默认场景）时才做。多实例场景由 save_instance_state 负责。

    用文本替换而不是重新 dump，避免破坏用户的注释与排版。
    """
    path = cfg.config_path
    try:
        if not path.exists():
            return
        text = path.read_text(encoding="utf-8")
        text = re.sub(r'(?m)^(\s*)node_id\s*=\s*".*?"', rf'\1node_id = "{node_id}"', text, count=1)
        if name:
            text = re.sub(r'(?m)^(\s*)name\s*=\s*".*?"', rf'\1name = "{name}"', text, count=1)
        path.write_text(text, encoding="utf-8")
    except Exception as exc:
        print(f"[config] 回写 node_id 失败（不影响运行）: {exc}")


def local_ip() -> str:
    """获取本机在局域网中的 IP。"""
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout(0.3)
        try:
            s.connect(("8.8.8.8", 80))
            return s.getsockname()[0]
        finally:
            s.close()
    except Exception:
        try:
            return socket.gethostbyname(socket.gethostname())
        except Exception:
            return "127.0.0.1"


# --------------------------------------------------------------- 托管服务状态
# 需求 §7：「设备重启后，设置了 Auto Start 的模型 Runtime 自动恢复」。
# 恢复的前提是"重启前是什么样"得先活下来 —— 进程句柄随进程消失，
# 但"该跑什么、该不该自动起来"必须落盘，否则 Agent 一重启就全忘了。


def services_path(node_id: str) -> Path:
    """托管服务状态文件。与身份 / 凭据分开存，理由同上：三件事互不牵连。"""
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", node_id) or "default"
    return BASE_DIR / "state" / f"services-{safe}.json"


def load_services(node_id: str) -> dict[str, dict[str, Any]]:
    """读回托管服务记录。读不到 / 坏了都返回空 dict（当作"本来就没跑过"）。"""
    if not node_id:
        return {}
    try:
        data = json.loads(services_path(node_id).read_text(encoding="utf-8"))
    except Exception:
        return {}
    items = data.get("services") if isinstance(data, dict) else None
    if not isinstance(items, dict):
        return {}
    out: dict[str, dict[str, Any]] = {}
    for sid, rec in items.items():
        if isinstance(rec, dict) and rec.get("command"):
            out[str(sid)] = rec
    return out


def save_services(node_id: str, records: dict[str, dict[str, Any]]) -> None:
    """落盘托管服务记录。

    写 command / args / auto_restart / auto_start / restart_count，外加 pid。
    pid **只用于清理上一次遗留的子进程**（Agent 被强杀时子进程会活下来），
    不能用来判断服务"是否在运行" —— 那必须靠进程句柄，句柄是跨不了重启的。
    """
    if not node_id:
        return
    safe_records = {
        str(sid): {
            "command": str(rec.get("command") or ""),
            "args": [str(x) for x in (rec.get("args") or [])],
            "auto_restart": bool(rec.get("auto_restart")),
            "auto_start": bool(rec.get("auto_start")),
            "restart_count": int(rec.get("restart_count") or 0),
            # pid 只用于**清理上一次留下的进程**，绝不用于判断"它是不是还活着"。
            # 必须记：服务子进程跑在独立会话里，Agent 被强杀时它们不会跟着死，
            # 下次恢复直接再拉一遍就会撞端口（新进程绑定失败立刻退出，
            # 表现成"服务恢复不了/反复重启"，而真凶是一个没人认领的旧进程）。
            "pid": int(rec["proc"].pid) if rec.get("proc") is not None
                   and getattr(rec["proc"], "pid", None) else 0,
        }
        for sid, rec in records.items()
        if rec.get("command")
    }
    try:
        p = services_path(node_id)
        p.parent.mkdir(parents=True, exist_ok=True)
        tmp = p.with_suffix(".tmp")
        tmp.write_text(
            json.dumps({"node_id": node_id, "services": safe_records}, indent=2),
            encoding="utf-8",
        )
        tmp.replace(p)  # 原子替换：写一半掉电不会留下半个 JSON
    except Exception as exc:
        print(f"[config] 保存服务状态失败（不影响运行）: {exc}")
