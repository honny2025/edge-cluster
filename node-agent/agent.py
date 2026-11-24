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
import threading
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
    load_device_token,
    load_instance_state,
    load_services,
    local_ip,
    save_device_token,
    save_instance_state,
    save_services,
    update_config_node_id,
)
from agent_discovery import discover_platform  # noqa: E402
from agent_files import FileError, FileService  # noqa: E402
from agent_metrics import MetricsCollector, device_info  # noqa: E402
from agent_rollback import InstallJournal, RollbackError  # noqa: E402
from agent_utils import get_logger, human_bytes, setup_logging  # noqa: E402

log = get_logger("agent.main")

# 单次传输的 socket 超时（秒）。之前是 600（10 分钟）：慢 Wi-Fi 下传/收几个 GB 的
# 模型时，单个 socket 读写操作一旦被网络卡住超过 10 分钟就会抛超时、中断这次传输。
# 放宽到 1 小时等于"传输时间不受限"，配合断点续传，慢网也能传完。
_XFER_TIMEOUT = 3600


class _UploadBody:
    """上传用的请求体：边读边算 sha256、边记进度。

    为什么不把文件读进内存：模型文件可能几个 GB，读进内存等于赌手机
    不会被 OOM 杀掉 —— 而 Android 的后台进程正是最容易被杀的那一类。

    urllib 只有拿到显式 Content-Length 才会用定长上传，所以长度由调用方
    （`_fs_fetch`）通过请求头给出，而不是在这里偷偷退化成 chunked。
    """

    _CHUNK = 1024 * 1024

    def __init__(self, path: Path, digest: Any, state: dict[str, int]) -> None:
        self._f = open(path, "rb")
        self._digest = digest
        self._state = state

    def read(self, n: int = -1) -> bytes:
        chunk = self._f.read(n if (n and n > 0) else self._CHUNK)
        if chunk:
            self._digest.update(chunk)
            self._state["done"] = int(self._state.get("done") or 0) + len(chunk)
        return chunk

    def close(self) -> None:
        try:
            self._f.close()
        except Exception:
            pass


# 文件类指令（§4）。单独列出而不是散在 _handle_message 里，
# 是为了让"哪些指令属于文件管理"一眼可见 —— 也方便以后加新的 fs_* 。
_FS_COMMANDS = {
    "fs_roots", "fs_list", "fs_stat", "fs_du",
    "fs_mkdir", "fs_delete", "fs_copy", "fs_move",
}


