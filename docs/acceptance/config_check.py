#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""配置层自检：环境变量覆盖是否真的生效。

为什么单独有这么一个脚本
------------------------
`server/app/config.py` 的环境变量覆盖曾因**前缀截断写错**而静默失效：
    section, _, key = env_key[4:].partition("__")      # ← 错：少截一个字符
`"EDGE_"` 是 5 个字符，截 4 个会留下前导下划线，partition 出来的 section
变成 `"_server"`，`getattr(cfg, "_server")` 恒为 None，于是**整段覆盖逻辑
一条都不生效，且不报任何错**。

后果是 `run.py --port / --host / --log-level`（它们都靠写 EDGE_* 环境变量实现）
全部静默无效果，而 config.toml 里却写着"所有可调项都能被环境变量覆盖"。

这个脚本把这条路径钉死，防止再次回归。它**不需要启动平台**，秒级完成。

用法：
    python docs/acceptance/config_check.py
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]   # edge-cluster/
SERVER = ROOT / "server"

# 探针必须跑在子进程里：app.config 是模块级单例，同进程内改不了
PROBE = r"""
import json, sys
sys.path.insert(0, sys.argv[1])
import app.config as c
print(json.dumps({
    "port":         c.settings.server.port,
    "host":         c.settings.server.host,
    "token":        c.settings.server.token,
    "log_level":    c.settings.server.log_level,
    "serve_static": c.settings.web.serve_static,
    "cpu_warn":     c.settings.alerts.cpu_usage_warn,
}))
"""

# (说明, 注入的环境变量, 期望至少命中的键值)
CASES = [
    ("不加覆盖时读 config.toml", {}, {"port": 8000, "host": "0.0.0.0"}),
    ("EDGE_SERVER__PORT 覆盖端口", {"EDGE_SERVER__PORT": "9000"}, {"port": 9000}),
    ("EDGE_SERVER__HOST 覆盖地址", {"EDGE_SERVER__HOST": "127.0.0.1"}, {"host": "127.0.0.1"}),
    ("EDGE_SERVER__TOKEN 覆盖 token", {"EDGE_SERVER__TOKEN": "zzz-test"}, {"token": "zzz-test"}),
    ("EDGE_SERVER__LOG_LEVEL 覆盖日志级别",
     {"EDGE_SERVER__LOG_LEVEL": "DEBUG"}, {"log_level": "DEBUG"}),
    ("EDGE_WEB__SERVE_STATIC=false 转 bool",
     {"EDGE_WEB__SERVE_STATIC": "false"}, {"serve_static": False}),
    ("EDGE_ALERTS__CPU_USAGE_WARN=55.5 转 float",
     {"EDGE_ALERTS__CPU_USAGE_WARN": "55.5"}, {"cpu_warn": 55.5}),
    ("多个变量同时生效",
     {"EDGE_SERVER__PORT": "9100", "EDGE_SERVER__HOST": "1.2.3.4"},
     {"port": 9100, "host": "1.2.3.4"}),
]


def run_case(extra_env: dict[str, str]) -> dict:
    env = {k: v for k, v in os.environ.items()
           if not (k.upper().startswith("EDGE_") and "__" in k.upper())}
    env.update(extra_env)
    env["PYTHONIOENCODING"] = "utf-8"
    r = subprocess.run(
        [sys.executable, "-c", PROBE, str(SERVER)],
        env=env, capture_output=True, text=True, encoding="utf-8", timeout=60,
    )
    if r.returncode != 0:
        raise RuntimeError(f"探针执行失败 (exit {r.returncode}): {(r.stderr or '').strip()[-500:]}")
    return json.loads(r.stdout.strip().splitlines()[-1])


def main() -> int:
    print(f"配置层自检  ·  {SERVER / 'app' / 'config.py'}\n")
    passed = failed = 0
    for label, extra, expect in CASES:
        try:
            got = run_case(extra)
        except Exception as exc:                      # noqa: BLE001
            print(f"  [FAIL] {label}: {exc}")
            failed += 1
            continue
        bad = {k: (got.get(k), v) for k, v in expect.items() if got.get(k) != v}
        if bad:
            print(f"  [FAIL] {label}")
            for k, (a, b) in bad.items():
                print(f"          {k}: 实际={a!r} 期望={b!r}")
            failed += 1
        else:
            print(f"  [PASS] {label}")
            passed += 1

    total = passed + failed
    print(f"\n验收结果：{passed}/{total} 通过")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
