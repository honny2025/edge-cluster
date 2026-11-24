//! 设备信息与指标采集。
//!
//! 真机（Android/Termux）优先，逐级降级；拿不到的指标一律返回 None，不造假。
//! 模拟模式（--simulate）下生成"合理"的假数据。

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::Rng;
use serde_json::{json, Value};

use crate::config::SimulateCfg;

// 路径前缀做成可重定向的进程级全局，只为可测试：真机读 /proc、/sys，验收脚本把
// 它俩指向合成目录，按场景摆文件就能测降级链。默认值不变（生产行为不受影响）。
static PROC: Mutex<Option<String>> = Mutex::new(None);
static SYS: Mutex<Option<String>> = Mutex::new(None);

fn proc() -> String {
    PROC.lock()
        .expect("proc path lock poisoned")
        .clone()
        .unwrap_or_else(|| "/proc".to_string())
}

fn sys() -> String {
    SYS.lock()
        .expect("sys path lock poisoned")
        .clone()
        .unwrap_or_else(|| "/sys".to_string())
}

/// 测试用：把 /proc /sys 重定向到合成目录。传 None 恢复默认。
#[cfg(test)]
fn set_paths(proc_dir: Option<String>, sys_dir: Option<String>) {
    *PROC.lock().expect("proc path lock poisoned") = proc_dir;
    *SYS.lock().expect("sys path lock poisoned") = sys_dir;
}

fn is_android() -> bool {
    env::var_os("ANDROID_ROOT").is_some() || Path::new("/system/build.prop").exists()
}

/// 执行命令，失败一律返回空串（采集失败不能影响 Agent 主流程）。
fn run_cmd(cmd: &[&str], timeout: f64) -> String {
    if cmd.is_empty() {
        return String::new();
    }
    let mut child = match Command::new(cmd[0])
        .args(&cmd[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    if let Some(mut out) = child.stdout.take() {
                        let mut s = String::new();
                        if out.read_to_string(&mut s).is_ok() {
                            return s.trim().to_string();
                        }
                    }
                }
                return String::new();
            }
            Ok(None) => {
                if start.elapsed().as_secs_f64() > timeout {
                    let _ = child.kill();
                    return String::new();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return String::new(),
        }
    }
}

fn has_cmd(cmd: &str) -> bool {
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let p = dir.join(cmd);
            if p.exists() {
                return true;
            }
        }
    }
    false
}

fn read_text(path: &str) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_float(path: &str) -> Option<f64> {
    let raw = read_text(path);
    if raw.is_empty() {
        return None;
    }
    raw.parse::<f64>().ok().map(|v| (v * 10.0).round() / 10.0)
}

fn read_first(paths: &[String], pattern: &str) -> String {
    for p in paths {
        if !Path::new(p).exists() {
            continue;
        }
        if let Ok(text) = fs::read_to_string(p) {
            for line in text.lines() {
                if let Some(rest) = line.split_once(pattern).map(|x| x.1) {
                    return rest.trim().to_string();
                }
            }
        }
    }
    String::new()
}

fn readable(path: &str) -> bool {
    match fs::File::open(path) {
        Ok(mut f) => {
            let mut b = [0u8; 1];
            f.read(&mut b).map(|n| n > 0).unwrap_or(false)
        }
        Err(_) => false,
    }
}

// ============================================================== 设备信息（静态）

pub fn device_info(simulate: bool, sim_cfg: &SimulateCfg) -> Value {
    if simulate {
        let model = if sim_cfg.model.is_empty() { "Xiaomi 13" } else { &sim_cfg.model };
        let soc = if sim_cfg.soc.is_empty() { "Snapdragon 8 Gen 2" } else { &sim_cfg.soc };
        let android = if sim_cfg.android_version.is_empty() { "14" } else { &sim_cfg.android_version };
        return json!({
            "model": model,
            "soc": soc,
            "android_version": android,
            "cpu": "Qualcomm Kryo",
            "gpu": "Adreno 740",
            "npu": "Qualcomm Hexagon NPU",
            "qnn_version": "2.28.0",
            "is_root": 0,
            "capabilities": capabilities(true, 0, "2.28.0"),
        });
    }

    let model = {
        let v = run_cmd(&["getprop", "ro.product.model"], 3.0);
        if v.is_empty() {
            read_first(
                &["/system/build.prop".to_string(), "/vendor/build.prop".to_string()],
                "ro.product.model=",
            )
        } else {
            v
        }
    };
    let mut soc = run_cmd(&["getprop", "ro.soc.model"], 3.0);
    if soc.is_empty() {
        soc = run_cmd(&["getprop", "ro.board.platform"], 3.0);
    }
    if soc.is_empty() {
        soc = read_first(&[format!("{}/cpuinfo", proc())], "Hardware");
        if soc.is_empty() {
            soc = cpuinfo_hardware();
        }
    }
    let mut android = run_cmd(&["getprop", "ro.build.version.release"], 3.0);
    if android.is_empty() {
        android = read_first(
            &["/system/build.prop".to_string()],
            "ro.build.version.release=",
        );
    }

    let is_root = root_state();
    let qnn = qnn_version();
    json!({
        "model": if model.is_empty() { "Unknown" } else { &model },
        "soc": if soc.is_empty() { "Unknown" } else { &soc },
        "android_version": if android.is_empty() { "Unknown" } else { &android },
        "cpu": cpu_model(&soc),
        "gpu": gpu_model(),
        "npu": npu_model(&soc, &qnn),
        "qnn_version": qnn,
        "is_root": is_root,
        "capabilities": capabilities(false, is_root, &qnn),
    })
}

fn root_state() -> i64 {
    #[cfg(unix)]
    {
        // geteuid()==0 → 1
        unsafe {
            if libc::geteuid() == 0 {
                return 1;
            }
        }
    }
    if !has_cmd("su") {
        return if is_android() { 0 } else { -1 };
    }
    let out = run_cmd(&["su", "-c", "id"], 2.0);
    if out.contains("uid=0") {
        1
    } else {
        0
    }
}

fn qnn_version() -> String {
    for prop in [
        "ro.vendor.qti.qnn.version",
        "vendor.qti.qnn.version",
        "ro.vendor.qti.hexagon.version",
        "ro.vendor.hexagon.version",
    ] {
        let v = run_cmd(&["getprop", prop], 3.0);
        if !v.is_empty() {
            return v.chars().take(64).collect();
        }
    }
    String::new()
}

