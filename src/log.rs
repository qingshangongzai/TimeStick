//! 可选调试日志：仅当环境变量 `TIMESTICK_LOG` 指向文件路径时写入，不影响发布体积。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: Mutex<Option<String>> = Mutex::new(None);

pub struct Log;

impl Log {
    pub fn init() {
        let path = std::env::var("TIMESTICK_LOG").unwrap_or_default();
        if !path.trim().is_empty() {
            *LOG_PATH.lock().unwrap() = Some(path);
        }
    }

    pub fn log(msg: &str) {
        let guard = LOG_PATH.lock().unwrap();
        let Some(path) = guard.as_ref() else { return };
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{}] {msg}", secs);
        }
    }
}
