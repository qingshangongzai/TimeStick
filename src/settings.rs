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

//! 设置持久化：跨重启记忆用户偏好（时间格式 + 显示风格 + 显示大小档位 + 显示配色）。
//! 存 %APPDATA%\TimeStick\settings.txt，逐行 `key=value`：
//! `show_seconds=1/0`、`seven_seg=1/0`、`size_level=0..4`、`color_mode=auto|white|black`；
//! 缺键/坏行回退默认值，写入失败仅记日志降级。

use std::path::PathBuf;

use crate::log::Log;

const CFG_FILE: &str = "settings.txt";

/// 显示配色模式；`Default` 为自动反色（按背景亮度判定）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// 按背景亮度自动反色
    #[default]
    Auto,
    /// 常驻白色文字（深色底配色）
    White,
    /// 常驻黑色文字（浅色底配色）
    Black,
}

/// 可持久化的用户偏好；`Default` 即「未设置」时的行为。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// 显示秒（HH:MM:SS）；false 为 HH:MM
    pub show_seconds: bool,
    /// 七段数码管显示风格；false 为默认字体
    pub seven_seg: bool,
    /// 显示大小档位索引（render::FONT_LEVELS 下标）
    pub size_level: u8,
    /// 显示配色模式（自动反色 / 常驻白色 / 常驻黑色）
    pub color_mode: ColorMode,
}

// 档位默认取小档（18px）而非 0，故手写 Default 而不用 derive
impl Default for Settings {
    fn default() -> Self {
        Self {
            show_seconds: false,
            seven_seg: false,
            size_level: crate::render::SIZE_LEVEL_DEFAULT,
            color_mode: ColorMode::Auto,
        }
    }
}

/// 读取设置；文件不存在 / 解析失败 → 默认值。
pub fn load() -> Settings {
    let Some(path) = config_path() else { return Settings::default() };
    let Ok(content) = std::fs::read_to_string(path) else { return Settings::default() };
    parse(&content)
}

/// 保存设置；失败仅记日志（下次启动回退默认值，不中断运行）。
pub fn save(s: &Settings) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let content = format!(
        "show_seconds={}\nseven_seg={}\nsize_level={}\ncolor_mode={}\n",
        if s.show_seconds { 1 } else { 0 },
        if s.seven_seg { 1 } else { 0 },
        s.size_level,
        match s.color_mode {
            ColorMode::Auto => "auto",
            ColorMode::White => "white",
            ColorMode::Black => "black",
        },
    );
    match std::fs::write(&path, &content) {
        Ok(_) => Log::log(&format!("settings saved: {}", content.trim())),
        Err(e) => Log::log(&format!("settings save failed: {e}")),
    }
}

/// 配置文件路径：%APPDATA%\TimeStick\settings.txt；无 APPDATA（服务会话）时返回 None 静默跳过。
fn config_path() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|base| PathBuf::from(base).join("TimeStick").join(CFG_FILE))
}