fn qnn_runtime_present() -> i64 {
    const DIRS: [&str; 6] = [
        "/vendor/lib64",
        "/vendor/lib",
        "/odm/lib64",
        "/odm/lib",
        "/system/lib64",
        "/system/lib",
    ];
    const NAMES: [&str; 4] = [
        "libQnnHtp.so",
        "libQnnSystem.so",
        "libQnnCpu.so",
        "libQnnHtpPrepare.so",
    ];
    let mut opened = false;
    for d in DIRS {
        if !Path::new(d).is_dir() {
            continue;
        }
        if fs::read_dir(d).is_ok() {
            opened = true;
        }
        for name in NAMES {
            if Path::new(&format!("{d}/{name}")).exists() {
                return 1;
            }
        }
    }
    if opened {
        0
    } else {
        -1
    }
}

fn cpu_model(soc: &str) -> String {
    let m = read_first(&[format!("{}/cpuinfo", proc())], "model name");
    if !m.is_empty() {
        return m;
    }
    let board = run_cmd(&["getprop", "ro.product.board"], 3.0);
    if !board.is_empty() {
        return board;
    }
    let hw = run_cmd(&["getprop", "ro.hardware"], 3.0);
    if !hw.is_empty() {
        return hw;
    }
    if !soc.is_empty() {
        soc.to_string()
    } else {
        "Unknown".to_string()
    }
}

fn gpu_model() -> String {
    for rel in [
        "class/kgsl/kgsl-3d0/gpu_model",
        "class/kgsl/kgsl-3d0/devfreq/device/uevent",
        "class/misc/mali0/device/gpuinfo",
    ] {
        let v = read_text(&format!("{}/{rel}", sys()));
        if v.is_empty() {
            continue;
        }
        let v = v.split_whitespace().next().unwrap_or(&v).to_string();
        return v.chars().take(64).collect();
    }
    for prop in ["ro.hardware.vulkan", "ro.hardware.egl", "ro.board.platform"] {
        let v = run_cmd(&["getprop", prop], 3.0);
        if !v.is_empty() {
            return v.chars().take(64).collect();
        }
    }
    "Unknown".to_string()
}

fn npu_model(soc: &str, qnn_version: &str) -> String {
    let blob = format!(
        "{} {} {} {} {}",
        soc,
        run_cmd(&["getprop", "ro.hardware.npu"], 3.0),
        run_cmd(&["getprop", "ro.vendor.qti.hardware.npu"], 3.0),
        read_text(&format!("{}/kernel/npu/type", sys())),
        read_text(&format!("{}/class/npu/name", sys()))
    )
    .to_lowercase();
    if blob.contains("hexagon") || blob.contains("htp") {
        return "Qualcomm Hexagon NPU".to_string();
    }
    if blob.contains("apu") {
        return "MediaTek APU".to_string();
    }
    if blob.contains("npu") {
        let name = read_text(&format!("{}/class/npu/name", sys()));
        if !name.is_empty() {
            return name.chars().take(64).collect();
        }
        return "Generic NPU".to_string();
    }
    if !qnn_version.is_empty() {
        return "Qualcomm Hexagon NPU".to_string();
    }
    "Unknown".to_string()
}

fn capabilities(simulate: bool, is_root: i64, qnn_version: &str) -> Value {
    if simulate {
        return json!({
            "can_proc_stat": true,
            "can_proc_net": true,
            "can_thermal": true,
            "termux_api": true,
            "gpu_counters": true,
            "qnn_runtime": if qnn_version.is_empty() { 0 } else { 1 },
            "root_state": is_root,
            "root": is_root == 1,
        });
    }
    json!({
        "can_proc_stat": readable(&format!("{}/stat", proc())),
        "can_proc_net": readable(&format!("{}/net/dev", proc())) || readable(&format!("{}/self/net/dev", proc())),
        "can_thermal": !thermal_zones().is_empty(),
        "termux_api": has_cmd("termux-battery-status"),
        "gpu_counters": gpu_counters_present(),
        "qnn_runtime": qnn_runtime_present(),
        "root_state": is_root,
        "root": is_root == 1,
    })
}

const GPU_COUNTER_PATHS: [&str; 5] = [
    "class/kgsl/kgsl-3d0/gpu_busy_percentage",
    "class/kgsl/kgsl-3d0/gpubusy",
    "class/misc/mali0/device/utilization",
    "kernel/gpu/gpu_busy",
    "class/devfreq/gpufreq/load",
];

fn gpu_counters_present() -> bool {
    GPU_COUNTER_PATHS
        .iter()
        .any(|r| Path::new(&format!("{}/{r}", sys())).exists())
}

fn cpuinfo_hardware() -> String {
    if let Ok(text) = fs::read_to_string(format!("{}/cpuinfo", proc())) {
        for line in text.lines() {
            if line.to_lowercase().starts_with("hardware") {
                return line.split_once(':').map(|x| x.1.trim().to_string()).unwrap_or_default();
            }
        }
    }
    String::new()
}

// ---- 温度区 ----

const CPU_ZONE_HINTS: [&str; 6] = ["cpu", "soc", "tsens", "cluster", "big", "little"];
const GPU_ZONE_HINTS: [&str; 5] = ["gpu", "kgsl", "gpuss", "mali", "adreno"];

fn thermal_zones() -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for zone in 0..16 {
        let base = format!("{}/class/thermal/thermal_zone{zone}", sys());
        let mut val = match read_float(&format!("{base}/temp")) {
            Some(v) => v,
            None => continue,
        };
        if val > 1000.0 {
            val /= 1000.0;
        }
        if val > 0.0 && val < 200.0 {
            out.push((read_text(&format!("{base}/type")), (val * 10.0).round() / 10.0));
        }
    }
    out
}

fn pick_zone(zones: &[(String, f64)], hints: &[&str]) -> Option<f64> {
    zones
        .iter()
        .filter(|(name, _)| {
            let n = name.to_lowercase();
            !name.is_empty() && hints.iter().any(|h| n.contains(h))
        })
        .map(|(_, t)| *t)
        .fold(None, |acc, t| Some(acc.map_or(t, |a: f64| a.max(t))))
}

// ============================================================== 指标（动态）

struct SimState {
    cpu: f64,
    npu: f64,
    mem_ratio: f64,
    temp: f64,
    rx: f64,
    tx: f64,
    battery: f64,
    charging: f64,
}

impl Default for SimState {
    fn default() -> Self {
        Self {
            cpu: 28.0,
            npu: 12.0,
            mem_ratio: 0.45,
            temp: 38.0,
            rx: 0.0,
            tx: 0.0,
            battery: 86.0,
            charging: 0.0,
        }
    }
}

pub struct MetricsCollector {
    simulate: bool,
    prev_cpu: Option<(i64, i64)>,
    prev_self: Option<(i64, Instant)>,
    cpu_source: String,
    net_source: String,
    sim_state: SimState,
    sim_started: Instant,
}

