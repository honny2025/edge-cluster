//! 工具：结构化日志（带大小滚动）+ 通用小工具。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;
use log::{LevelFilter, Log, Metadata, Record};

/// 大小滚动日志器：同时输出到控制台与文件，文件按 max_bytes 滚动。
struct RotatingLogger {
    level: LevelFilter,
    path: PathBuf,
    max_bytes: u64,
    backup_count: usize,
    file: Mutex<Option<File>>,
}

impl RotatingLogger {
    fn open(&self) -> io::Result<File> {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        OpenOptions::new().create(true).append(true).open(&self.path)
    }

    fn rotate(&self, file: &mut Option<File>) {
        if let Some(f) = file.take() {
            drop(f);
        }
        // agent.log.N <- ... <- agent.log.1 <- agent.log
        for i in (1..=self.backup_count).rev() {
            let src = if i == 1 {
                self.path.clone()
            } else {
                self.path.with_extension(format!("log.{}", i - 1))
            };
            let dst = self.path.with_extension(format!("log.{}", i));
            if src.exists() {
                let _ = fs::remove_file(&dst);
                let _ = fs::rename(&src, &dst);
            }
        }
        *file = self.open().ok();
    }
}

impl Log for RotatingLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!(
            "{} | {:<7} | {:<16} | {}\n",
            ts,
            record.level(),
            record.target(),
            record.args()
        );
        let mut stdout = io::stdout();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.flush();

        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_none() {
            *guard = self.open().ok();
        }
        let wrote = if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
            f.metadata().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        if wrote >= self.max_bytes {
            self.rotate(&mut guard);
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

/// 初始化日志（进程内只做一次）。返回日志文件路径。
pub fn setup_logging(
    log_dir: &PathBuf,
    level: &str,
    max_bytes: i64,
    backup_count: usize,
    tag: &str,
) -> PathBuf {
    let log_file = log_dir.join(format!("{tag}.log"));
    let level = match level.to_ascii_uppercase().as_str() {
        "DEBUG" => LevelFilter::Debug,
        "WARNING" | "WARN" => LevelFilter::Warn,
        "ERROR" => LevelFilter::Error,
        "TRACE" => LevelFilter::Trace,
        "OFF" => LevelFilter::Off,
        _ => LevelFilter::Info,
    };
    let logger = RotatingLogger {
        level,
        path: log_file.clone(),
        max_bytes: (max_bytes.max(1024)) as u64,
        backup_count: backup_count.max(0),
        file: Mutex::new(None),
    };
    // 忽略重复初始化错误：日志器已由首次调用安装。
    let leaked: &'static RotatingLogger = Box::leak(Box::new(logger));
    let _ = log::set_logger(leaked);
    log::set_max_level(level);
    log_file
}

/// 字节转可读字符串，用于日志。
pub fn human_bytes(n: Option<i64>) -> String {
    match n {
        None => "N/A".to_string(),
        Some(n) => {
            let mut v = n as f64;
            for unit in ["B", "KB", "MB", "GB", "TB"] {
                if v.abs() < 1024.0 {
                    return format!("{v:.1}{unit}");
                }
                v /= 1024.0;
            }
            format!("{v:.1}PB")
        }
    }
}

/// 当前 epoch 秒（对应 Python `time.time()`）。
pub fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 进程 id（对应 Python `os.getpid()`）。
pub fn pid() -> i64 {
    std::process::id() as i64
}