class _DeployCancelled(Exception):
    """用户取消了这次部署。

    单独一个异常类型而不是复用 RuntimeError：取消与失败必须能被区分开，
    它们的平台侧状态不同（cancelled / failed），混用会让"用户主动取消"
    在界面上显示成"部署失败"。
    """


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
        # 平台签发的设备 token（需求 §8）。首轮握手还没有它，用共享 token 换；
        # 拿到后写进 state/token-<node_id>.json，此后每次握手都用它。
        # 这样一台设备的 token 泄露不会波及其它设备。
        self.device_token = load_device_token(self.node_id)
        # 上次退出时托管的服务（需求 §7：设备/Agent 重启后 Auto Start 的 Runtime 自动恢复）
        self._saved_services: dict[str, dict[str, Any]] = load_services(self.node_id)

        self._ws: Any = None
        self._stop = asyncio.Event()
        self._session_started = time.time()
        # 指令幂等去重（M5）：记录已处理过的 cmd_id
        self._handled_cmds: dict[str, float] = {}
        # 被用户取消的部署任务 task_id。只在阶段边界检查（见 _raise_if_cancelled），
        # 上限做个保护：长期运行的 Agent 不该因为不断取消而无限攒字符串。
        self._cancelled_tasks: set[str] = set()
        # 正在后台执行的传输任务（task_id / transfer_id）。
        # 部署/推文件在 _handle_message 里只 create_task 不 await，recv 循环保持空闲，
        # WS 消息队列不会被下载占满 —— 否则 websockets 的 keepalive 会在下载开始
        # ~2 分钟后判定连接死亡，掉线→重连→平台重投→再掉线，传输永远完不成。
        # 这个集合同时挡掉平台重投的重复指令：同一 task 已在跑就只回 ack，不重复起线程。
        self._active_tasks: set[str] = set()
        # 同一目标文件同时只能有一个下载线程写它（per-file 锁）。
        # 网络抖动 → 平台重投 → 新旧两个下载线程并发写同一个 `.part`，
        # 会把文件写坏 / 写翻倍（SHA 永远对不上、永远落不了位）。见 _download_model_file。
        self._dl_locks: dict[str, "threading.Lock"] = {}
        # 模拟用：本轮是否已经模拟过下载失败
        self._sim_failed_once = False
        # service_id -> 进程句柄 + 启停元数据（日志路径、命令行、是否自动拉起）
        self._services: dict[str, dict[str, Any]] = {}
        # 文件管理（§4）。模型/日志目录也注册成根 —— 它们是 Agent 自己管的目录，
        # 用户最常需要在这里放模型、取日志，不该逼他记住绝对路径。
        self.files = FileService(extra_roots=[
            ("models", "模型目录", str(cfg.models_path)),
            ("logs", "日志目录", str(cfg.logs_path)),
        ])

        cfg.ensure_dirs()

    # ---------------------------------------------------------------- 服务辅助

    def _service_log_path(self, sid: str) -> Path:
        """服务日志文件。放在 logs/services/ 下，避免和 Agent 自身日志混在一起。"""
        d = Path(self.cfg.logs_path) / "services"
        d.mkdir(parents=True, exist_ok=True)
        return d / f"{sid}.log"

    def _persist_services(self) -> None:
        """把当前托管服务的启动配置落盘（进程句柄不写，pid 不可跨重启复用）。"""
        save_services(self.node_id, {
            sid: {
                "command": rec.get("command"),
                "args": rec.get("args"),
                "auto_restart": rec.get("auto_restart"),
                "auto_start": rec.get("auto_start"),
                "restart_count": rec.get("restart_count"),
                # proc 只用于取 pid（save_services 会读它），不落盘句柄本身
                "proc": rec.get("proc"),
            }
            for sid, rec in self._services.items()
        })

    @staticmethod
    def _proc_cmdline(pid: int) -> list[str] | None:
        """读某进程的命令行（Linux/Android：/proc/<pid>/cmdline）。

        读不到返回 None —— 那意味着我们**无法确认这个 pid 还是不是当初那个进程**，
        此时绝不能动手杀：pid 会被系统复用，杀错一个不相干的进程，
        比留下一个孤儿严重得多。
        """
        try:
            raw = Path(f"/proc/{int(pid)}/cmdline").read_bytes()
        except Exception:
            return None
        parts = [x.decode("utf-8", errors="replace") for x in raw.split(bytes([0])) if x]
        return parts or None

    @classmethod
    def _kill_leftover(cls, pid: int, command: str, args: list[str]) -> bool:
        """杀掉"上次 Agent 死掉时没带走的"服务子进程。

        为什么需要它：服务用 `start_new_session=True` 起在独立会话里，
        Agent 被强杀（SIGKILL / taskkill /F）时**不会**连带杀掉它们。
        于是恢复时会出现"端口已被占用 → 新进程绑定失败 → 立刻退出"，
        现象是自愈失败，真凶却是一个没人认领的旧进程。

        安全阀：必须比对 /proc/<pid>/cmdline 与记录的命令行一致才动手。
        拿不到 cmdline（非 Linux、权限不足、pid 已消失）一律放弃，
        宁可留着孤儿也不误杀。
        """
        if not pid or pid <= 0:
            return False
        live = cls._proc_cmdline(pid)
        if live is None:
            return False
        want = [str(command), *[str(a) for a in args]]
        # 内核里 argv[0] 可能是解析后的绝对路径，也可能是原样；
        # 比 basename 更稳，且首参之后的参数必须完全一致，避免误伤同程序的其他实例。
        if Path(live[0]).name != Path(want[0]).name or live[1:] != want[1:]:
            return False
        try:
            os.kill(pid, signal.SIGTERM)
            for _ in range(20):  # 最多等 2 秒
                time.sleep(0.1)
                if cls._proc_cmdline(pid) is None:
                    return True
            os.kill(pid, signal.SIGKILL)
            return True
        except Exception as exc:
            log.warning("清理遗留服务进程失败 pid=%s err=%s", pid, exc)
            return False

    def stop_all_services(self) -> int:
        """Agent 正常退出前收掉所有托管子进程。返回处理个数。

        正常退出路径（Ctrl-C / SIGTERM）走这里，就不会留下孤儿；
        被强杀那条路径收不到信号，只能靠下次启动的 `_kill_leftover`。
        """
        n = 0
        for sid, rec in list(self._services.items()):
            proc = rec.get("proc")
            if proc is None or proc.poll() is not None:
                continue
            try:
                proc.terminate()
                proc.wait(timeout=5)
            except Exception:
                try:
                    proc.kill()
                except Exception:
                    pass
            n += 1
        return n

    def restore_services(self) -> int:
        """开机恢复：拉起上次标记了 auto_start 的服务。返回拉起成功的个数。

        在**连上平台之前**就做，两个原因：
        1. 恢复不该等网络。手机刚重启时网络往往还没好，等服务等到天荒地老不是自愈；
        2. 平台侧另有"重连后兜底恢复"，两者靠状态判断去重，谁先谁后都对。

        auto_start 与 auto_restart 是两件事，这里只认 auto_start：
        崩溃自动拉起由 `_service_watchdog` 负责，那个是"进程级"的事件源。
        如果用 auto_restart 决定开机恢复，用户手工停掉的服务会在下次开机诈尸。
        """
        if not self._saved_services:
            return 0
        restored = 0
        for sid, rec in self._saved_services.items():
            if not rec.get("auto_start"):
                continue
            command = str(rec.get("command") or "")
            if not command:
                continue
            args = list(rec.get("args") or [])
            # 先收掉上次没带走的子进程，再拉起新的 —— 顺序反了就会撞端口。
            try:
                leftover = int(rec.get("pid") or 0)
            except (TypeError, ValueError):
                leftover = 0
            if leftover and self._kill_leftover(leftover, command, args):
                log.warning("已清理服务 %s 上次遗留的进程 pid=%s", sid, leftover)
            try:
                proc = self._spawn_service(sid, command, args)
            except Exception as exc:
                log.error("服务 %s 开机恢复失败 err=%s", sid, exc)
                continue
            self._services[sid] = {
                "proc": proc,
                "command": command,
                "args": list(rec.get("args") or []),
                "started_at": time.time(),
                "auto_restart": bool(rec.get("auto_restart", True)),
                "auto_start": True,
                "restart_count": int(rec.get("restart_count") or 0),
                "log_path": str(self._service_log_path(sid)),
                "restored": True,
            }
            restored += 1
            log.warning("服务 %s 已按 Auto Start 恢复 pid=%s", sid, proc.pid)
        if restored:
            self._persist_services()
        return restored

    def _service_states(self) -> list[dict[str, Any]]:
        """当前托管服务的状态快照，随心跳上报给平台。"""
        out: list[dict[str, Any]] = []
        for sid, rec in list(self._services.items()):
            proc: subprocess.Popen | None = rec.get("proc")
            alive = bool(proc is not None and proc.poll() is None)
            out.append({
                "service_id": sid,
                "status": "running" if alive else "stopped",
                "pid": proc.pid if (alive and proc) else None,
                "exit_code": (proc.poll() if proc else None),
                "started_at": rec.get("started_at"),
                "auto_restart": bool(rec.get("auto_restart")),
                "restart_count": int(rec.get("restart_count") or 0),
            })
        return out

    async def _service_watchdog(self, interval: float = 5.0) -> None:
        """崩溃自动拉起：进程没了但登记为「应运行」时，按原命令行重启。

        只负责拉起，不做业务判断；平台侧另有健康探测，两者互补。
        """
        while True:
            try:
                await asyncio.sleep(interval)
                for sid, rec in list(self._services.items()):
                    if not rec.get("auto_restart"):
                        continue
                    proc: subprocess.Popen | None = rec.get("proc")
                    if proc is not None and proc.poll() is None:
                        continue
                    command = rec.get("command")
                    if not command:
                        continue
                    log.warning("服务 %s 已退出（code=%s），自动拉起", sid, proc.poll() if proc else None)
                    try:
                        newp = self._spawn_service(sid, str(command), list(rec.get("args") or []))
                        rec["proc"] = newp
                        rec["started_at"] = time.time()
                        rec["restart_count"] = int(rec.get("restart_count") or 0) + 1
                        log.warning("服务 %s 已重启 pid=%s（第 %s 次）",
                                    sid, newp.pid, rec["restart_count"])
                        self._persist_services()  # 重启次数要活过 Agent 自己的重启
                    except Exception as exc:
                        log.error("服务 %s 自动拉起失败 err=%s", sid, exc)
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                log.error("服务看门狗异常 err=%s", exc, exc_info=True)

    def _spawn_service(self, sid: str, command: str, args: list[str]) -> subprocess.Popen:
        """启动服务进程，stdout/stderr **追加**写入服务日志文件（不再丢到 DEVNULL）。

        注意必须用 append：自动拉起后要保留上一次崩溃前的输出，否则排错时看不到死因。
        """
        log_path = self._service_log_path(sid)
        fh = open(log_path, "ab", buffering=0)
        return subprocess.Popen(
            [command, *[str(x) for x in args]],
            stdout=fh,
            stderr=subprocess.STDOUT,
            start_new_session=True,  # 独立进程组，Termux 里更稳
        )

    # ================================================================== 生命周期

    async def run(self) -> None:
        """主循环：先恢复托管服务，再进入"连接—服务—重连"。"""
        # 恢复放在连平台之前 —— 手机刚重启时网络常常还没好，
        # 把自己该跑的服务等在网络后面，就不是"自愈"而是"看运气"。
        try:
            n = self.restore_services()
            if n:
                log.warning("Auto Start 恢复完成：%d 个服务已拉起", n)
        except Exception as exc:
            log.error("Auto Start 恢复异常（不影响连接）err=%s", exc, exc_info=True)

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

    def _ws_token(self) -> str:
        """握手用哪个 token。

        **有设备 token 就优先用它**，没有才退回共享 token。
        理由：设备 token 是"我是这一台"，共享 token 是"我是管理员"。
        让设备长期持有管理员凭据，等于任何一台被拿下的手机都能接管整个集群。
        """
        return self.device_token or self.cfg.agent.token

    async def _connect_and_serve(self) -> bool:
        import websockets

        # 1) 定位平台
        if not self.platform_url:
            # 强制走一次 UDP 广播（force=True）：即使配置里填过 platform_url，
            # 那也只是「上次已知地址」，平台 IP 变了就得靠重新发现拿到新地址。
            found = await asyncio.to_thread(
                discover_platform, self.cfg, self.cfg.agent.token, AGENT_VERSION, self.name, True
            )
            if found:
                self.platform_url = found
            elif self.cfg.agent.platform_url:
                # 广播发现不到（如路由器隔离了 UDP 广播），退回配置里的手工地址兜底
                self.platform_url = self.cfg.agent.platform_url.rstrip("/")
            else:
                return False

        url = self.cfg.ws_url(self.platform_url)
        log.info("正在连接平台 %s（node_id=%s）", url, self.node_id)

        try:
            # 局域网直连场景下不使用系统代理：很多环境（公司/校园网）会设置
            # HTTP_PROXY，导致连 127.0.0.1 或内网地址时被代理拒绝（HTTP 502）。
            async with websockets.connect(
                f"{url}?token={self._ws_token()}",
                ping_interval=20,
                ping_timeout=30,
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
                wd_task = asyncio.create_task(self._service_watchdog(), name="watchdog")

                try:
                    done, pending = await asyncio.wait(
                        {hb_task, recv_task, sim_task, wd_task}, return_when=asyncio.FIRST_COMPLETED
                    )
                    for task in pending:
                        task.cancel()
                    # 把异常抛出来，交给外层重连逻辑处理
                    for task in done:
                        exc = task.exception()
                        if exc:
                            raise exc
                finally:
                    for task in (hb_task, recv_task, sim_task, wd_task):
                        if not task.done():
                            task.cancel()
                    self._ws = None
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            log.warning("连接中断: %s", exc)
            # 平台地址可能变了（平台换 IP / 重启换端口）。无条件清空缓存，
            # 下次循环重新广播发现新地址；配置里的手工地址仅作发现失败时的兜底，
            # 而不是永远钉死旧 IP。
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
            # 设备能力（需求 §3）：平台据此决定能不能下发某类任务，
            # 而不是靠"猜这台手机是什么型号"。
            "cpu": self.device.get("cpu", ""),
            "gpu": self.device.get("gpu", ""),
            "npu": self.device.get("npu", ""),
            "qnn_version": self.device.get("qnn_version", ""),
            "is_root": self.device.get("is_root", -1),
            "capabilities": self.device.get("capabilities", {}),
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

    def _accept_device_token(self, msg: dict[str, Any]) -> None:
        """存下平台下发的设备 token。

        三条纪律：
        1. **只认给自己发的**。ack 里的 node_id 与本地不一致时拒绝 ——
           否则一个错发的 token 会让我们替另一台设备连上去。
        2. **只在变化时写盘**。每次重连都写一次文件毫无意义，还徒增写坏的机会。
        3. **写失败不能让连接失败**。共享 token 仍可用，退路没断，
           所以这里只记日志。
        """
        token = msg.get("device_token")
        if not isinstance(token, str) or not token:
            return
        ack_node = str(msg.get("node_id") or "")
        if ack_node and ack_node != self.node_id:
            log.warning(
                "register_ack 的 node_id 与本地不一致，拒绝保存设备 token ack=%s local=%s",
                ack_node, self.node_id,
            )
            return
        if token == self.device_token:
            return
        self.device_token = token
        save_device_token(self.node_id, token)
        log.info("已接收平台签发的设备 token（此后握手将改用它）node_id=%s", self.node_id)

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
                        # 服务状态随心跳上报：平台据此展示「当前运行的服务」，
                        # 也是进程被 kill -9 后平台能发现「服务没了」的主要途径。
                        "services": self._service_states(),
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
            self._accept_device_token(msg)
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
            self._spawn_deploy(ws, cmd_id, msg.get("payload") or {})
            return
        if mtype == "cancel_deploy":
            # 只记标记，不在这里做清理：真正的中止发生在部署流程的下一个
            # 阶段边界（见 _raise_if_cancelled）。原因是这样最干净 ——
            # 在别处动一个正在下载的临时文件，等于制造竞态。
            tid = str((msg.get("payload") or {}).get("task_id") or "")
            if tid:
                self._cancelled_tasks.add(tid)
                if len(self._cancelled_tasks) > 256:
                    # 只清最早的一批（set 无序，清掉一半即可，语义是"别无限涨"）
                    self._cancelled_tasks = set(list(self._cancelled_tasks)[-128:])
                log.warning("收到取消指令 task=%s（将在下一个阶段边界中止）", tid)
            await self._reply(ws, cmd_id, True, extra={"task_id": tid, "cancelled": True})
            return

        if mtype == "start_service":
            await self._service_command(ws, cmd_id, msg.get("payload") or {}, start=True)
            return
        if mtype in ("stop_service", "restart_service"):
            await self._service_command(ws, cmd_id, msg.get("payload") or {}, start=mtype == "restart_service")
            return
        if mtype == "service_logs":
            await self._service_logs(ws, cmd_id, msg.get("payload") or {})
            return
        if mtype == "remove_service":
            # 平台把服务定义删了：本地的启动记录也必须跟着消失，
            # 否则 Agent 下次重启会按残留的 auto_start 把它又拉起来 ——
            # 用户删掉的东西自己复活，是最难解释的一类 bug。
            sid = str((msg.get("payload") or {}).get("service_id") or "")
            rec = self._services.pop(sid, None)
            proc = rec.get("proc") if rec else None
            if proc is not None and proc.poll() is None:
                try:
                    proc.terminate()
                    proc.wait(timeout=5)
                except Exception:
                    try:
                        proc.kill()
                    except Exception:
                        pass
            self._saved_services.pop(sid, None)
            self._persist_services()
            await self._reply(ws, cmd_id, True, extra={"service_id": sid, "removed": True})
            return
        if mtype == "list_services":
            # 平台看门狗用：一次问清这台设备当前托管了什么、各是什么状态。
            # 单独一条指令而不是复用心跳，是因为看门狗要的是"此刻"，
            # 心跳是"刚才那一拍"，用它判断崩溃会慢一个周期。
            await self._reply(ws, cmd_id, True, extra={"services": self._service_states()})
            return

        if mtype in _FS_COMMANDS:
            await self._fs_op(ws, cmd_id, mtype, msg.get("payload") or {})
            return
        if mtype == "fs_fetch":
            await self._fs_fetch(ws, cmd_id, msg.get("payload") or {})
            return
        if mtype == "fs_push":
            self._spawn_push(ws, cmd_id, msg.get("payload") or {})
            return

        # M3 / M4 的指令在此处扩展（deploy_model / start_service ...）
        log.warning("收到暂不支持的指令 type=%s", mtype)
        if cmd_id:
            await self._reply(ws, cmd_id, ok=False, error=f"不支持的指令类型: {mtype}")

    async def _service_command(self, ws: Any, cmd_id: str, payload: dict[str, Any], start: bool) -> None:
        sid = str(payload.get("service_id") or "")
        if not sid:
            await self._reply(ws, cmd_id, False, "缺少 service_id"); return

        # 先停（stop / restart 都会走到这里）
        rec = self._services.pop(sid, None)
        if rec:
            proc = rec.get("proc")
            if proc is not None and proc.poll() is None:
                try:
                    proc.terminate()
                    proc.wait(timeout=5)
                except Exception:
                    try:
                        proc.kill()
                    except Exception:
                        pass

        if not start:
            self._persist_services()  # 停掉的服务必须从"该自动恢复"的名单里消失
            await self._reply(ws, cmd_id, True, extra={
                "service_id": sid, "status": "stopped",
            })
            return

        command = str(payload.get("command") or "")
        args = [str(x) for x in (payload.get("args") or [])]
        if not command:
            await self._reply(ws, cmd_id, False, "缺少 command"); return

        try:
            proc = self._spawn_service(sid, command, args)
        except Exception as exc:
            await self._reply(ws, cmd_id, False, f"启动失败: {exc}",
                              extra={"service_id": sid, "status": "error"})
            return

        self._services[sid] = {
            "proc": proc,
            "command": command,
            "args": args,
            "started_at": time.time(),
            "auto_restart": bool(payload.get("auto_restart")),
            # auto_start 由平台给定：它管的是"Agent 重启后要不要自动恢复"，
            # 与 auto_restart（进程崩了要不要拉）是两个事件源。
            "auto_start": bool(payload.get("auto_start")),
            "restart_count": 0,
            "log_path": str(self._service_log_path(sid)),
        }
        self._persist_services()
        await self._reply(ws, cmd_id, True, extra={
            "service_id": sid, "pid": proc.pid, "status": "running",
            "log_path": str(self._service_log_path(sid)),
        })

    async def _service_logs(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        """回传服务日志尾部。stdout/stderr 已在启动时重定向到文件，这里直接读。"""
        sid = str(payload.get("service_id") or "")
        if not sid:
            await self._reply(ws, cmd_id, False, "缺少 service_id"); return
        lines = int(payload.get("lines") or 200)
        lines = max(1, min(lines, 5000))
        path = self._service_log_path(sid)
        if not path.exists():
            await self._reply(ws, cmd_id, True, extra={
                "service_id": sid, "lines": [], "total_bytes": 0,
                "note": "尚无日志（服务可能还没启动过）",
            })
            return
        try:
            # 二进制读 + 容错解码：服务输出不一定是 UTF-8
            raw = path.read_bytes()
            text = raw.decode("utf-8", errors="replace")
            tail = text.splitlines()[-lines:]
            rec = self._services.get(sid) or {}
            proc = rec.get("proc")
            await self._reply(ws, cmd_id, True, extra={
                "service_id": sid,
                "lines": tail,
                "total_bytes": len(raw),
                "status": "running" if (proc is not None and proc.poll() is None) else "stopped",
            })
        except Exception as exc:
            await self._reply(ws, cmd_id, False, f"读取日志失败: {exc}",
                              extra={"service_id": sid})

    # ================================================================== 文件管理（§4）

    async def _fs_op(self, ws: Any, cmd_id: str, mtype: str, payload: dict[str, Any]) -> None:
        """文件类指令的统一入口：沙箱校验 → 线程池执行 → 结果回传。

        所有 fs_* 都走这里，是为了让"错误怎么回"只有一种写法：
        `FileError` 的 message **原样回给平台**（那是写给人看的话，
        比如"目录非空，需要显式确认递归删除"）；其它异常回泛化消息并记日志
        （那是给我们自己看的，不该把栈信息糊到用户界面上）。

        全部丢进线程池：列目录、算 du、复制大文件都是阻塞调用，
        直接在事件循环里跑会把心跳卡停 —— 而平台 15 秒收不到心跳就判离线。
        """

        def run() -> dict[str, Any]:
            if mtype == "fs_roots":
                return {"roots": self.files.roots()}
            if mtype == "fs_list":
                return self.files.list_dir(
                    str(payload.get("path") or ""),
                    offset=int(payload.get("offset") or 0),
                    limit=int(payload.get("limit") or 500),
                )
            if mtype == "fs_stat":
                return self.files.stat(str(payload.get("path") or ""))
            if mtype == "fs_du":
                return self.files.du(str(payload.get("path") or ""))
            if mtype == "fs_mkdir":
                return self.files.mkdir(
                    str(payload.get("path") or ""), parents=bool(payload.get("parents", True))
                )
            if mtype == "fs_delete":
                # recursive 默认 **False**：漏传参数时拒绝删非空目录，
                # 而不是替用户做出"全删了"这个不可逆的决定。
                return self.files.delete(
                    str(payload.get("path") or ""), recursive=bool(payload.get("recursive", False))
                )
            if mtype in ("fs_copy", "fs_move"):
                return self.files.transfer(
                    str(payload.get("src") or ""),
                    str(payload.get("dst") or ""),
                    move=(mtype == "fs_move"),
                    overwrite=bool(payload.get("overwrite", False)),
                )
            raise FileError(f"未知的文件操作 {mtype}")

        try:
            result = await asyncio.to_thread(run)
            await self._reply(ws, cmd_id, True, extra={"result": result})
        except FileError as exc:
            await self._reply(ws, cmd_id, False, str(exc))
        except Exception as exc:
            log.warning("文件操作异常 type=%s err=%s", mtype, exc, exc_info=True)
            await self._reply(ws, cmd_id, False, f"操作失败：{exc}")

    async def _fs_fetch(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        """上传：把设备上的文件**推给平台**（平台侧的"下载"）。

        方向仍是设备主动连平台 —— 与整个架构一致：平台从不主动连手机。
        传输令牌是一次性的、只对这一个文件有效（见平台 core/security.py），
        所以即使这条命令被日志记录下来，令牌也没法被用来读别的文件。
        """
        transfer_id = str(payload.get("transfer_id") or "")
        url = str(payload.get("url") or "")
        token = str(payload.get("token") or "")
        if not (transfer_id and url):
            await self._reply(ws, cmd_id, False, "缺少传输参数")
            return
        try:
            path, size = await asyncio.to_thread(self.files.open_read, str(payload.get("path") or ""))
        except FileError as exc:
            await self._reply(ws, cmd_id, False, str(exc))
            return

        state: dict[str, int] = {"done": 0, "size": size}

        def probe_offset() -> int:
            req = urllib.request.Request(url, method="GET")
            req.add_header("X-Transfer-Token", token)
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.loads((resp.read() or b"{}").decode() or "{}")
            return int(data.get("offset") or 0)

        def upload(offset: int) -> tuple[int, str]:
            digest = hashlib.sha256()
            state["done"] = offset
            body = _UploadBody(path, digest, state)
            try:
                if offset:
                    # 续传时把已跳过的前缀也补进 sha256，否则返回的摘要只覆盖
                    # [offset, size)，平台入库的校验值就成了残缺值。
                    with open(path, "rb") as pf:
                        while chunk := pf.read(1024 * 1024):
                            digest.update(chunk)
                    body._f.seek(offset)
                req = urllib.request.Request(url, data=body, method="POST")
                req.add_header("X-Transfer-Token", token)
                req.add_header("Content-Type", "application/octet-stream")
                # 显式给 Content-Length：否则 urllib 会退化成 chunked 上传，
                # 而中间只要有一层代理不认 chunked，传大文件就会 411/400。
                req.add_header("Content-Length", str(max(0, size - offset)))
                req.add_header("X-File-Size", str(size))
                req.add_header("X-Upload-Offset", str(offset))
                with urllib.request.urlopen(req, timeout=_XFER_TIMEOUT) as resp:
                    resp.read()
            finally:
                body.close()
            return size, digest.hexdigest()

        try:
            last_err: Exception | None = None
            total = 0
            digest = ""
            for attempt in range(3):
                try:
                    offset = await asyncio.to_thread(probe_offset)
                    if offset < 0:
                        offset = 0
                    if 0 < size <= offset:
                        # 平台已经收满（上一轮其实传完了），直接判成功
                        total, digest = size, ""
                        break
                    fut = asyncio.get_event_loop().run_in_executor(None, upload, offset)
                    await self._await_with_progress(ws, fut, transfer_id, state)
                    total, digest = await fut
                    break
                except Exception as exc:
                    last_err = exc
                    log.warning("文件上传第 %d 次失败 transfer=%s path=%s err=%s",
                                attempt + 1, transfer_id, path, exc)
                    if attempt < 2:
                        await asyncio.sleep(2 * (attempt + 1))
            else:
                raise last_err if last_err else RuntimeError("上传失败")

            await self._reply(
                ws, cmd_id, True,
                extra={"transfer_id": transfer_id, "bytes": total, "sha256": digest,
                       "direction": "pull", "path": str(path)},
            )
        except Exception as exc:
            log.warning("文件上传失败 transfer=%s path=%s err=%s", transfer_id, path, exc)
            await self._reply(ws, cmd_id, False, f"上传失败：{exc}",
                              extra={"transfer_id": transfer_id, "direction": "pull"})


    def _spawn_task(self, key: str, coro: Any) -> None:
        """把传输任务丢到后台执行并登记 in-flight，不阻塞 recv 循环。

        登记失败（同一 task 已在跑）时由调用方负责回 ack；这里只做登记和清理。
        """
        if len(self._active_tasks) > 64:
            # 上限保护：只清最早的一半（set 无序，语义是"别无限涨"）
            self._active_tasks = set(list(self._active_tasks)[32:])
        self._active_tasks.add(key)
        task = asyncio.create_task(coro)
        task.add_done_callback(lambda _t: self._active_tasks.discard(key))
        log.info("后台传输任务已启动 key=%s", key)

    def _spawn_deploy(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        task_id = str(payload.get("task_id") or "")
        if task_id and task_id in self._active_tasks:
            log.info("同一部署任务已在执行，忽略重复下发 task=%s", task_id)
            asyncio.create_task(self._reply(ws, cmd_id, True,
                                            extra={"duplicate": True, "task_id": task_id}))
            return
        self._spawn_task(task_id or cmd_id, self._deploy_model(ws, cmd_id, payload))

    def _spawn_push(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        transfer_id = str(payload.get("transfer_id") or "")
        if transfer_id and transfer_id in self._active_tasks:
            log.info("同一推文件任务已在执行，忽略重复下发 transfer=%s", transfer_id)
            asyncio.create_task(self._reply(ws, cmd_id, True,
                                            extra={"duplicate": True, "transfer_id": transfer_id}))
            return
        self._spawn_task(transfer_id or cmd_id, self._fs_push(ws, cmd_id, payload))

    async def _deploy_model(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        """平台下发的模型部署：下载 → 校验 → 落位 → 拉附属物 → Self Test。

        **分阶段上报**（需求 §6 的部署状态机）是这里最重要的改动。
        原来只有一个 downloading，下载完直接 ready，中间全是黑盒：
        "SHA 对不上"和"Self Test 挂了"在平台上长得一模一样，
        排查时只能去翻手机日志。现在每一步都有独立的阶段与进度，
        卡在哪一步一眼可见。

        落盘纪律与文件传输一致：`.part` 临时文件 + 流式 sha256 + 原子改名。
        "下到一半断网"只会留下一个 `.part`，绝不会在目标路径上出现
        一份看起来完整、实际截断的模型文件 —— 后者比失败难查得多。
        """
        url = str(payload.get("url") or "")
        # 只取 basename，禁止 `../` 之类路径穿越（与文件管理同一条红线）
        name = Path(str(payload.get("target") or "model.bin")).name or "model.bin"
        expected = str(payload.get("sha256") or "").lower()
        size_hint = int(payload.get("size") or 0)
        task_id = payload.get("task_id")
        # 一次性下载令牌：平台为**这一个模型、这一次下发**签发。
        # 它只能下载本模型的文件与附属物，调不了任何管理接口 ——
        # 所以它丢了也不等于平台失守（这正是它不用共享 token 的原因）。
        dl_token = str(payload.get("download_token") or "")
        if not url:
            await self._reply(ws, cmd_id, False, "缺少下载地址")
            return

        manifest = payload.get("manifest")
        if not isinstance(manifest, list):
            manifest = []
        self_test = payload.get("self_test")
        if not isinstance(self_test, dict) or not self_test.get("command"):
            self_test = None

        models_dir = Path(self.cfg.models_path)
        models_dir.mkdir(parents=True, exist_ok=True)
        target = models_dir / name
        temp = models_dir / f"{name}.part"

        # 不删 `.part`：上一轮进程被杀 / 掉线留下的残片正是断点续传的起点。
        # 残片是否可用由 _download_model_file 判断（超过期望大小 = 写坏，删掉重来；
        # 等于期望大小 = 直接哈希校验），错误续传不可能把文件写超。
        if temp.exists():
            log.info("检测到上一轮残片，将从断点续传 task=%s file=%s size=%d",
                     task_id, name, temp.stat().st_size)

        # ---- 回滚点（A1）----
        # 从这一刻起，设备上任何"即将被覆盖或新建"的文件都必须先在这里留一份。
        # 放在 models_dir 里面而不是旁边，是为了**和被动的是同一个文件系统** ——
        # os.replace 只有在同一文件系统内才是"改个名"（常数时间、不占额外空间），
        # 跨文件系统会退化成真的复制，几个 GB 的模型复制一次就是灾难。
        journal = InstallJournal(models_dir / ".rollback", task_id)
        try:
            stale_note = journal.prepare()
        except RollbackError as exc:
            # **建不了回滚点就不动手**。覆盖一个救不回来的文件，
            # 比这次部署失败严重得多 —— 这条纪律写在 agent_rollback 的模块注释里。
            log.error("无法建立回滚点，部署中止 task=%s err=%s", task_id, exc)
            await self._reply(ws, cmd_id, False, f"无法建立回滚点：{exc}",
                              extra={"status": "failed", "task_id": task_id,
                                     "rolled_back": False, "rollback_note": ""})
            return
        if stale_note:
            log.warning("部署前先收拾了上一轮的遗留 task=%s：%s", task_id, stale_note)

        # 同一任务重投时，上一轮的取消标记必须清掉，否则新的一轮一开头就被判取消
        if task_id:
            self._cancelled_tasks.discard(str(task_id))

        state: dict[str, int] = {"done": 0, "size": size_hint}
        try:
            total, digest = await self._download_model_file(
                ws, task_id, url, temp, state, dl_token, expected=size_hint
            )
            self._raise_if_cancelled(task_id)

            # ---- 校验（状态机：verifying）----
            await self._send_progress(ws, task_id, state, phase="verifying")
            if expected and digest != expected:
                temp.unlink(missing_ok=True)
                raise RuntimeError(f"SHA-256 校验失败（期望 {expected[:12]}… 实际 {digest[:12]}…）")
            if size_hint and total != size_hint:
                # 长度不符但 sha 相符，说明平台登记的大小与文件不一致 ——
                # 记警告不失败：sha 才是唯一判据，大小只是辅助。
                log.warning("模型大小与登记不符 task=%s 登记=%s 实际=%s",
                            task_id, size_hint, total)

            # ---- 落位（状态机：installing）----
            await self._send_progress(ws, task_id, state, phase="installing")
            # 覆盖之前先把旧版本挪走。这一步失败就抛 —— 没有后悔药不落位。
            journal.capture(target)
            temp.replace(target)
            self._raise_if_cancelled(task_id)

            # ---- 附属物（Deployment Package）----
            assets = await self._fetch_manifest(ws, task_id, manifest, models_dir, dl_token,
                                                journal=journal)
            self._raise_if_cancelled(task_id)

            # ---- Self Test（状态机：testing）----
            test_output = ""
            if self_test:
                await self._send_progress(ws, task_id, state, phase="testing")
                ok, test_output = await self._run_self_test(self_test, target)
                if not ok:
                    raise RuntimeError(
                        f"Self Test 未通过（{self_test.get('command')}）：{test_output[:300]}"
                    )

            journal.commit()   # 成功：后悔药作废
            await self._reply(ws, cmd_id, True, extra={
                "status": "ready",
                "bytes_downloaded": total,
                "sha256": digest,
                "target": str(target),
                "assets": assets,
                "self_test_output": test_output[:1000],
                "task_id": task_id,
                "rolled_back": False,
                "rollback_note": "",
            })
        except _DeployCancelled as exc:
            temp.unlink(missing_ok=True)
            rolled_back, note = journal.rollback()
            # 取消不是失败：状态必须是 cancelled，写成 failed 会让用户
            # 以为是自己操作出了问题，而实际上正是他取消了它。
            # 但**设备上的文件要还原** —— 用户要的是"当没发生过"，
            # 不是"东西装了一半、任务显示已取消"。
            await self._reply(ws, cmd_id, False, str(exc),
                              extra={"status": "cancelled", "task_id": task_id,
                                     "rolled_back": rolled_back, "rollback_note": note})
            self._log_rollback(task_id, rolled_back, note)
        except Exception as exc:
            temp.unlink(missing_ok=True)
            # 失败发生在落位之前的（下载断、sha 对不上）这里什么都不会还原 ——
            # journal 里没有记录，rollback 返回的说明就是"无需还原"。
            rolled_back, note = journal.rollback()
            await self._reply(ws, cmd_id, False, str(exc),
                              extra={"status": "failed", "task_id": task_id,
                                     "rolled_back": rolled_back, "rollback_note": note})
            self._log_rollback(task_id, rolled_back, note)

    def _log_rollback(self, task_id: Any, rolled_back: bool, note: str) -> None:
        """回滚结果必须留下日志。

        **回滚失败的时刻，日志是唯一的证据** —— 那时设备处于中间状态，
        界面上一句"部署失败"完全说明不了它现在是什么样。
        """
        if rolled_back:
            log.warning("部署失败，已回滚 task=%s：%s", task_id, note)
        else:
            log.error("部署失败，且**回滚未完整完成** task=%s：%s —— 设备可能停在中间状态",
                      task_id, note)

    def _raise_if_cancelled(self, task_id: Any) -> None:
        """在阶段切换处检查取消标记。

        只在**阶段边界**检查，不放进下载循环里逐字节判断 ——
        循环里查共享集合要加锁，为了一次取消请求不值得。
        下载进行中收到取消，最迟在下载结束后的第一个边界生效，
        比"立即中断 + 一堆半成品状态"更好收拾。
        """
        if task_id and str(task_id) in self._cancelled_tasks:
            raise _DeployCancelled(f"部署已被用户取消（task={task_id}）")

    def _dl_lock_for(self, key: str) -> "threading.Lock":
        """取（或创建）某个目标文件的下载锁，保证同名 `.part` 串行写入。"""
        lock = self._dl_locks.get(key)
        if lock is None:
            lock = threading.Lock()
            self._dl_locks[key] = lock
        return lock

    async def _download_model_file(
        self, ws: Any, task_id: Any, url: str, temp: Path, state: dict[str, int],
        dl_token: str = "", expected: int = 0,
    ) -> tuple[int, str]:
        """后台线程下载 + 主协程按 1s 节奏上报进度。返回 (字节数, sha256)。

        治「下发模型后文件一直看不到」的两个根因：
        1. **同名 `.part` 串行写**：网络抖动让连接反复断、平台反复重投，Agent 可能
           同时有「上一轮没来得及收尾的下载线程」和「新一轮下载线程」在写同一个
           `.part`。两个线程都按 append 写，文件就会被写超、翻倍，SHA 永远对不上，
           于是永远走不到「落位 / ready」，文件自然也看不到。用 per-file 锁串起来。
        2. **断点在文件末尾之后不再续传**：`.part` 已等于期望大小时再发
           `Range: bytes=N-`，服务器会把越界 Range 回退成 200 全量；若仍按 append
           追加，等于把整份文件再写一遍。等于期望大小 → 直接对盘上文件算哈希返回；
           超过期望大小 → 说明被写坏了，删掉从头再来。
        """
        lock = self._dl_lock_for(str(temp))

        def download() -> tuple[int, str]:
            with lock:
                digest = hashlib.sha256()
                offset = temp.stat().st_size if temp.exists() else 0

                # ---- 断点已在（或越过）文件末尾：不再发起网络请求 ----
                if expected and offset >= expected:
                    if offset > expected:
                        # 被并发写坏 / 之前越界续传写超了 —— 删掉重来
                        temp.unlink(missing_ok=True)
                        offset = 0
                    else:
                        # 整份已在盘上，只差校验。哈希后交给上层做 SHA 校验与改名。
                        with temp.open("rb") as old:
                            while chunk := old.read(1024 * 1024):
                                digest.update(chunk)
                        state["size"] = expected
                        state["done"] = expected
                        return expected, digest.hexdigest()

                headers = {"X-Download-Token": dl_token} if dl_token else {}
                if offset:
                    headers["Range"] = f"bytes={offset}-"
                req = urllib.request.Request(url, headers=headers)
                with urllib.request.urlopen(req, timeout=_XFER_TIMEOUT) as src:
                    status = int(getattr(src, "status", 0) or 0)
                    # 续传却拿到全量响应（Range 越界被服务器回退成 200）：不能再 append，
                    # 否则整份文件会被再写一遍。清空后把当前 200 全量流写进去。
                    if offset and status != 206:
                        temp.unlink(missing_ok=True)
                        offset = 0
                    mode = "wb" if offset == 0 else "ab"
                    with temp.open(mode) as dst:
                        cl = src.headers.get("Content-Length")
                        if cl:
                            # 206 时 Content-Length 是剩余部分，总大小要加回 offset；
                            # 200（offset 已被归零）时它就是总大小。
                            state["size"] = int(cl) + offset
                        else:
                            state["size"] = expected or 0
                        state["done"] = offset
                        if offset:
                            with temp.open("rb") as old:
                                while chunk := old.read(1024 * 1024):
                                    digest.update(chunk)
                        total = offset
                        while chunk := src.read(1024 * 1024):
                            dst.write(chunk)
                            digest.update(chunk)
                            total += len(chunk)
                            state["done"] = total
                return total, digest.hexdigest()

        # 网络抖动时按 1s 节奏上报进度，并最多重试 3 次（续传）。
        return await self._download_with_retry(
            ws, self._send_progress, str(task_id or ""), state, download)

    async def _fetch_manifest(
        self, ws: Any, task_id: Any, manifest: list[Any], models_dir: Path,
        dl_token: str = "", journal: InstallJournal | None = None,
    ) -> list[dict[str, Any]]:
        """拉取模型本体之外的附属物（context / DLC / lib / config）。

        **附属物拿不到只报警不失败**。有些模型不依赖附属物，因为一个可选的
        config 没拉下来就把整次部署判失败，是把"可选"当成了"必需"。
        返回逐项结果，让平台如实显示哪些拿到了、哪些没有。

        `journal` 是回滚日志（A1）：附属物**同样会被覆盖**，
        所以每一个在替换之前也要先记一笔 —— 只保护模型本体，
        回滚之后附属物还是新的，等于没回滚。
        """
        out: list[dict[str, Any]] = []
        for item in manifest:
            if not isinstance(item, dict):
                continue
            aname = Path(str(item.get("name") or "")).name
            aurl = str(item.get("url") or "")
            if not aname or not aurl:
                continue
            dest = models_dir / aname
            tmp = models_dir / f"{aname}.part"
            want = str(item.get("sha256") or "").lower()
            try:
                def fetch() -> tuple[int, str]:
                    d = hashlib.sha256()
                    n = 0
                    aheaders = {"X-Download-Token": dl_token} if dl_token else {}
                    areq = urllib.request.Request(aurl, headers=aheaders)
                    with urllib.request.urlopen(areq, timeout=_XFER_TIMEOUT) as src, tmp.open("wb") as dst:
                        while chunk := src.read(1024 * 1024):
                            dst.write(chunk)
                            d.update(chunk)
                            n += len(chunk)
                    return n, d.hexdigest()

                # 附属物小，但同样可能碰上网络抖动；重试 3 次再放弃。
                last_err: Exception | None = None
                for attempt in range(3):
                    try:
                        size, got = await asyncio.get_event_loop().run_in_executor(None, fetch)
                        break
                    except Exception as exc:
                        last_err = exc
                        if attempt < 2:
                            await asyncio.sleep(2 * (attempt + 1))
                else:
                    raise last_err if last_err else RuntimeError("拉取失败")
                if want and got != want:
                    tmp.unlink(missing_ok=True)
                    raise RuntimeError(f"附属物 {aname} SHA-256 校验失败")
                if journal is not None:
                    # 留不下备份就不覆盖这一个附属物。走下面那个 except，
                    # 结果是"这一项失败"而不是"整次部署失败" —— 与本函数
                    # "附属物不阻断部署"的既定口径一致。
                    journal.capture(dest)
                tmp.replace(dest)
                out.append({"name": aname, "ok": True, "size": size,
                            "kind": str(item.get("kind") or "other")})
                log.info("附属物已落位 task=%s name=%s size=%s", task_id, aname, size)
            except Exception as exc:
                tmp.unlink(missing_ok=True)
                out.append({"name": aname, "ok": False, "error": str(exc)[:200],
                            "kind": str(item.get("kind") or "other")})
                log.warning("附属物拉取失败（不阻断部署）task=%s name=%s err=%s",
                            task_id, aname, exc)
        return out

    async def _run_self_test(
        self, spec: dict[str, Any], model_path: Path
    ) -> tuple[bool, str]:
        """执行 Self Test 命令，返回 (是否通过, 输出摘要)。

        命令模板支持 `{model_path}` 占位符 —— 平台不知道手机上的落盘路径，
        只有 Agent 知道，所以路径必须由 Agent 填。
        超时按配置强制生效：Self Test 是个验证步骤，不是生产任务，
        让它无限期挂着只会把一次部署变成一次挂起。
        """
        command = str(spec.get("command") or "").replace("{model_path}", str(model_path))
        timeout = max(1, int(spec.get("timeout") or 60))
        try:
            proc = await asyncio.create_subprocess_shell(
                command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
            )
            try:
                stdout, _ = await asyncio.wait_for(proc.communicate(), timeout=timeout)
            except asyncio.TimeoutError:
                proc.kill()
                return False, f"Self Test 超时（{timeout}s）"
            output = (stdout or b"").decode(errors="replace").strip()
            if proc.returncode == 0:
                return True, output or "OK"
            return False, f"退出码 {proc.returncode}：{output}"
        except Exception as exc:
            return False, f"Self Test 无法执行：{exc}"


    async def _fs_push(self, ws: Any, cmd_id: str, payload: dict[str, Any]) -> None:
        """下载：把平台暂存的文件**拉到设备**（平台侧的"上传"）。

        与 `_deploy_model` 同一套纪律：`.part` 临时文件 + 流式 sha256 + 原子改名。
        这样"传到一半断网"只会留下一个 `.part`，绝不会在目标路径上留下一份
        看起来完整、实际截断的文件 —— 后者比失败更难查。
        """
        transfer_id = str(payload.get("transfer_id") or "")
        url = str(payload.get("url") or "")
        token = str(payload.get("token") or "")
        expected = str(payload.get("sha256") or "").lower()
        size = int(payload.get("size") or 0)
        if not (transfer_id and url):
            await self._reply(ws, cmd_id, False, "缺少传输参数")
            return

        try:
            target = await asyncio.to_thread(
                lambda: self.files.prepare_write(str(payload.get("path") or ""), size=size)
            )
        except FileError as exc:
            await self._reply(ws, cmd_id, False, str(exc),
                              extra={"transfer_id": transfer_id, "direction": "push"})
            return

        temp = target.with_suffix(target.suffix + ".part")
        # 不删 `.part`：跨进程/跨重投续传都靠它。残片是否可用在 download() 里判断。
        state: dict[str, int] = {"done": 0, "size": size}
        try:
            def download() -> tuple[int, str]:
                digest = hashlib.sha256()
                offset = temp.stat().st_size if temp.exists() else 0

                # ---- 断点已在（或越过）文件末尾：不再发起网络请求 ----
                if size and offset >= size:
                    if offset > size:
                        # 被写坏 / 写超了 —— 删掉重来
                        temp.unlink(missing_ok=True)
                        offset = 0
                    else:
                        with temp.open("rb") as old:
                            while chunk := old.read(1024 * 1024):
                                digest.update(chunk)
                        state["done"] = size
                        return size, digest.hexdigest()

                headers = {"X-Transfer-Token": token}
                if offset:
                    headers["Range"] = f"bytes={offset}-"
                req = urllib.request.Request(url, headers=headers)
                with urllib.request.urlopen(req, timeout=_XFER_TIMEOUT) as src:
                    status = int(getattr(src, "status", 0) or 0)
                    # 续传却拿到全量响应（Range 越界被服务器回退成 200）：不能再 append，
                    # 否则整份文件会被再写一遍、越写越大。清空后重写。
                    if offset and status != 206:
                        temp.unlink(missing_ok=True)
                        offset = 0
                    cl = int(src.headers.get("Content-Length") or 0)
                    # 续传时 Content-Length 只是剩余部分，总大小要加回 offset
                    state["size"] = (cl + offset) if cl else size
                    state["done"] = offset
                    mode = "ab" if offset else "wb"
                    if offset:
                        with temp.open("rb") as old:
                            while chunk := old.read(1024 * 1024):
                                digest.update(chunk)
                    total = offset
                    with temp.open(mode) as dst:
                        while chunk := src.read(1024 * 1024):
                            dst.write(chunk)
                            digest.update(chunk)
                            total += len(chunk)
                            state["done"] = total
                return total, digest.hexdigest()

            total, digest = await self._download_with_retry(
                ws, self._send_transfer_progress, transfer_id, state, download)
            if expected and digest != expected:
                temp.unlink(missing_ok=True)
                raise RuntimeError("SHA-256 校验失败，文件已丢弃")
            temp.replace(target)
            await self._reply(
                ws, cmd_id, True,
                extra={"transfer_id": transfer_id, "bytes": total, "sha256": digest,
                       "direction": "push", "path": str(target)},
            )
        except Exception as exc:
            temp.unlink(missing_ok=True)
            log.warning("文件下载失败 transfer=%s path=%s err=%s", transfer_id, target, exc)
            await self._reply(ws, cmd_id, False, f"下载失败：{exc}",
                              extra={"transfer_id": transfer_id, "direction": "push"})

    async def _download_with_retry(
        self, ws: Any, reporter: Any, ident: str, state: dict[str, int],
        download: Any,
    ) -> tuple[int, str]:
        """带断点续传的下载重试：最多 3 次，每次从 `.part` 的当前偏移继续。

        与上传方向（_fs_fetch）的 3 次重试对齐。之前只有上传会重试，下载方向
        （推文件 / 部署模型 / 拉附属物）一次网络抖动就直接判失败 —— 这正是
        「平台访问节点不稳定、老是传不了文件」的一大来源。download() 每次被调
        都会重新读 `.part` 的现有字节数当作续传偏移，所以这里直接重跑即可。
        """
        last_err: Exception | None = None
        for attempt in range(3):
            fut = asyncio.get_event_loop().run_in_executor(None, download)
            last = 0.0
            while not fut.done():
                await asyncio.sleep(0.5)
                if time.time() - last >= 1.0:
                    last = time.time()
                    await reporter(ws, ident, state)
            try:
                total, digest = await fut
            except Exception as exc:
                last_err = exc
                log.warning("下载第 %d 次失败 id=%s err=%s", attempt + 1, ident, exc)
                if attempt < 2:
                    await asyncio.sleep(2 * (attempt + 1))
                continue
            await reporter(ws, ident, state)  # 收尾进度
            return total, digest
        raise last_err if last_err else RuntimeError("下载失败")

    async def _await_with_progress(
        self, ws: Any, fut: "asyncio.Future[Any]", transfer_id: str, state: dict[str, int]
    ) -> None:
        """等一个后台传输任务，同时按固定节奏上报进度。

        节奏是 1 秒一次，和模型部署保持一致：更密只会刷爆 WS，
        而 WS 一堵，心跳也跟着慢，平台可能因此误判节点离线。
        """
        last = 0.0
        while not fut.done():
            await asyncio.sleep(0.5)
            if time.time() - last >= 1.0:
                last = time.time()
                await self._send_transfer_progress(ws, transfer_id, state)
        await self._send_transfer_progress(ws, transfer_id, state)

    async def _send_transfer_progress(
        self, ws: Any, transfer_id: str, state: dict[str, int]
    ) -> None:
        """上报一次文件传输进度。拿不到总大小时 percent 为 None（不编造进度）。"""
        if not transfer_id:
            return
        done = int(state.get("done") or 0)
        size = int(state.get("size") or 0)
        msg = {
            "type": "transfer_progress",
            "ts": time.time(),
            "payload": {
                "transfer_id": transfer_id,
                "node_id": self.node_id,
                "bytes_done": done,
                "bytes_total": size or None,
                "percent": round(done * 100.0 / size, 1) if size else None,
            },
        }
        try:
            await ws.send(json.dumps(msg))
        except Exception as exc:
            log.debug("传输进度上报失败 transfer=%s err=%s", transfer_id, exc)
    async def _send_progress(
        self, ws: Any, task_id: Any, state: dict[str, int], phase: str = ""
    ) -> None:
        """上报一次下载进度。task_id 为空说明不是平台下发的任务，没必要报。

        `phase` 是部署状态机里的阶段（downloading / verifying / installing /
        testing）。不带 phase 的是**常规字节进度**，平台按 downloading 处理；
        带 phase 的是**阶段切换**，平台据此推进状态机。
        这样一条通道既能报进度又能报阶段，不必为阶段另开一种消息类型。
        """
        if not task_id:
            return
        done = int(state.get("done") or 0)
        size = int(state.get("size") or 0)
        msg = {
            "type": "progress",
            "ts": time.time(),
            "payload": {
                "task_id": str(task_id),
                "node_id": self.node_id,
                "bytes_downloaded": done,
                "bytes_total": size or None,
                # 拿不到总大小时 percent 为 None，让前端显示字节数而不是假进度
                "percent": round(done * 100.0 / size, 1) if size else None,
                "phase": phase or "downloading",
            },
        }
        try:
            await ws.send(json.dumps(msg))
        except Exception as exc:
            # 进度上报失败不能影响下载本身
            log.debug("进度上报失败 task=%s err=%s", task_id, exc)

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
        # 正常退出路径顺手收掉托管子进程：它们跑在独立会话里，不主动收就会变成
        # 占用端口的孤儿，下次启动再拉起同名服务直接撞端口。
        try:
            n = agent.stop_all_services()
            if n:
                log.info("已停止 %d 个托管服务进程", n)
        except Exception as exc:
            log.warning("停止托管服务时出错（不影响退出）err=%s", exc)
        agent._persist_services()
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
