use crate::config::{local_cache_dir, LoggingConfig};
use chrono::Local;
use parking_lot::Mutex;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Off = 5,
}

impl LogLevel {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" | "debug" => Self::Debug,
            "info" => Self::Info,
            "warn" | "warning" => Self::Warn,
            "error" => Self::Error,
            "off" | "none" | "false" => Self::Off,
            _ => Self::Info,
        }
    }

    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Debug,
            2 => Self::Info,
            3 => Self::Warn,
            4 => Self::Error,
            _ => Self::Off,
        }
    }

    pub fn as_tag(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO ",
            Self::Warn => "WARN ",
            Self::Error => "ERROR",
            Self::Off => "OFF  ",
        }
    }
}

struct FileLoggerInner {
    log_dir: PathBuf,
    active_path: PathBuf,
    file: Option<File>,
    current_bytes: u64,
    max_file_bytes: u64,
    max_backups: usize,
}

pub struct GlobalLogger {
    enabled: AtomicBool,
    min_level: AtomicU8,
    inner: Mutex<FileLoggerInner>,
}

static LOGGER: OnceLock<GlobalLogger> = OnceLock::new();

pub fn default_log_dir() -> PathBuf {
    let dir = local_cache_dir().join("logs");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn active_log_file_path() -> PathBuf {
    if let Some(logger) = LOGGER.get() {
        return logger.inner.lock().active_path.clone();
    }
    default_log_dir().join("zettel-quick.log")
}

/// 在进程最早期初始化全局日志文件与未捕获 Panic 钩子
pub fn init_early_logger() {
    let _ = global_logger();
    install_panic_hook();
}

fn global_logger() -> &'static GlobalLogger {
    LOGGER.get_or_init(|| {
        let log_dir = std::env::var("ZETTEL_LOG_DIR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(default_log_dir);

        let _ = fs::create_dir_all(&log_dir);
        let active_path = log_dir.join("zettel-quick.log");
        let (file, current_bytes) = open_append_file(&active_path);

        GlobalLogger {
            enabled: AtomicBool::new(true),
            min_level: AtomicU8::new(LogLevel::Info as u8),
            inner: Mutex::new(FileLoggerInner {
                log_dir,
                active_path,
                file,
                current_bytes,
                max_file_bytes: 5 * 1024 * 1024,
                max_backups: 3,
            }),
        }
    })
}

/// 根据加载或热重载后的 LoggingConfig 动态调整日志级别、开关与轮转阈值
pub fn apply_logging_config(cfg: &LoggingConfig) {
    let logger = global_logger();
    logger.enabled.store(cfg.enabled, Ordering::Relaxed);
    let level = if cfg.enabled {
        LogLevel::from_str_lossy(&cfg.level)
    } else {
        LogLevel::Off
    };
    logger.min_level.store(level as u8, Ordering::Relaxed);

    let max_bytes = cfg.max_file_size_mb.clamp(1, 100) * 1024 * 1024;
    let max_backups = cfg.max_files.clamp(1, 10);

    let mut guard = logger.inner.lock();
    guard.max_file_bytes = max_bytes;
    guard.max_backups = max_backups;
}

fn open_append_file(path: &Path) -> (Option<File>, u64) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let existing_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok();
    (file, existing_len)
}

fn rotate_files_in_dir(log_dir: &Path, active_path: &Path, max_backups: usize) {
    if max_backups == 0 {
        let _ = fs::remove_file(active_path);
        return;
    }

    let oldest = log_dir.join(format!("zettel-quick.{max_backups}.log"));
    let _ = fs::remove_file(&oldest);

    for idx in (1..max_backups).rev() {
        let src = log_dir.join(format!("zettel-quick.{idx}.log"));
        let dst = log_dir.join(format!("zettel-quick.{}.log", idx + 1));
        if src.exists() {
            let _ = fs::rename(&src, &dst);
        }
    }

    let first_backup = log_dir.join("zettel-quick.1.log");
    if active_path.exists() {
        let _ = fs::rename(active_path, &first_backup);
    }
}

