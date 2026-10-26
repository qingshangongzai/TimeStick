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

//! 应用黑名单：列表内的应用处于前台时，悬浮窗不显示。
//! 存 %APPDATA%\TimeStick\blacklist.txt，逐行一个条目（exe 文件名或完整路径）：
//! 空行与 `#` 开头的行忽略；比较键为路径末段并统一转小写（Windows 文件名不区分大小写）。
//! 另提供原始行读写（lines / save_lines），供黑名单管理对话框使用；
//! 归一化只发生在 matches 比对，文件里保留用户写的原始形态。

use std::path::PathBuf;

use crate::log::Log;

const CFG_FILE: &str = "blacklist.txt";

/// 判断完整 exe 路径是否命中黑名单。
/// 每次调用重读配置（不缓存）：文件 < 1KB，1Hz 读取代价可忽略，且用户编辑后 ≤1s 自然生效。
/// 文件不存在 / 读取失败 / 无 APPDATA → false（照常显示）。
pub fn matches(exe_path: &str) -> bool {
    let Some(path) = config_path() else { return false };
    let Ok(content) = std::fs::read_to_string(path) else { return false };
    matches_content(&content, exe_path)
}

/// 判断文件内容是否命中：任一有效行与该 exe 同应用（键归一见 is_same_app）。
/// 抽纯函数供单测直接覆盖 BOM / CRLF / 注释 / 大小写等解析行为（matches 依赖真实文件）。
fn matches_content(content: &str, exe_path: &str) -> bool {
    parse_lines(content).iter().any(|line| is_same_app(line, exe_path))
}

/// 配置文件路径：%APPDATA%\TimeStick\blacklist.txt；无 APPDATA（服务会话）时返回 None 静默跳过。
/// 跨模块只需显示路径（黑名单管理对话框），故 pub。
pub fn config_path() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|base| PathBuf::from(base).join("TimeStick").join(CFG_FILE))
}

/// 读取配置原始行（去 UTF-8 BOM 与首尾空白、跳过空行；`#` 注释行保留可删），
/// 文件不存在 / 读取失败 → 空 Vec。管理对话框以此做「文件 ↔ 列表」镜像。
pub fn lines() -> Vec<String> {
    let Some(path) = config_path() else { return Vec::new() };
    let Ok(content) = std::fs::read_to_string(path) else { return Vec::new() };
    parse_lines(&content)
}

/// 整文件重写（UTF-8 无 BOM、CRLF 换行）。写失败记日志降级，文件保持原内容。
pub fn save_lines(lines: &[String]) {
    if let Err(e) = write(&render_lines(lines)) {
        Log::log(&format!("blacklist save failed: {e}"));
    }
}

/// 解析配置原始行：去 UTF-8 BOM 与首尾空白、跳过空行；`#` 注释行原样保留。
fn parse_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// 原始行列表 → 文件内容（CRLF 换行，末行同样带换行）；空列表 → 空内容。
fn render_lines(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// 判断列表条目与给定 exe 路径是否为同一应用（末段 + 小写归一化）。
/// `#` 注释行与无文件名的行不与任何应用判同——注释条目等于被禁用，命中（matches_content）
/// 与对话框「加入黑名单」按钮共用这一语义，不会出现「注释了却仍算已拉黑」。
/// 管理对话框据此判断「上次全屏应用」是否已在列表里（列表是编辑中的镜像，不是文件）。
pub fn is_same_app(line: &str, exe_path: &str) -> bool {
    let line = line.trim();
    if line.starts_with('#') {
        return false;
    }
    let key = exe_name(line).to_lowercase();
    !key.is_empty() && key == exe_name(exe_path.trim()).to_lowercase()
}

/// 把内容写入配置文件（父目录按需创建）；无 APPDATA 视为失败。
fn write(content: &str) -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Err(std::io::Error::other("no APPDATA"));
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, content)
}

/// 取路径末段（`\` 或 `/` 分隔）；只写文件名时原样返回。
fn exe_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_takes_last_segment() {
        assert_eq!(exe_name(r"C:\Windows\explorer.exe"), "explorer.exe");
        assert_eq!(exe_name(r"C:\Program Files\Qoder IDE\Qoder IDE.exe"), "Qoder IDE.exe");
        assert_eq!(exe_name("potplayer.exe"), "potplayer.exe");
    }

    #[test]
    fn matches_ignores_blank_and_comment_lines() {
        // 注释条目等于被禁用：写出完整文件名也不命中
        assert!(!matches_content("\n  \n# notepad.exe\n  # 缩进注释\n", "notepad.exe"));
    }

    #[test]
    fn matches_normalizes_whitespace_crlf_and_case() {
        // CRLF + 前后空白 + 完整路径 + 大小写混杂 → 按末段小写键命中
        assert!(matches_content("  C:\\Program Files\\DAUM\\PotPlayer\\PotPlayerMini64.exe  \r\n", "potplayermini64.exe"));
        assert!(matches_content("a.exe\n#x\n\nB.EXE\n", "b.exe"));
        assert!(!matches_content("a.exe\n#x\n\nB.EXE\n", "c.exe"));
    }

    #[test]
    fn matches_accepts_utf8_bom() {
        assert!(matches_content("\u{feff}vlc.exe\n", "vlc.exe"));
    }

    #[test]
    fn matches_skips_lines_without_name() {
        // 只写到分隔符为止的行取不到末段，不产生空键（否则会命中空文件名）
        assert!(!matches_content("C:\\dir\\\n", ""));
    }

    #[test]
    fn parse_lines_keeps_comments_in_raw_form() {
        let items = parse_lines("# 播放器\r\n  potplayer.exe  \n\n");
        assert_eq!(items, vec!["# 播放器".to_string(), "potplayer.exe".to_string()]);
    }

    #[test]
    fn save_and_load_round_trip() {
        // 对话框「保存并关闭」按列表整文件重写，重读必须还原同一列表（否则会越存越乱）
        let items = vec!["# 播放器".to_string(), "C:\\Program Files\\DAUM\\PotPlayer\\PotPlayerMini64.exe".to_string()];
        let text = render_lines(&items);
        assert_eq!(text, "# 播放器\r\nC:\\Program Files\\DAUM\\PotPlayer\\PotPlayerMini64.exe\r\n");
        assert_eq!(parse_lines(&text), items);
        assert_eq!(render_lines(&[]), "");
    }

    #[test]
    fn is_same_app_compares_by_normalized_key() {
        assert!(is_same_app(r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe", "potplayermini64.exe"));
        assert!(is_same_app("vlc.exe", r"D:\Tools\VLC.EXE"));
        assert!(!is_same_app("vlc.exe", "mpv.exe"));
        // 注释行（临时禁用的条目）不与任何应用判同——写出完整路径也不算
        assert!(!is_same_app("# 播放器", "vlc.exe"));
        assert!(!is_same_app("# C:\\Tools\\vlc.exe", "vlc.exe"));
        assert!(!is_same_app("  # vlc.exe", "vlc.exe"));
        // 对比键为空（空串）不命中任何应用
        assert!(!is_same_app("", "vlc.exe"));
    }
}