impl MetricsCollector {
    pub fn new(simulate: bool) -> Self {
        Self {
            simulate,
            prev_cpu: None,
            prev_self: None,
            cpu_source: "unknown".to_string(),
            net_source: "unknown".to_string(),
            sim_state: SimState::default(),
            sim_started: Instant::now(),
        }
    }

    pub fn collect(&mut self) -> Value {
        if self.simulate {
            self.collect_simulated()
        } else {
            self.collect_real()
        }
    }

    fn collect_real(&mut self) -> Value {
        let battery_raw = Self::termux_battery_raw();

        let (cpu_usage, cpu_source) = self.cpu_usage_real();
        self.cpu_source = cpu_source;

        let mut m = serde_json::Map::new();
        m.insert("cpu_usage".to_string(), cpu_usage.map(Value::from).unwrap_or(Value::Null));

        for (k, v) in self.cpu_mem_real().into_iter() {
            m.insert(k, v);
        }
        for (k, v) in self.storage_real().into_iter() {
            m.insert(k, v);
        }
        for (k, v) in self.net_real().into_iter() {
            m.insert(k, v);
        }
        for (k, v) in self.temp_real(&battery_raw).into_iter() {
            m.insert(k, v);
        }
        m.insert("gpu_usage".to_string(), opt_float(self.gpu_real()));
        m.insert("npu_usage".to_string(), opt_float(self.npu_real()));
        m.insert("power_watts".to_string(), Value::Null);

        let freq = self.cpu_freq_stats();
        let battery = self.battery_real(&battery_raw);
        let (cpu_temp, gpu_temp) = self.cpu_gpu_temp();
        m.insert("cpu_temp".to_string(), opt_float(cpu_temp));
        m.insert("gpu_temp".to_string(), opt_float(gpu_temp));
        m.insert(
            "cpu_freq_mhz".to_string(),
            freq.get("cpu_freq_avg_mhz").cloned().unwrap_or(Value::Null),
        );
        m.insert("gpu_freq_mhz".to_string(), opt_float(self.gpu_freq_mhz()));
        m.insert(
            "battery_percent".to_string(),
            battery.get("battery_percent").cloned().unwrap_or(Value::Null),
        );
        m.insert(
            "battery_temp".to_string(),
            battery.get("battery_temperature").cloned().unwrap_or(Value::Null),
        );
        m.insert(
            "battery_status".to_string(),
            battery.get("battery_status").cloned().unwrap_or(Value::Null),
        );
        m.insert("extra".to_string(), self.extra_real(&freq, &battery));
        Value::Object(m)
    }

    fn extra_real(
        &mut self,
        freq: &serde_json::Map<String, Value>,
        battery: &serde_json::Map<String, Value>,
    ) -> Value {
        let mut extra = serde_json::Map::new();
        extra.insert("cpu_cores".to_string(), Value::from(cpu_count() as i64));
        extra.insert("cpu_source".to_string(), Value::String(self.cpu_source.clone()));
        extra.insert("net_source".to_string(), Value::String(self.net_source.clone()));
        for (k, v) in freq {
            extra.insert(k.clone(), v.clone());
        }
        if let Some(p) = self.cpu_pressure_avg10() {
            extra.insert("cpu_pressure_avg10".to_string(), Value::from(p));
        }
        if let Some(c) = self.agent_self_cpu() {
            extra.insert("agent_cpu_percent".to_string(), Value::from(c));
        }
        for (k, v) in battery {
            extra.insert(k.clone(), v.clone());
        }
        Value::Object(extra)
    }

    fn collect_simulated(&mut self) -> Value {
        let s = &mut self.sim_state;
        let mut rng = rand::thread_rng();
        let mut rng2 = rand::thread_rng();
        let mut r = |lo: f64, hi: f64| rng.gen_range(lo..hi);

        s.cpu = clamp(s.cpu + r(-9.0, 9.0), 5.0, 96.0, Some(28.0));
        if rng2.gen::<f64>() < 0.04 {
            s.cpu = (s.cpu + r(25.0, 45.0)).min(99.0);
        }

        s.npu = clamp(
            s.npu * 0.6 + s.cpu * 0.25 + r(-3.0, 6.0),
            0.0,
            95.0,
            Some(12.0),
        );

        let target = 0.42 + s.cpu / 400.0;
        s.mem_ratio = clamp(s.mem_ratio * 0.9 + target * 0.1, 0.25, 0.92, Some(0.45));

        s.temp = clamp(
            s.temp * 0.93 + (30.0 + s.cpu * 0.28) * 0.07,
            28.0,
            85.0,
            Some(38.0),
        );

        s.rx += r(2_000.0, 90_000.0);
        s.tx += r(1_500.0, 60_000.0);

        if s.charging >= 1.0 {
            s.battery = (s.battery + r(0.15, 0.5)).min(100.0);
            if s.battery >= 95.0 {
                s.charging = 0.0;
            }
        } else {
            s.battery = (s.battery - r(0.05, 0.2)).max(0.0);
            if s.battery <= 25.0 {
                s.charging = 1.0;
            }
        }

        let mem_total: i64 = 12 * 1024 * 1024 * 1024;
        let storage_total: i64 = 256 * 1024 * 1024 * 1024;
        let cores = cpu_count() as i64;
        let charging = s.charging >= 1.0;

        json!({
            "cpu_usage": (s.cpu * 100.0).round() / 100.0,
            "gpu_usage": (clamp(s.cpu * 0.55 + r(-5.0, 8.0), 0.0, 99.0, Some(25.0)) * 100.0).round() / 100.0,
            "npu_usage": (s.npu * 100.0).round() / 100.0,
            "mem_used": (mem_total as f64 * s.mem_ratio) as i64,
            "mem_total": mem_total,
            "storage_used": (storage_total as f64 * 0.62) as i64,
            "storage_total": storage_total,
            "temperature": (s.temp * 10.0).round() / 10.0,
            "cpu_temp": ((s.temp * 1.06).min(95.0) * 10.0).round() / 10.0,
            "gpu_temp": ((s.temp * 0.96).min(95.0) * 10.0).round() / 10.0,
            "cpu_freq_mhz": (1000.0 + s.cpu * 18.0 * 10.0).round() / 10.0,
            "gpu_freq_mhz": (305.0 + s.cpu * 6.2 * 10.0).round() / 10.0,
            "battery_percent": (s.battery * 10.0).round() / 10.0,
            "battery_temp": ((26.0 + s.cpu * 0.12).max(20.0).min(45.0) * 10.0).round() / 10.0,
            "battery_status": if charging { "CHARGING" } else { "DISCHARGING" },
            "power_watts": null,
            "net_rx_bytes": s.rx as i64,
            "net_tx_bytes": s.tx as i64,
            "load_avg": (s.cpu / 100.0 * cores as f64 * 1.1 * 100.0).round() / 100.0,
            "cpu_cores": cores,
            "extra": {
                "simulated": true,
                "sim_uptime": (self.sim_started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                "cpu_cores": cores,
                "cpu_source": "simulated",
                "net_source": "simulated",
                "cpu_freq_avg_mhz": (1000.0 + s.cpu * 18.0 * 10.0).round() / 10.0,
                "battery_percent": (s.battery * 10.0).round() / 10.0,
                "battery_status": if charging { "CHARGING" } else { "DISCHARGING" },
            },
        })
    }

    // ---- 真机各分项 ----

    fn termux_battery_raw() -> String {
        if has_cmd("termux-battery-status") {
            run_cmd(&["termux-battery-status"], 4.0)
        } else {
            String::new()
        }
    }

    fn cpu_usage_real(&mut self) -> (Option<f64>, String) {
        let line = match fs::read_to_string(format!("{}/stat", proc())) {
            Ok(t) => t.lines().next().unwrap_or("").to_string(),
            Err(_) => {
                self.prev_cpu = None;
                return (None, "unavailable".to_string());
            }
        };
        let parts: Vec<i64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|x| x.parse::<i64>().ok())
            .collect();
        if parts.len() < 5 {
            self.prev_cpu = None;
            return (None, "unavailable".to_string());
        }
        let idle = parts[3] + parts[4];
        let total: i64 = parts.iter().sum();
        let value = match self.prev_cpu {
            Some((p_idle, p_total)) => {
                let d_idle = idle - p_idle;
                let d_total = total - p_total;
                if d_total > 0 {
                    let v = 100.0 * (1.0 - d_idle as f64 / d_total as f64);
                    Some((v.max(0.0).min(100.0) * 100.0).round() / 100.0)
                } else {
                    None
                }
            }
            None => None,
        };
        self.prev_cpu = Some((idle, total));
        if value.is_none() {
            return (None, "proc_stat_first_sample".to_string());
        }
        (value, "proc_stat".to_string())
    }