pub fn write_log(level: LogLevel, target: &str, msg: &str) {
    let logger = global_logger();
    if !logger.enabled.load(Ordering::Relaxed) {
        return;
    }
    let min_lvl = LogLevel::from_u8(logger.min_level.load(Ordering::Relaxed));
    if level < min_lvl || level == LogLevel::Off {
        return;
    }

    let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let sanitized_msg = msg.replace('\r', "").replace('\n', " \\n ");
    let line = format!("{now} [{}] [{target}] {sanitized_msg}\n", level.as_tag());

    #[cfg(debug_assertions)]
    {
        eprint!("{line}");
    }

    let mut guard = logger.inner.lock();
    write_line_with_rotation(&mut guard, line.as_bytes(), level >= LogLevel::Warn);
}

fn write_line_with_rotation(inner: &mut FileLoggerInner, bytes: &[u8], force_flush: bool) {
    if inner.current_bytes + (bytes.len() as u64) > inner.max_file_bytes {
        // 先关闭当前句柄，以便 Windows 允许文件重命名
        if let Some(mut f) = inner.file.take() {
            let _ = f.flush();
        }
        rotate_files_in_dir(&inner.log_dir, &inner.active_path, inner.max_backups);
        let (new_file, len) = open_append_file(&inner.active_path);
        inner.file = new_file;
        inner.current_bytes = len;
    }

    if inner.file.is_none() {
        let (new_file, len) = open_append_file(&inner.active_path);
        inner.file = new_file;
        inner.current_bytes = len;
    }

    if let Some(file) = inner.file.as_mut() {
        if file.write_all(bytes).is_ok() {
            inner.current_bytes += bytes.len() as u64;
            if force_flush {
                let _ = file.flush();
            }
        }
    }
}

pub fn flush_logs() {
    if let Some(logger) = LOGGER.get() {
        if let Some(file) = logger.inner.lock().file.as_mut() {
            let _ = file.flush();
        }
    }
}

fn install_panic_hook() {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());

        let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };

        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        write_log(
            LogLevel::Error,
            "PANIC",
            &format!("Unhandled Rust panic on thread '{thread_name}' at {location}: {payload}"),
        );
        flush_logs();
        prev_hook(panic_info);
    }));
}

#[macro_export]
macro_rules! log_debug {
    ($target:expr, $($arg:tt)*) => {
        $crate::logger::write_log($crate::logger::LogLevel::Debug, $target, &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($target:expr, $($arg:tt)*) => {
        $crate::logger::write_log($crate::logger::LogLevel::Info, $target, &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($target:expr, $($arg:tt)*) => {
        $crate::logger::write_log($crate::logger::LogLevel::Warn, $target, &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($target:expr, $($arg:tt)*) => {
        $crate::logger::write_log($crate::logger::LogLevel::Error, $target, &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_rolls_over_files_within_backup_limit() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "zettel_logger_test_{}",
            Local::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).unwrap();

        let active_path = tmp_dir.join("zettel-quick.log");
        let (file, current_bytes) = open_append_file(&active_path);
        let mut inner = FileLoggerInner {
            log_dir: tmp_dir.clone(),
            active_path: active_path.clone(),
            file,
            current_bytes,
            max_file_bytes: 120, // 极小阈值触发多次轮转
            max_backups: 2,
        };

        for idx in 0..8 {
            let msg = format!("2026-11-25 12:00:00.000 [INFO ] [TEST] event line number {idx}\n");
            write_line_with_rotation(&mut inner, msg.as_bytes(), true);
        }

        assert!(active_path.exists(), "active log must exist");
        assert!(tmp_dir.join("zettel-quick.1.log").exists(), "backup 1 must exist");
        assert!(tmp_dir.join("zettel-quick.2.log").exists(), "backup 2 must exist");
        assert!(
            !tmp_dir.join("zettel-quick.3.log").exists(),
            "backups beyond max_backups=2 must be pruned"
        );

        drop(inner);
        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
