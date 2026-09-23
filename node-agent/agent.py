#!/usr/bin/env python3
"""手机边缘计算集群 —— 节点 Agent 主程序。

同一份代码同时支持：
  * 真机（Android / Termux）：采集真实指标、以子进程托管模型服务；
  * 模拟器（PC，`--simulate`）：生成合理假数据、可模拟崩溃/断线/下载中断。

真机不在开发机上，因此**所有联调与端到端验收默认用模拟器完成**。

用法：
    python agent.py                                  # 真机运行
    python agent.py --simulate --name phone-01       # 模拟节点
    python agent.py --simulate --name phone-02 --node-id phone-02
    python agent.py --config /path/to/config.toml

环境变量覆盖：EDGE_AGENT__TOKEN=xxx / EDGE_AGENT__PLATFORM_URL=http://...
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import random
import signal
import subprocess
import hashlib
import urllib.request
import sys
import time
from pathlib import Path
from typing import Any

BASE_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(BASE_DIR))

from agent_config import (  # noqa: E402
    AGENT_VERSION,
    Config,
    default_node_id,
    load_config,
    load_instance_state,
    local_ip,
    save_instance_state,
    update_config_node_id,
)
from agent_discovery import discover_platform  # noqa: E402
from agent_metrics import MetricsCollector, device_info  # noqa: E402
from agent_utils import get_logger, human_bytes, setup_logging  # noqa: E402

log = get_logger("agent.main")


class EdgeAgent:
    """节点 Agent：注册 + 心跳 + 指令处理（M1 阶段只做前两项）。"""

    def __init__(self, cfg: Config) -> None:
        self.cfg = cfg
        self.simulate = bool(cfg.simulate.enabled)
        self.node_id = cfg.agent.node_id
        self.name = cfg.agent.name or cfg.agent.node_id
        self.ip = local_ip()
        self.device = device_info(self.simulate, cfg.simulate)
        self.collector = MetricsCollector(simulate=self.simulate)
        self.platform_url: str | None = cfg.agent.platform_url or None

        self._ws: Any = None
        self._stop = asyncio.Event()
        self._session_started = time.time()
        # 指令幂等去重（M5）：记录已处理过的 cmd_id
        self._handled_cmds: dict[str, float] = {}
        # 模拟用：本轮是否已经模拟过下载失败
        self._sim_failed_once = False
        self._services: dict[str, subprocess.Popen] = {}

        cfg.ensure_dirs()

    # ================================================================== 生命周期

    async def run(self) -> None:
        attempt = 0
        while not self._stop.is_set():
            try:
                ok = await self._connect_and_serve()
                if ok:
                    attempt = 0  # 正常断开（如平台重启），退避重置
                else:
                    attempt += 1
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                attempt += 1
                log.error("连接循环异常（将继续重试）err=%s", exc, exc_info=True)

            if self._stop.is_set():
                break

            delay = self._backoff_delay(attempt)
            log.warning("将在 %.1fs 后重连（第 %d 次重试）", delay, attempt)
            try:
                await asyncio.wait_for(self._stop.wait(), timeout=delay)
            except asyncio.TimeoutError:
                pass

    def _backoff_delay(self, attempt: int) -> float:
        """指数退避 + 抖动：base*2^n 封顶 max_delay，再乘 (1±jitter)。"""
        base = float(self.cfg.reconnect.base_delay)
        cap = float(self.cfg.reconnect.max_delay)
        jitter = float(self.cfg.reconnect.jitter)
        raw = min(cap, base * (2 ** max(0, attempt - 1)))
        factor = 1.0 + random.uniform(-jitter, jitter)
        return max(0.2, raw * factor)

    async def _connect_and_serve(self) -> bool:
        import websockets

        # 1) 定位平台
        if not self.platform_url:
            found = await asyncio.to_thread(
                discover_platform, self.cfg, self.cfg.agent.token, AGENT_VERSION, self.name
            )
            if not found:
                return False
            self.platform_url = found

        url = self.cfg.ws_url(self.platform_url)
        log.info("正在连接平台 %s（node_id=%s）", url, self.node_id)

        try:
            # 局域网直连场景下不使用系统代理：很多环境（公司/校园网）会设置
            # HTTP_PROXY，导致连 127.0.0.1 或内网地址时被代理拒绝（HTTP 502）。
            async with websockets.connect(
                f"{url}?token={self.cfg.agent.token}",
                ping_interval=20,
                ping_timeout=20,
                max_size=16 * 1024 * 1024,
                open_timeout=10,
                proxy=None,
            ) as ws:
                self._ws = ws
                self._session_started = time.time()
                await self._register(ws)

                hb_task = asyncio.create_task(self._heartbeat_loop(ws), name="heartbeat")
                recv_task = asyncio.create_task(self._recv_loop(ws), name="recv")
                sim_task = asyncio.create_task(self._simulate_loop(), name="sim")

                try:
                    done, pending = await asyncio.wait(
                        {hb_task, recv_task, sim_task}, return_when=asyncio.FIRST_COMPLETED
                    )
                    for task in pending:
                        task.cancel()
                    # 把异常抛出来，交给外层重连逻辑处理
                    for task in done:
                        exc = task.exception()
                        if exc:
                            raise exc
                finally:
                    for task in (hb_task, recv_task, sim_task):
                        if not task.done():
                            task.cancel()
                    self._ws = None
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            log.warning("连接中断: %s", exc)
            # 平台地址可能变了（平台换 IP / 重启换端口），重新走发现流程
            if not self.cfg.agent.platform_url:
                self.platform_url = None
            return False

        log.info("与平台的连接已正常结束")
        return True

    async def stop(self) -> None:
        self._stop.set()
        if self._ws is not None:
            try:
                await self._ws.close()
            except Exception:
                pass

    # ================================================================== 注册

    async def _register(self, ws: Any) -> None:
        info = {
            "node_id": self.node_id,
            "name": self.name,
            "model": self.device.get("model", ""),
            "soc": self.device.get("soc", ""),
            "android_version": self.device.get("android_version", ""),
            "agent_version": AGENT_VERSION,
            "ip": self.ip,
            "simulate": self.simulate,
            "pid": os.getpid(),
        }
        # 顶层字段同时放一份，兼容平台侧「顶层 + payload 合并」的解析方式
        payload = {
            "type": "register",
            **info,
            "payload": {**info, "heartbeat_interval": self.cfg.agent.heartbeat_interval},
        }
        await ws.send(json.dumps(payload))
        log.info(
            "已发送注册信息 node_id=%s name=%s model=%s soc=%s android=%s ip=%s",
            self.node_id, self.name, info["model"], info["soc"],
            info["android_version"], self.ip,
        )

    # ================================================================== 心跳

    async def _heartbeat_loop(self, ws: Any) -> None:
        interval = max(1, int(self.cfg.agent.heartbeat_interval))
        while True:
            t0 = time.time()
            try:
                metrics = await asyncio.to_thread(self.collector.collect)
                # 注意：指标必须放在 payload.metrics 下，与平台端
                # NodeRegistry.heartbeat(payload) 的取值路径一致。
                msg = {
                    "type": "heartbeat",
                    "node_id": self.node_id,
                    "ts": time.time(),
                    "payload": {
                        "node_id": self.node_id,
                        "ip": self.ip,
                        "name": self.name,
                        "uptime_seconds": int(time.time() - self._session_started),
                        "metrics": metrics,
                    },
                }
                await ws.send(json.dumps(msg))
                log.debug(
                    "心跳已发送 cpu=%s%% mem=%s/%s temp=%s℃ 耗时=%.0fms",
                    metrics.get("cpu_usage"),
                    human_bytes(metrics.get("mem_used")),
                    human_bytes(metrics.get("mem_total")),
                    metrics.get("temperature"),
                    (time.time() - t0) * 1000,
                )
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                log.warning("心跳发送失败（将重连）err=%s", exc)
                return

            elapsed = time.time() - t0
            await asyncio.sleep(max(0.2, interval - elapsed))

    # ================================================================== 收消息

    async def _recv_loop(self, ws: Any) -> None:
        while True:
            raw = await ws.recv()
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                log.warning("收到非法 JSON，已忽略")
                continue
            if not isinstance(msg, dict):
                continue
            try:
                await self._handle_message(ws, msg)
            except Exception as exc:
                # 单条指令失败不能断连
                log.error("指令处理失败 type=%s err=%s", msg.get("type"), exc, exc_info=True)
                cmd_id = msg.get("cmd_id")
                if cmd_id:
                    await self._reply(ws, cmd_id, ok=False, error=str(exc))

    async def _handle_message(self, ws: Any, msg: dict[str, Any]) -> None:
        mtype = str(msg.get("type") or "")
        cmd_id = str(msg.get("cmd_id") or "")

        # --- 幂等：同一条指令重复下发直接回 ack，不做第二次动作 ---
        if cmd_id:
            if cmd_id in self._handled_cmds:
                log.info("收到重复指令（已忽略副作用）type=%s cmd_id=%s", mtype, cmd_id)
                await self._reply(ws, cmd_id, ok=True, extra={"duplicate": True})
                return
            self._handled_cmds[cmd_id] = time.time()
            if len(self._handled_cmds) > 512:  # 简单的 LRU 上限保护
                for k in sorted(self._handled_cmds, key=self._handled_cmds.get)[:256]:
                    self._handled_cmds.pop(k, None)

        if mtype in ("register_ack",):
            log.info("平台确认注册成功 node_id=%s server_version=%s",
                     msg.get("node_id"), msg.get("server_version"))
            return

        if mtype == "heartbeat_ack":
            return

        if mtype == "ping":
            await ws.send(json.dumps({"type": "pong", "ts": time.time()}))
            return

        if mtype == "collect_metrics":
            metrics = await asyncio.to_thread(self.collector.collect)
            await self._reply(ws, cmd_id, ok=True, extra={"metrics": metrics})
            return

        if mtype == "simulate":
            await self._handle_simulate(ws, cmd_id, msg.get("payload") or {})
            return

        if mtype == "deploy_model":
            await self._deploy_model(ws, cmd_id, msg.get("payload") or {})
            return

        if mtype == "start_service":
            await self._service_command(ws, cmd_id, msg.get("payload") or {}, start=True)
            return
        if mtype in ("stop_service", "restart_service"):
            await self._service_command(ws, cmd_id, msg.get("payload") or {}, start=mtype == "restart_service")
            return

        # M3 / M4 的指令在此处扩展（deploy_model / start_service ...）
        log.warning("收到暂不支持的指令 type=%s", mtype)
        if cmd_id:
            await self._reply(ws, cmd_id, ok=False, error=f"不支持的指令类型: {mtype}")

    async def _service_command(self, ws: Any, cmd_id: str, payload: dict[str, Any], start: bool) -> None:
        sid = str(payload.get("service_id") or "")
        if not sid:
            await self._reply(ws, cmd_id, False, "缺少 service_id"); return
        proc = self._services.get(sid)
        if proc and proc.poll() is None:
            proc.terminate(); proc.wait(timeout=3)
        self._services.pop(sid, None)
        if start:
            command = str(payload.get("command") or "")
            args = payload.get("args") or []
            if not command:
                await self._reply(ws, cmd_id, False, "缺少 command"); return
            proc = subprocess.Popen([command, *[str(x) for x in args]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            self._services[sid] = proc
            await self._reply(ws, cmd_id, True, extra={"service_id": sid, "pid": proc.pid, "status": "starting"})
        else:
            await self._reply(ws, cmd_id, True, extra={"service_id": sid, "status": "stopped"})

    async def _deploy_model(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        url = str(payload.get("url") or ""); target = Path(str(payload.get("target") or "model.bin"))
        expected = str(payload.get("sha256") or "").lower()
        if not url: await self._reply(ws, cmd_id, False, "缺少下载地址"); return
        target.parent.mkdir(parents=True, exist_ok=True); temp = target.with_suffix(target.suffix + ".part")
        try:
            def download() -> tuple[int, str]:
                digest=hashlib.sha256(); total=0
                offset = temp.stat().st_size if temp.exists() else 0
                req = urllib.request.Request(url, headers={"Range": f"bytes={offset}-"} if offset else {})
                with urllib.request.urlopen(req, timeout=60) as src, temp.open("ab" if offset else "wb") as dst:
                    if offset:
                        with temp.open("rb") as old:
                            while chunk:=old.read(1024*1024): digest.update(chunk)
                        total = offset
                    while chunk:=src.read(1024*1024): dst.write(chunk); digest.update(chunk); total+=len(chunk)
                return total,digest.hexdigest()
            total, digest = await asyncio.to_thread(download)
            if expected and digest != expected: temp.unlink(missing_ok=True); raise RuntimeError("SHA-256 校验失败")
            temp.replace(target)
            await self._reply(ws, cmd_id, True, extra={"status":"ready","bytes_downloaded":total,"sha256":digest,"target":str(target),"task_id":payload.get("task_id")})
        except Exception as exc:
            temp.unlink(missing_ok=True); await self._reply(ws, cmd_id, False, str(exc), extra={"status":"failed","task_id":payload.get("task_id")})

    async def _reply(self, ws: Any, cmd_id: str, ok: bool, error: str = "",
                     extra: dict[str, Any] | None = None) -> None:
        payload: dict[str, Any] = {"type": "ack", "cmd_id": cmd_id, "ok": ok}
        if error:
            payload["error"] = error
        if extra:
            payload.update(extra)
        try:
            await ws.send(json.dumps(payload))
        except Exception:
            pass

    # ================================================================== 模拟能力

    async def _handle_simulate(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        """模拟器专用：注入崩溃 / 断线 / 慢下载等故障，用于验收。"""
        action = str(payload.get("action") or "")
        log.warning("收到模拟指令 action=%s", action)

        if action == "disconnect":
            await self._reply(ws, cmd_id, ok=True, extra={"action": action})
            await asyncio.sleep(0.2)
            log.warning("模拟断线：主动关闭 WebSocket")
            try:
                await ws.close()
            except Exception:
                pass
            return

        if action == "crash":
            log.warning("模拟进程崩溃：3 秒后退出进程（由外部脚本重启）")
            await self._reply(ws, cmd_id, ok=True, extra={"action": action})
            await asyncio.sleep(3)
            os._exit(9)
            return

        await self._reply(ws, cmd_id, ok=False, error=f"未知模拟动作: {action}")

    async def _simulate_loop(self) -> None:
        """按配置在运行一段时间后自动模拟断线（M1 验收辅助）。"""
        seconds = int(self.cfg.simulate.disconnect_after or 0)
        if not self.simulate or seconds <= 0:
            await asyncio.Event().wait()  # 永久挂起
            return
        await asyncio.sleep(seconds)
        log.warning("触发配置的模拟断线（disconnect_after=%ss）", seconds)
        if self._ws is not None:
            try:
                await self._ws.close()
            except Exception:
                pass


# ====================================================================== CLI


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="手机边缘计算集群 - 节点 Agent",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument("--config", default=None, help="配置文件路径（默认 node-agent/config.toml）")
    p.add_argument("--simulate", action="store_true",
                   help="模拟节点模式：生成合理假数据，可在 PC 上冒充手机")
    p.add_argument("--name", default=None, help="节点显示名（如 phone-01）")
    p.add_argument("--node-id", default=None, help="指定 node_id（默认自动生成并回写配置）")
    p.add_argument("--platform-url", default=None,
                   help="直接指定平台地址（跳过 UDP 发现），如 http://192.168.1.100:8000")
    p.add_argument("--token", default=None, help="平台共享 token")
    p.add_argument("--heartbeat-interval", type=int, default=None, help="心跳间隔（秒）")
    p.add_argument("--log-level", default=None, help="日志级别 DEBUG/INFO/WARNING/ERROR")
    p.add_argument("--log-tag", default=None, help="日志文件名前缀（多实例并存时区分）")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    cfg = load_config(args.config)

    # 命令行覆盖
    if args.simulate:
        cfg.simulate.enabled = True
    if args.token:
        cfg.agent.token = args.token
    if args.platform_url:
        cfg.agent.platform_url = args.platform_url
    if args.heartbeat_interval:
        cfg.agent.heartbeat_interval = args.heartbeat_interval
    if args.name:
        cfg.agent.name = args.name

    # node_id：命令行 > 实例状态文件（按 --name 找回）> 配置 > 自动生成
    requested_name = args.name or cfg.agent.name
    node_id = args.node_id or ""
    if not node_id and requested_name:
        # 同一台 PC 跑多个模拟节点时，按 name 找回自己的身份，避免互相顶掉连接
        node_id = str(load_instance_state(requested_name).get("node_id") or "")
    if not node_id:
        node_id = cfg.agent.node_id
    auto_generated = False
    if not node_id:
        node_id = default_node_id(requested_name)
        auto_generated = True
    cfg.agent.node_id = node_id
    if not cfg.agent.name:
        cfg.agent.name = node_id

    # 身份持久化：
    #  - 单实例默认场景（未指定 --name 且配置里 node_id 为空）：回写 config.toml，最直观；
    #  - 指定了 --name（多实例场景）：写独立 state 文件，绝不污染共享配置。
    save_instance_state(cfg.agent.name, node_id, cfg.agent.name)
    if auto_generated and not args.name and not args.config:
        update_config_node_id(cfg, node_id, cfg.agent.name)
        log_hint = True
    else:
        log_hint = False

    log_dir = cfg.logs_path
    tag = args.log_tag or (cfg.agent.name or node_id).replace("/", "_")
    log_file = setup_logging(
        log_dir,
        level=args.log_level or "INFO",
        max_bytes=cfg.storage.log_max_bytes,
        backup_count=cfg.storage.log_backup_count,
        tag=tag,
    )

    mode = "模拟节点" if cfg.simulate.enabled else "真机节点"
    log.info("=" * 70)
    log.info("节点 Agent 启动 mode=%s version=%s", mode, AGENT_VERSION)
    if log_hint:
        log.info("已自动生成 node_id=%s 并回写配置（保证重启后身份稳定）", node_id)
    log.info("node_id=%s name=%s 平台=%s 心跳=%ss",
             node_id, cfg.agent.name, cfg.agent.platform_url or "(UDP 自动发现)",
             cfg.agent.heartbeat_interval)
    log.info("本地 IP=%s 日志=%s 模型目录=%s",
             local_ip(), log_file, cfg.models_path)
    log.info("=" * 70)

    agent = EdgeAgent(cfg)

    async def _main() -> None:
        loop = asyncio.get_running_loop()
        stop_event = asyncio.Event()

        def _signal_handler() -> None:
            log.info("收到退出信号，正在停止 Agent...")
            stop_event.set()

        for sig in (signal.SIGINT, signal.SIGTERM):
            try:
                loop.add_signal_handler(sig, _signal_handler)
            except (NotImplementedError, ValueError):
                # Windows / 部分 Termux 环境不支持 add_signal_handler
                pass

        runner = asyncio.create_task(agent.run(), name="agent-run")
        stopper = asyncio.create_task(stop_event.wait(), name="agent-stop")

        done, pending = await asyncio.wait(
            {runner, stopper}, return_when=asyncio.FIRST_COMPLETED
        )
        await agent.stop()
        for task in pending:
            task.cancel()
        for task in done:
            exc = task.exception()
            if exc and not isinstance(exc, asyncio.CancelledError):
                log.error("Agent 主循环退出异常: %s", exc, exc_info=True)

    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        pass

    log.info("节点 Agent 已退出 node_id=%s", node_id)
    return 0


if __name__ == "__main__":
    sys.exit(main())