    fn cpu_freq_stats(&self) -> serde_json::Map<String, Value> {
        let mut cur: Vec<f64> = Vec::new();
        let mut mx: Vec<f64> = Vec::new();
        for i in 0..32 {
            let base = format!("{}/devices/system/cpu/cpu{i}/cpufreq", sys());
            let mut c = read_float(&format!("{base}/scaling_cur_freq"));
            if c.is_none() {
                c = read_float(&format!("{base}/cpuinfo_cur_freq"));
            }
            if c.is_none() {
                if !cur.is_empty() {
                    break;
                }
                continue;
            }
            cur.push(c.unwrap());
            let mut m = read_float(&format!("{base}/cpuinfo_max_freq"));
            if m.is_none() {
                m = read_float(&format!("{base}/scaling_max_freq"));
            }
            if let Some(m) = m {
                mx.push(m);
            }
        }
        let mut out = serde_json::Map::new();
        if cur.is_empty() {
            return out;
        }
        let avg = cur.iter().sum::<f64>() / cur.len() as f64;
        out.insert(
            "cpu_freq_avg_mhz".to_string(),
            Value::from((avg / 1000.0 * 10.0).round() / 10.0),
        );
        if !mx.is_empty() {
            let avg_max = mx.iter().sum::<f64>() / mx.len() as f64;
            out.insert(
                "cpu_freq_ratio".to_string(),
                Value::from((100.0 * avg / avg_max * 10.0).round() / 10.0),
            );
        }
        out
    }

    fn cpu_pressure_avg10(&self) -> Option<f64> {
        let text = fs::read_to_string(format!("{}/pressure/cpu", proc())).ok()?;
        let idx = text.find("some")?;
        let rest = &text[idx..];
        let avg = rest.find("avg10=")?;
        let num = &rest[avg + 6..];
        let end = num.find(|c: char| !c.is_ascii_digit() && c != '.')?;
        num[..end].parse::<f64>().ok().map(|v| (v * 100.0).round() / 100.0)
    }

    fn agent_self_cpu(&mut self) -> Option<f64> {
        let raw = fs::read_to_string(format!("{}/self/stat", proc())).ok()?;
        let rp = raw.rfind(')')?;
        let fields: Vec<&str> = raw[rp + 2..].split_whitespace().collect();
        if fields.len() < 13 {
            return None;
        }
        let ticks = fields[11].parse::<i64>().ok()? + fields[12].parse::<i64>().ok()?;
        // 单调时钟等价于 Python 的 time.monotonic()；不能拿 now.elapsed()（恒 ~0）。
        let now = Instant::now();
        let prev = self.prev_self.replace((ticks, now));
        let (p_ticks, p_time) = prev?;
        let d_ticks = ticks - p_ticks;
        let d_time = now.duration_since(p_time).as_secs_f64();
        if d_time <= 0.0 || d_ticks < 0 {
            return None;
        }
        let hz = clock_ticks();
        Some((100.0 * (d_ticks as f64 / hz) / d_time * 100.0).round() / 100.0)
    }

    fn battery_real(&self, battery_raw: &str) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        if !battery_raw.is_empty() {
            if let Some(pct) = extract_number(battery_raw, "\"percentage\"") {
                out.insert("battery_percent".to_string(), Value::from(pct));
            }
            if let Some(st) = extract_string(battery_raw, "\"status\"") {
                out.insert("battery_status".to_string(), Value::String(st));
            }
            if let Some(temp) = extract_number(battery_raw, "\"temperature\"") {
                out.insert("battery_temperature".to_string(), Value::from(temp));
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(pct) = read_float(&format!("{}/class/power_supply/battery/capacity", sys())) {
            out.insert("battery_percent".to_string(), Value::from(pct));
        }
        let status = read_text(&format!("{}/class/power_supply/battery/status", sys()));
        if !status.is_empty() {
            out.insert("battery_status".to_string(), Value::String(status));
        }
        out
    }

    fn cpu_mem_real(&self) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        let mut total: Option<i64> = None;
        let mut avail: Option<i64> = None;
        if let Ok(text) = fs::read_to_string(format!("{}/meminfo", proc())) {
            let mut mem: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
            for line in text.lines() {
                let (k, v) = match line.split_once(':') {
                    Some(x) => x,
                    None => continue,
                };
                let num = v
                    .split_whitespace()
                    .next()
                    .and_then(|x| x.parse::<i64>().ok());
                if let Some(n) = num {
                    mem.insert(k.trim().to_string(), n * 1024);
                }
            }
            total = mem.get("MemTotal").copied();
            avail = mem.get("MemAvailable").copied();
            if avail.is_none() {
                avail = Some(
                    mem.get("MemFree").copied().unwrap_or(0)
                        + mem.get("Cached").copied().unwrap_or(0)
                        + mem.get("Buffers").copied().unwrap_or(0),
                );
            }
        }
        if let Some(t) = total {
            out.insert("mem_total".to_string(), Value::from(t));
            out.insert("mem_used".to_string(), Value::from((t - avail.unwrap_or(0)).max(0)));
        } else {
            out.insert("mem_total".to_string(), Value::Null);
            out.insert("mem_used".to_string(), Value::Null);
        }

        let load = fs::read_to_string(format!("{}/loadavg", proc()))
            .ok()
            .and_then(|t| {
                t.split_whitespace()
                    .next()
                    .and_then(|x| x.parse::<f64>().ok())
            })
            .map(|v| (v * 100.0).round() / 100.0);
        out.insert("load_avg".to_string(), load.map(Value::from).unwrap_or(Value::Null));
        out.insert("cpu_cores".to_string(), Value::from(cpu_count() as i64));
        out
    }

