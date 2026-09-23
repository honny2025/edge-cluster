"""配置加载模块。

优先级：环境变量（EDGE_SECTION__KEY） > config.toml > 内置默认值。
所有可调项都有默认值，保证开箱即用。
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field, fields
from pathlib import Path
from typing import Any

try:  # Python 3.11+ 自带 tomllib
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore


# 后端目录（server/）
BASE_DIR = Path(__file__).resolve().parent.parent


def _as_bool(v: Any) -> bool:
    if isinstance(v, bool):
        return v
    return str(v).strip().lower() in {"1", "true", "yes", "on"}


@dataclass
class ServerConfig:
    host: str = "0.0.0.0"
    port: int = 8000
    token: str = "edge-cluster-dev-token"
    discovery_port: int = 37020
    offline_threshold: int = 15
    offline_confirm_times: int = 3
    heartbeat_interval: int = 5
    service_probe_interval: int = 15
    metric_retention_days: int = 7
    metric_min_interval: int = 2
    log_level: str = "INFO"


@dataclass
class DataConfig:
    db_path: str = "data/edge_cluster.db"
    models_dir: str = "data/models"
    logs_dir: str = "data/logs"


@dataclass
class WebConfig:
    serve_static: bool = True


@dataclass
class AlertConfig:
    """告警阈值（里程碑 2）。

    全部配置驱动，禁止在代码里硬编码阈值。
    指标拿不到（None）时一律**不产生告警**——没有数据不等于异常。
    """

    enabled: bool = True
    cpu_usage_warn: float = 85.0       # CPU 使用率 % ≥ 此值告警
    mem_usage_warn: float = 85.0       # 内存使用率 % ≥ 此值告警
    storage_usage_warn: float = 90.0   # 存储使用率 % ≥ 此值告警
    temperature_warn: float = 45.0     # 温度 ℃ ≥ 此值告警
    temperature_critical: float = 55.0 # 温度 ℃ ≥ 此值严重告警
    alarm_on_offline: bool = True      # 节点离线是否告警
    probe_timeout_warn: int = 3        # 服务探活连续失败 N 次视为异常（M4 用）


@dataclass
class Config:
    server: ServerConfig = field(default_factory=ServerConfig)
    data: DataConfig = field(default_factory=DataConfig)
    web: WebConfig = field(default_factory=WebConfig)
    alerts: AlertConfig = field(default_factory=AlertConfig)

    # ---- 便捷属性 ----
    @property
    def db_file(self) -> Path:
        return _resolve(self.data.db_path)

    @property
    def models_path(self) -> Path:
        return _resolve(self.data.models_dir)

    @property
    def logs_path(self) -> Path:
        return _resolve(self.data.logs_dir)

    def ensure_dirs(self) -> None:
        """确保运行期需要的目录都存在。"""
        for p in (self.db_file.parent, self.models_path, self.logs_path):
            p.mkdir(parents=True, exist_ok=True)


def _resolve(p: str) -> Path:
    """相对路径一律相对 server/ 目录解析，避免受启动目录影响。"""
    path = Path(p)
    return path if path.is_absolute() else (BASE_DIR / path)


_SECTION_TYPES: dict[str, dict[str, type]] = {
    "server": {f.name: type(f.default) for f in fields(ServerConfig)},
    "data": {f.name: type(f.default) for f in fields(DataConfig)},
    "web": {f.name: type(f.default) for f in fields(WebConfig)},
    "alerts": {f.name: type(f.default) for f in fields(AlertConfig)},
}


def _coerce(section: str, key: str, value: Any) -> Any:
    """把 toml / 环境变量里的值转成 dataclass 声明的类型。"""
    want = _SECTION_TYPES.get(section, {}).get(key)
    if want is bool:
        return _as_bool(value)
    if want is int:
        try:
            return int(value)
        except (TypeError, ValueError):
            return value
    if want is float:
        try:
            return float(value)
        except (TypeError, ValueError):
            return value
    return value


def load_config(config_file: str | Path | None = None) -> Config:
    """加载配置。config_file 为空时依次看 EDGE_CONFIG_FILE、server/config.toml。"""
    cfg = Config()

    # 配置文件路径优先级：显式参数 > 环境变量 EDGE_CONFIG_FILE > server/config.toml
    if not config_file:
        config_file = os.environ.get("EDGE_CONFIG_FILE") or None
    path = Path(config_file).expanduser().resolve() if config_file else (BASE_DIR / "config.toml")
    raw: dict[str, Any] = {}
    if path.exists():
        try:
            with open(path, "rb") as f:
                raw = tomllib.load(f)
        except Exception as exc:  # 配置坏了不能拖垮进程，退回默认值
            print(f"[config] 解析 {path} 失败，使用默认配置: {exc}")

    for section, values in raw.items():
        obj = getattr(cfg, section, None)
        if obj is None or not isinstance(values, dict):
            continue
        for key, value in values.items():
            if hasattr(obj, key):
                setattr(obj, key, _coerce(section, key, value))

    # 环境变量覆盖：EDGE_SERVER__PORT=9000 / EDGE_WEB__SERVE_STATIC=false
    # 注意：前缀是 "EDGE_"（5 个字符）。这里曾误写成 env_key[4:]，
    # 截出来是 "_SERVER__PORT"，partition("__") 后 section 成了 "_server"，
    # getattr(cfg, "_server") 恒为 None —— 整段覆盖逻辑会**静默失效**，
    # 表现为 run.py --port / --host 不生效。改动这行请务必跑一次
    # `EDGE_SERVER__PORT=9000 python run.py` 确认打印的端口变了。
    prefix_len = len("EDGE_")
    for env_key, env_val in os.environ.items():
        if not env_key.startswith("EDGE_") or "__" not in env_key:
            continue
        section, _, key = env_key[prefix_len:].partition("__")
        section, key = section.lower(), key.lower()
        obj = getattr(cfg, section, None)
        if obj is not None and hasattr(obj, key):
            setattr(obj, key, _coerce(section, key, env_val))

    return cfg


# 全局单例（模块导入即可用）
settings = load_config()
