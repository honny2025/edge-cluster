"""平台启动入口。

用法：
    python run.py                  # 使用 server/config.toml
    python run.py --config my.toml # 指定配置文件
    python run.py --port 9000      # 临时改端口

注意：配置文件必须在导入 app 之前加载（settings 是模块级单例）。
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

BASE_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(BASE_DIR))


def main() -> None:
    parser = argparse.ArgumentParser(description="手机边缘计算集群管理平台 - 服务端")
    parser.add_argument("--config", default=None, help="配置文件路径（默认 server/config.toml）")
    parser.add_argument("--host", default=None, help="覆盖监听地址")
    parser.add_argument("--port", type=int, default=None, help="覆盖监听端口")
    parser.add_argument("--log-level", default=None, help="覆盖日志级别")
    parser.add_argument("--reload", action="store_true", help="开发模式：代码热重载")
    args = parser.parse_args()

    # 配置必须在 app 导入前生效
    if args.config:
        os.environ["EDGE_CONFIG_FILE"] = str(Path(args.config).resolve())
    if args.host:
        os.environ["EDGE_SERVER__HOST"] = args.host
    if args.port:
        os.environ["EDGE_SERVER__PORT"] = str(args.port)
    if args.log_level:
        os.environ["EDGE_SERVER__LOG_LEVEL"] = args.log_level

    import uvicorn

    from app.config import settings
    from app.core.logging_conf import setup_logging

    setup_logging()
    print(f"\n  平台地址: http://{settings.server.host}:{settings.server.port}")
    print(f"  接口文档: http://127.0.0.1:{settings.server.port}/docs")
    print(f"  共享token: {settings.server.token}")
    print(f"  发现端口: {settings.server.discovery_port} (UDP)")
    print(f"  数据目录: {settings.data.db_path}\n")

    uvicorn.run(
        "app.main:app",
        host=settings.server.host,
        port=settings.server.port,
        log_level=str(settings.server.log_level).lower(),
        reload=args.reload,
        ws_ping_interval=20,
        ws_ping_timeout=20,
    )


if __name__ == "__main__":
    main()