/// 解析逐行 `key=value` 配置；未知键 / 值非法 / 格式不符的行忽略，缺键回退默认值。
fn parse(content: &str) -> Settings {
    let mut s = Settings::default();
    for line in content.lines() {
        let mut kv = line.trim().splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some("show_seconds"), Some("1")) => s.show_seconds = true,
            (Some("show_seconds"), Some("0")) => s.show_seconds = false,
            (Some("seven_seg"), Some("1")) => s.seven_seg = true,
            (Some("seven_seg"), Some("0")) => s.seven_seg = false,
            // 档位须落在 FONT_LEVELS 下标范围内；越界 / 非数字忽略，保持默认小档
            (Some("size_level"), Some(v)) => match v.parse::<u8>() {
                Ok(n) if (n as usize) < crate::render::FONT_LEVELS.len() => s.size_level = n,
                _ => {}
            },
            // 配色模式值取文字颜色语义；非法值 / 空值落入下面的兜底忽略，保持默认自动反色
            (Some("color_mode"), Some("auto")) => s.color_mode = ColorMode::Auto,
            (Some("color_mode"), Some("white")) => s.color_mode = ColorMode::White,
            (Some("color_mode"), Some("black")) => s.color_mode = ColorMode::Black,
            _ => {} // 未知键 / 坏行：忽略
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_keys() {
        let s = parse("show_seconds=1\nseven_seg=1\nsize_level=3\n");
        assert_eq!(s, Settings { show_seconds: true, seven_seg: true, size_level: 3, color_mode: ColorMode::Auto });
        let s = parse("show_seconds=0\nseven_seg=0\nsize_level=0\n");
        assert_eq!(s, Settings { show_seconds: false, seven_seg: false, size_level: 0, color_mode: ColorMode::Auto });
    }

    #[test]
    fn parse_missing_keys_fall_back_to_default() {
        assert_eq!(parse(""), Settings::default());
        let s = parse("seven_seg=1\n");
        assert_eq!(s, Settings { show_seconds: false, seven_seg: true, size_level: crate::render::SIZE_LEVEL_DEFAULT, color_mode: ColorMode::Auto });
        // 缺省 size_level → 小档（非 0）
        assert_eq!(parse("show_seconds=1\n").size_level, crate::render::SIZE_LEVEL_DEFAULT);
    }

    #[test]
    fn parse_ignores_bad_lines() {
        // 坏值 / 未知键 / 格式不符的行忽略，其余键照常解析
        let s = parse("show_seconds=2\nseven_seg=1\nshow_seconds=1 extra\nbogus=1\n");
        assert_eq!(s, Settings { show_seconds: false, seven_seg: true, size_level: crate::render::SIZE_LEVEL_DEFAULT, color_mode: ColorMode::Auto });
        let s = parse("show_seconds=1\nseven_seg=5\n");
        assert_eq!(s, Settings { show_seconds: true, seven_seg: false, size_level: crate::render::SIZE_LEVEL_DEFAULT, color_mode: ColorMode::Auto });
        let s = parse("show_seconds = 1\nseven_seg=1\n");
        assert_eq!(s, Settings { seven_seg: true, show_seconds: false, size_level: crate::render::SIZE_LEVEL_DEFAULT, color_mode: ColorMode::Auto });
    }

    #[test]
    fn parse_size_level_accepts_all_levels() {
        // 五个合法档位索引（0..FONT_LEVELS.len()）
        for n in 0..crate::render::FONT_LEVELS.len() as u8 {
            assert_eq!(parse(&format!("size_level={n}\n")).size_level, n);
        }
    }

    #[test]
    fn parse_size_level_out_of_range_falls_back() {
        // 越界 / 非数字 / 空值：保持默认小档（FONT_LEVELS.len() 为越界下界）
        assert_eq!(parse("size_level=5\n"), Settings::default());
        assert_eq!(parse("size_level=255\n"), Settings::default());
        assert_eq!(parse("size_level=x\n"), Settings::default());
        assert_eq!(parse("size_level=\n"), Settings::default());
        assert_eq!(parse("size_level=-1\n"), Settings::default());
    }

    #[test]
    fn parse_size_level_mixes_with_other_keys() {
        // 与其它键混合解析：坏档位不影响其余键
        let s = parse("show_seconds=1\nsize_level=9\nseven_seg=1\n");
        assert_eq!(s, Settings { show_seconds: true, seven_seg: true, size_level: crate::render::SIZE_LEVEL_DEFAULT, color_mode: ColorMode::Auto });
        let s = parse("size_level=2\nshow_seconds=1\nseven_seg=0\n");
        assert_eq!(s, Settings { show_seconds: true, seven_seg: false, size_level: 2, color_mode: ColorMode::Auto });
        // 末档（特大，索引 = 档数 - 1）合法
        let last = (crate::render::FONT_LEVELS.len() - 1) as u8;
        assert_eq!(parse(&format!("size_level={last}\n")).size_level, last);
    }

    #[test]
    fn parse_color_mode_all_values() {
        assert_eq!(parse("color_mode=auto\n").color_mode, ColorMode::Auto);
        assert_eq!(parse("color_mode=white\n").color_mode, ColorMode::White);
        assert_eq!(parse("color_mode=black\n").color_mode, ColorMode::Black);
        // 非法值 / 空值 / 缺键回退 Auto
        assert_eq!(parse("color_mode=light\n").color_mode, ColorMode::Auto);
        assert_eq!(parse("color_mode=\n").color_mode, ColorMode::Auto);
        assert_eq!(parse("").color_mode, ColorMode::Auto);
    }
}
