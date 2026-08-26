/*
 * Copyright (c) 2026 青山公仔
 * TimeStick（时间贴）is licensed under Mulan PSL v2.
 * You can use this software according to the terms and conditions of the Mulan PSL v2.
 * You may obtain a copy of Mulan PSL v2 at:
 *     http://license.coscl.org.cn/MulanPSL2
 * THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY KIND,
 * EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO NON-INFRINGEMENT,
 * MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
 * See the Mulan PSL v2 for more details.
 */

//! 设置持久化：跨重启记忆用户偏好（当前仅时间格式）。
//! 存 %APPDATA%\TimeStick\settings.txt，单行 `show_seconds=1/0`；
//! 文件不存在或解析失败时调用方使用默认值，写入失败仅记日志降级。

use std::path::PathBuf;

use crate::log::Log;

const CFG_FILE: &str = "settings.txt";

/// 读取设置；文件不存在 / 解析失败 → None（调用方回退默认值）。
pub fn load() -> Option<bool> {
    let content = std::fs::read_to_string(config_path()?).ok()?;
    parse_show_seconds(&content)
}

/// 保存设置；失败仅记日志（下次启动回退默认值，不中断运行）。
pub fn save(v: bool) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let line = if v { "show_seconds=1" } else { "show_seconds=0" };
    match std::fs::write(&path, format!("{line}\n")) {
        Ok(_) => Log::log(&format!("settings saved: {line}")),
        Err(e) => Log::log(&format!("settings save failed: {e}")),
    }
}

/// 配置文件路径：%APPDATA%\TimeStick\settings.txt；无 APPDATA（服务会话）时返回 None 静默跳过。
fn config_path() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|base| PathBuf::from(base).join("TimeStick").join(CFG_FILE))
}

/// 解析单行配置 `show_seconds=1/0`；格式不符 / 值非法 → None。
fn parse_show_seconds(content: &str) -> Option<bool> {
    match content.trim() {
        "show_seconds=1" => Some(true),
        "show_seconds=0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ok() {
        assert_eq!(parse_show_seconds("show_seconds=1\n"), Some(true));
        assert_eq!(parse_show_seconds("show_seconds=0\n"), Some(false));
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(parse_show_seconds(""), None);
        assert_eq!(parse_show_seconds("show_seconds=2\n"), None);
        assert_eq!(parse_show_seconds("show_seconds=\n"), None);
        assert_eq!(parse_show_seconds("show_seconds = 1\n"), None);
        assert_eq!(parse_show_seconds("show_seconds=1 extra\n"), None);
        assert_eq!(parse_show_seconds("abc\n"), None);
    }
}