    fn storage_real(&self) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
        for path in ["/data", home.as_str()] {
            if let Some((total, free)) = fs_space(path) {
                if total > 0 {
                    out.insert("storage_total".to_string(), Value::from(total));
                    out.insert(
                        "storage_used".to_string(),
                        Value::from((total - free).max(0)),
                    );
                    return out;
                }
            }
        }
        out.insert("storage_total".to_string(), Value::Null);
        out.insert("storage_used".to_string(), Value::Null);
        out
    }

    fn net_real(&mut self) -> serde_json::Map<String, Value> {
        let readers: [fn() -> (Option<i64>, Option<i64>); 3] = [
            || parse_net_dev(&format!("{}/net/dev", proc())),
            || parse_net_dev(&format!("{}/self/net/dev", proc())),
            || net_from_sysfs(),
        ];
        let sources = ["proc_net_dev", "proc_self_net_dev", "sysfs_iface"];
        for (reader, source) in readers.iter().zip(sources.iter()) {
            if let (Some(rx), Some(tx)) = reader() {
                self.net_source = source.to_string();
                let mut out = serde_json::Map::new();
                out.insert("net_rx_bytes".to_string(), Value::from(rx));
                out.insert("net_tx_bytes".to_string(), Value::from(tx));
                return out;
            }
        }
        self.net_source = "unavailable".to_string();
        let mut out = serde_json::Map::new();
        out.insert("net_rx_bytes".to_string(), Value::Null);
        out.insert("net_tx_bytes".to_string(), Value::Null);
        out
    }

    fn temp_real(&self, battery_raw: &str) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        if !battery_raw.is_empty() {
            if let Some(t) = extract_number(battery_raw, "\"temperature\"") {
                out.insert("temperature".to_string(), Value::from(t));
                return out;
            }
        }
        let zones = thermal_zones();
        if !zones.is_empty() {
            let max = zones.iter().map(|(_, t)| *t).fold(f64::MIN, f64::max);
            out.insert("temperature".to_string(), Value::from(max));
        } else {
            out.insert("temperature".to_string(), Value::Null);
        }
        out
    }

    fn cpu_gpu_temp(&self) -> (Option<f64>, Option<f64>) {
        let zones = thermal_zones();
        (
            pick_zone(&zones, &CPU_ZONE_HINTS),
            pick_zone(&zones, &GPU_ZONE_HINTS),
        )
    }

    fn gpu_freq_mhz(&self) -> Option<f64> {
        for rel in [
            "class/devfreq/gpufreq/cur_freq",
            "class/kgsl/kgsl-3d0/devfreq/cur_freq",
            "class/kgsl/kgsl-3d0/gpuclk",
        ] {
            if let Some(hz) = read_float(&format!("{}/{rel}", sys())) {
                if hz > 0.0 {
                    return Some(if hz > 10_000.0 {
                        (hz / 1_000_000.0 * 10.0).round() / 10.0
                    } else {
                        hz
                    });
                }
            }
        }
        None
    }

    fn gpu_real(&self) -> Option<f64> {
        for rel in GPU_COUNTER_PATHS {
            let p = format!("{}/{rel}", sys());
            let raw = match fs::read_to_string(&p) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let nums: Vec<f64> = raw
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<f64>().ok())
                .take(2)
                .collect();
            if nums.is_empty() {
                continue;
            }
            if nums.len() == 2 {
                if nums[1] > 0.0 {
                    return Some((100.0 * nums[0] / nums[1] * 100.0).round() / 100.0);
                }
                continue;
            }
            let v = nums[0];
            if v <= 100.0 {
                return Some((v * 100.0).round() / 100.0);
            }
            return Some((v / 10.0 * 100.0).round() / 100.0);
        }
        None
    }

    fn npu_real(&self) -> Option<f64> {
        for p in [format!("{}/kernel/npu/load", sys()), format!("{}/class/npu/npu_busy", sys())] {
            if let Ok(raw) = fs::read_to_string(&p) {
                if let Some(first) = raw
                    .split(|c: char| !c.is_ascii_digit() && c != '.')
                    .find(|s| !s.is_empty())
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    return Some((first * 100.0).round() / 100.0);
                }
            }
        }
        None
    }
}

// ------------------------------------------------------------------ 工具

fn clamp(v: f64, lo: f64, hi: f64, center: Option<f64>) -> f64 {
    let v = if let Some(c) = center { v * 0.97 + c * 0.03 } else { v };
    v.max(lo).min(hi)
}

fn opt_float(v: Option<f64>) -> Value {
    v.map(Value::from).unwrap_or(Value::Null)
}

fn cpu_count() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

#[cfg(unix)]
fn clock_ticks() -> f64 {
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 }.max(1.0)
}

#[cfg(not(unix))]
fn clock_ticks() -> f64 {
    100.0
}

fn parse_net_dev(path: &str) -> (Option<i64>, Option<i64>) {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (None, None),
    };
    let mut rx: i64 = 0;
    let mut tx: i64 = 0;
    let mut seen = false;
    for line in text.lines() {
        if !line.contains(':') {
            continue;
        }
        let (iface, data) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let iface = iface.trim();
        if iface == "lo" || iface.starts_with("dummy") || iface.starts_with("ifb") {
            continue;
        }
        let cols: Vec<&str> = data.split_whitespace().collect();
        if cols.len() >= 9 {
            if let (Ok(r), Ok(t)) = (cols[0].parse::<i64>(), cols[8].parse::<i64>()) {
                rx += r;
                tx += t;
                seen = true;
            }
        }
    }
    if seen {
        (Some(rx), Some(tx))
    } else {
        (None, None)
    }
}

fn net_from_sysfs() -> (Option<i64>, Option<i64>) {
    let base = format!("{}/class/net", sys());
    let dir = match fs::read_dir(&base) {
        Ok(d) => d,
        Err(_) => return (None, None),
    };
    let mut rx: i64 = 0;
    let mut tx: i64 = 0;
    let mut seen = false;
    for entry in dir.flatten() {
        let iface = entry.file_name().to_string_lossy().to_string();
        if iface == "lo" {
            continue;
        }
        let rp = entry.path().join("statistics/rx_bytes");
        let tp = entry.path().join("statistics/tx_bytes");
        let r = read_i64_file(&rp);
        let t = read_i64_file(&tp);
        if let (Some(r), Some(t)) = (r, t) {
            rx += r;
            tx += t;
            seen = true;
        }
    }
    if seen {
        (Some(rx), Some(tx))
    } else {
        (None, None)
    }
}

fn read_i64_file(path: &PathBuf) -> Option<i64> {
    let txt = fs::read_to_string(path).ok()?;
    txt.split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as i64)
}

/// 磁盘空间（total, free）。仅 unix 可用，非 unix 返回 None。
fn fs_space(path: &str) -> Option<(i64, i64)> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let c = CString::new(path).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
        if rc != 0 {
            return None;
        }
        let total = st.f_blocks as i64 * st.f_frsize as i64;
        let free = st.f_bavail as i64 * st.f_frsize as i64;
        return Some((total, free));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// 从 JSON 文本里提取 `"key": 数字` 的数值（对应 Python 的正则）。
fn extract_number(text: &str, key: &str) -> Option<f64> {
    let idx = text.find(key)?;
    let rest = &text[idx + key.len()..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(rest.len());
    rest[..end].trim().parse::<f64>().ok().map(|v| (v * 10.0).round() / 10.0)
}

/// 从 JSON 文本里提取 `"key": "字符串"` 的值。
fn extract_string(text: &str, key: &str) -> Option<String> {
    let idx = text.find(key)?;
    let rest = &text[idx + key.len()..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    //! 对应 docs/acceptance/agent_metrics_check.py 的 A–G 段。
    //!
    //! 真机上 `/proc/stat`、`/proc/net/dev` 会被 Android SELinux 封掉，开发机上没法
    //! 自然复现，所以把 `/proc`、`/sys` 指向合成目录（`set_paths`），按场景摆出不同
    //! 能读到的文件，断言降级链的每一步——尤其是"拿不到就 None，不造假"。

    use super::*;
    use crate::config::SimulateCfg;
    use serde_json::Value;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    struct Results {
        failures: Vec<String>,
    }
    impl Results {
        fn new() -> Self {
            Self { failures: vec![] }
        }
        fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
            if !ok {
                self.failures.push(format!("{name} — {}", detail.as_ref()));
            }
        }
        fn finish(self) {
            assert!(
                self.failures.is_empty(),
                "\nAgent 指标降级链验收失败 {} 项：\n{}",
                self.failures.len(),
                self.failures.join("\n")
            );
        }
    }

    fn w(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn cpu_stat(user: i64, idle: i64) -> String {
        format!("cpu {user} 0 {user} {idle} 0 0 0 0\ncpu0 {user} 0 {user} {idle} 0 0 0 0\n")
    }

    fn meminfo() -> String {
        "MemTotal:       12000000 kB\nMemFree:         2000000 kB\nMemAvailable:    4500000 kB\nBuffers:          100000 kB\nCached:           500000 kB\n".to_string()
    }

    /// comm 里故意放一个空格（`(python test)`），解析必须从**最后一个右括号**之后开始。
    fn self_stat(utime: i64, stime: i64) -> String {
        format!("1234 (python test) S 1 2 3 4 5 6 7 8 9 10 {utime} {stime} 0 0 0 0 0 0\n")
    }

    fn build_tree(root: &Path, proc_stat: Option<&str>, freq: bool, thermal: bool) {
        let proc = root.join("proc");
        let sys_dir = root.join("sys");
        fs::create_dir_all(&proc).unwrap();
        fs::create_dir_all(&sys_dir).unwrap();
        if let Some(ps) = proc_stat {
            w(&proc.join("stat"), ps);
            w(&proc.join("loadavg"), "0.42 0.51 0.48 2/512 12345\n");
        }
        w(&proc.join("meminfo"), &meminfo());
        w(&proc.join("self").join("stat"), &self_stat(1000, 500));
        if freq {
            for i in [0i64, 4i64] {
                w(
                    &sys_dir.join(format!("devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq")),
                    "1800000\n",
                );
                w(
                    &sys_dir.join(format!("devices/system/cpu/cpu{i}/cpufreq/cpuinfo_max_freq")),
                    "2400000\n",
                );
            }
        }
        if thermal {
            w(&sys_dir.join("class/thermal/thermal_zone0/temp"), "37000\n");
        }
    }

    fn point_at(root: &Path) {
        set_paths(
            Some(root.join("proc").to_string_lossy().to_string()),
            Some(root.join("sys").to_string_lossy().to_string()),
        );
    }

    fn as_f64(v: &Value, key: &str) -> Option<f64> {
        v.get(key).and_then(|x| x.as_f64())
    }
    fn extra_str(v: &Value, key: &str) -> String {
        v.get("extra")
            .and_then(|e| e.get(key))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    }
    fn extra_f64(v: &Value, key: &str) -> Option<f64> {
        v.get("extra").and_then(|e| e.get(key)).and_then(|x| x.as_f64())
    }

    #[test]
    fn agent_metrics_degradation_chain() {
        let mut r = Results::new();
        let base = std::env::temp_dir().join(format!(
            "edge-agent-metrics-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);

        // ---- A 完整 Linux：能算出真 CPU% ----
        {
            let root_a = base.join("a");
            build_tree(&root_a, Some(&cpu_stat(100, 800)), true, true);
            point_at(&root_a);
            let mut c = MetricsCollector::new(false);
            let first = c.collect();
            r.check(
                "首次采样 cpu_usage 为 None（差值还不够算）",
                as_f64(&first, "cpu_usage").is_none(),
                format!("{:?}", first.get("cpu_usage")),
            );
            r.check(
                "首次采样来源标注为 proc_stat_first_sample",
                extra_str(&first, "cpu_source") == "proc_stat_first_sample",
                extra_str(&first, "cpu_source"),
            );

            w(&root_a.join("proc").join("stat"), &cpu_stat(200, 1400));
            let second = c.collect();
            r.check(
                "第二次采样 cpu_usage == 25.0",
                as_f64(&second, "cpu_usage") == Some(25.0),
                format!("{:?}", second.get("cpu_usage")),
            );
            r.check(
                "来源标注为 proc_stat",
                extra_str(&second, "cpu_source") == "proc_stat",
                extra_str(&second, "cpu_source"),
            );
            r.check(
                "内存总量解析正确（1200 万 kB → 字节）",
                as_f64(&second, "mem_total") == Some(12_000_000.0 * 1024.0),
                format!("{:?}", second.get("mem_total")),
            );
            let used = as_f64(&second, "mem_used");
            let total = as_f64(&second, "mem_total");
            r.check(
                "可用内存正确扣减（used < total）",
                matches!((used, total), (Some(u), Some(t)) if u > 0.0 && u < t),
                format!("used={used:?}"),
            );
            r.check(
                "loadavg 可读时有值",
                as_f64(&second, "load_avg").is_some(),
                format!("{:?}", second.get("load_avg")),
            );
        }

        // ---- B Android 封锁：/proc/stat 读不到 → 必须留空，不造假 ----
        {
            let root_b = base.join("b");
            build_tree(&root_b, None, true, true);
            point_at(&root_b);
            let mut c = MetricsCollector::new(false);
            c.collect();
            let b = c.collect();
            r.check(
                "cpu_usage 为 None（不拿替代指标冒充）",
                as_f64(&b, "cpu_usage").is_none(),
                format!("{:?}", b.get("cpu_usage")),
            );
            r.check(
                "来源标注为 unavailable",
                extra_str(&b, "cpu_source") == "unavailable",
                extra_str(&b, "cpu_source"),
            );
            r.check(
                "load_avg 同步为 None（同一批被封的文件）",
                as_f64(&b, "load_avg").is_none(),
                format!("{:?}", b.get("load_avg")),
            );
            r.check(
                "替代指标 cpu_freq_avg_mhz 可用（1800MHz）",
                extra_f64(&b, "cpu_freq_avg_mhz") == Some(1800.0),
                format!("{:?}", extra_f64(&b, "cpu_freq_avg_mhz")),
            );
            r.check(
                "替代指标 cpu_freq_ratio 可用（1800/2400 = 75%）",
                extra_f64(&b, "cpu_freq_ratio") == Some(75.0),
                format!("{:?}", extra_f64(&b, "cpu_freq_ratio")),
            );
            r.check(
                "内存不受 CPU 封锁影响，仍然有值",
                as_f64(&b, "mem_total").is_some(),
                format!("{:?}", b.get("mem_total")),
            );

            // 进程自身 CPU：真机上唯一稳的 CPU 真值
            std::thread::sleep(Duration::from_millis(250));
            w(&root_b.join("proc").join("self").join("stat"), &self_stat(1025, 500));
            let b2 = c.collect();
            let selfcpu = extra_f64(&b2, "agent_cpu_percent");
            r.check(
                "agent_cpu_percent 可算（读自己的进程不受限）",
                matches!(selfcpu, Some(v) if v > 0.0 && v < 1000.0),
                format!("{selfcpu:?}"),
            );
        }

        // ---- C 网络三级降级 ----
        {
            let root_c = base.join("c");
            build_tree(&root_c, None, false, false);
            point_at(&root_c);
            let mut c = MetricsCollector::new(false);
            c.collect();
            let c3 = c.collect();
            r.check(
                "全无网络来源时 net 为 None + unavailable",
                as_f64(&c3, "net_rx_bytes").is_none()
                    && extra_str(&c3, "net_source") == "unavailable",
                extra_str(&c3, "net_source"),
            );

            w(
                &root_c.join("proc").join("self").join("net").join("dev"),
                "Inter-|   Receive                                                |  Transmit\n\
                 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets\n\
                 wlan0: 111111 0 0 0 0 0 0 0 222222 0 0 0 0 0 0 0\n",
            );
            let c4 = c.collect();
            r.check(
                "第二级 /proc/self/net/dev 生效且数值正确",
                as_f64(&c4, "net_rx_bytes") == Some(111111.0)
                    && as_f64(&c4, "net_tx_bytes") == Some(222222.0)
                    && extra_str(&c4, "net_source") == "proc_self_net_dev",
                format!(
                    "{:?}/{:?} src={}",
                    as_f64(&c4, "net_rx_bytes"),
                    as_f64(&c4, "net_tx_bytes"),
                    extra_str(&c4, "net_source")
                ),
            );

            w(
                &root_c.join("proc").join("net").join("dev"),
                "wlan0: 999999 0 0 0 0 0 0 0 888888 0 0 0 0 0 0 0\n",
            );
            let c5 = c.collect();
            r.check(
                "第一级 /proc/net/dev 优先于第二级",
                as_f64(&c5, "net_rx_bytes") == Some(999999.0)
                    && extra_str(&c5, "net_source") == "proc_net_dev",
                format!("src={}", extra_str(&c5, "net_source")),
            );

            let root_d = base.join("d");
            build_tree(&root_d, None, false, false);
            point_at(&root_d);
            w(
                &root_d.join("sys/class/net/wlan0/statistics/rx_bytes"),
                "123456\n",
            );
            w(
                &root_d.join("sys/class/net/wlan0/statistics/tx_bytes"),
                "654321\n",
            );
            let mut c6 = MetricsCollector::new(false);
            c6.collect();
            c6.collect();
            let cd = c6.collect();
            r.check(
                "第三级 sysfs 生效",
                as_f64(&cd, "net_rx_bytes") == Some(123456.0)
                    && extra_str(&cd, "net_source") == "sysfs_iface",
                format!(
                    "{:?} src={}",
                    as_f64(&cd, "net_rx_bytes"),
                    extra_str(&cd, "net_source")
                ),
            );
        }

        // ---- D 契约一致性：新字段落 extra，主字段保持 0~100 语义 ----
        {
            let root_a2 = base.join("a2");
            build_tree(&root_a2, Some(&cpu_stat(100, 800)), true, false);
            point_at(&root_a2);
            let mut c = MetricsCollector::new(false);
            c.collect();
            let d = c.collect();
            let has_cpu_src = d.get("extra").and_then(|e| e.get("cpu_source")).is_some();
            let has_net_src = d.get("extra").and_then(|e| e.get("net_source")).is_some();
            r.check(
                "cpu_source / net_source 在 extra 内，不污染主字段",
                has_cpu_src && has_net_src,
                "",
            );
            r.check(
                "cpu_usage 仍是 0~100 的百分比（或 None）",
                match as_f64(&d, "cpu_usage") {
                    Some(v) => (0.0..=100.0).contains(&v),
                    None => true,
                },
                format!("{:?}", d.get("cpu_usage")),
            );
        }

        // ---- E 一等字段：分项温度 / GPU 频率 / 电池进主字段 ----
        {
            let root_e = base.join("e");
            build_tree(&root_e, Some(&cpu_stat(100, 800)), true, false);
            point_at(&root_e);
            let sys_e = root_e.join("sys");
            w(&sys_e.join("class/thermal/thermal_zone0/temp"), "37000");
            w(&sys_e.join("class/thermal/thermal_zone1/type"), "cpu-0-0");
            w(&sys_e.join("class/thermal/thermal_zone1/temp"), "41000");
            w(&sys_e.join("class/thermal/thermal_zone2/type"), "gpu-thermal");
            w(&sys_e.join("class/thermal/thermal_zone2/temp"), "39000");
            w(&sys_e.join("class/devfreq/gpufreq/cur_freq"), "490000000");
            w(&sys_e.join("class/power_supply/battery/capacity"), "77");
            w(&sys_e.join("class/power_supply/battery/status"), "Discharging");
            let mut c = MetricsCollector::new(false);
            let e = c.collect();
            r.check(
                "temperature 取全部热区最高者（41.0，含无标签 zone0）",
                as_f64(&e, "temperature") == Some(41.0),
                format!("{:?}", e.get("temperature")),
            );
            r.check(
                "cpu_temp 按 type 标签命中（41.0）",
                as_f64(&e, "cpu_temp") == Some(41.0),
                format!("{:?}", e.get("cpu_temp")),
            );
            r.check(
                "gpu_temp 按 type 标签命中（39.0）",
                as_f64(&e, "gpu_temp") == Some(39.0),
                format!("{:?}", e.get("gpu_temp")),
            );
            r.check(
                "cpu_freq_mhz 进主字段且与 extra 同源（1800.0）",
                as_f64(&e, "cpu_freq_mhz") == Some(1800.0)
                    && extra_f64(&e, "cpu_freq_avg_mhz") == Some(1800.0),
                format!("{:?}", e.get("cpu_freq_mhz")),
            );
            r.check(
                "gpu_freq_mhz 由 Hz 换算成 MHz（490.0）",
                as_f64(&e, "gpu_freq_mhz") == Some(490.0),
                format!("{:?}", e.get("gpu_freq_mhz")),
            );
            r.check(
                "battery_percent / battery_status 进主字段",
                as_f64(&e, "battery_percent") == Some(77.0)
                    && e.get("battery_status").and_then(|x| x.as_str()) == Some("Discharging"),
                format!(
                    "{:?}/{:?}",
                    e.get("battery_percent"),
                    e.get("battery_status")
                ),
            );
        }

        // ---- F 拿不到 ≠ 0：一等字段读不到时必须是 None ----
        {
            let root_f = base.join("f");
            build_tree(&root_f, None, false, false);
            point_at(&root_f);
            let f = MetricsCollector::new(false).collect();
            for key in [
                "cpu_temp",
                "gpu_temp",
                "cpu_freq_mhz",
                "gpu_freq_mhz",
                "battery_percent",
                "battery_temp",
                "battery_status",
            ] {
                let is_null = f.get(key).map(|x| x.is_null()).unwrap_or(false);
                r.check(
                    &format!("{key} 为 None（而不是 0 / 空串）"),
                    is_null,
                    format!("{:?}", f.get(key)),
                );
            }
            for key in [
                "cpu_temp",
                "gpu_temp",
                "gpu_freq_mhz",
                "battery_percent",
                "battery_temp",
                "battery_status",
            ] {
                r.check(&format!("{key} 键存在（值可为 None）"), f.get(key).is_some(), "");
            }

            // 只有无标签热区时：机身温度有值，分项温度必须留空（不按编号猜）
            let root_g = base.join("g");
            build_tree(&root_g, None, false, true);
            point_at(&root_g);
            let g = MetricsCollector::new(false).collect();
            r.check(
                "热区无 type 标签 → 机身温度有值（37.0）而 cpu_temp 为 None",
                as_f64(&g, "temperature") == Some(37.0) && as_f64(&g, "cpu_temp").is_none(),
                format!(
                    "temperature={:?} cpu_temp={:?}",
                    g.get("temperature"),
                    g.get("cpu_temp")
                ),
            );
        }

        // ---- G 设备能力：capabilities / is_root / qnn_runtime 三态 ----
        {
            // 复用 root_e（含热区 + /proc/stat，但没有 net/dev）
            let root_e = base.join("e");
            point_at(&root_e);
            let info = device_info(false, &SimulateCfg::default());
            let caps = info.get("capabilities").cloned().unwrap_or(Value::Null);
            r.check(
                "capabilities.can_proc_stat=True",
                caps.get("can_proc_stat").and_then(|x| x.as_bool()) == Some(true),
                format!("{:?}", caps.get("can_proc_stat")),
            );
            r.check(
                "capabilities.can_thermal=True（有热区）",
                caps.get("can_thermal").and_then(|x| x.as_bool()) == Some(true),
                format!("{:?}", caps.get("can_thermal")),
            );
            r.check(
                "capabilities.can_proc_net=False（没有 net/dev）",
                caps.get("can_proc_net").and_then(|x| x.as_bool()) == Some(false),
                format!("{:?}", caps.get("can_proc_net")),
            );
            r.check(
                "is_root 落在三态 {-1,0,1}",
                matches!(info.get("is_root").and_then(|x| x.as_i64()), Some(v) if v == -1 || v == 0 || v == 1),
                format!("{:?}", info.get("is_root")),
            );
            r.check(
                "qnn_runtime 落在三态 {-1,0,1}",
                matches!(caps.get("qnn_runtime").and_then(|x| x.as_i64()), Some(v) if v == -1 || v == 0 || v == 1),
                format!("{:?}", caps.get("qnn_runtime")),
            );
            r.check(
                "型号字段缺失时一律 Unknown，不编造",
                ["model", "soc", "cpu", "gpu", "npu"]
                    .iter()
                    .all(|k| info.get(*k).and_then(|x| x.as_str()) == Some("Unknown")),
                format!(
                    "{:?}",
                    ["model", "soc", "cpu"]
                        .iter()
                        .map(|k| (*k, info.get(*k).cloned()))
                        .collect::<Vec<_>>()
                ),
            );
        }

        set_paths(None, None);
        let _ = fs::remove_dir_all(&base);
        r.finish();
    }
}
