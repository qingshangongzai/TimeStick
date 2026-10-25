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

//! 位置持久化：跨重启记忆悬浮窗“大致位置”，并提供九宫格格位坐标计算。
//! 保存“显示器设备名 + 显示器内比例坐标（窗口左上角）”，分辨率改变后按比例换算；
//! 目标显示器不存在时按同一比例回退主显示器；换算结果夹取在显示器可视区内。

use std::mem::size_of;
use std::path::PathBuf;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use crate::log::Log;

const CFG_FILE: &str = "position.txt";
const GRID_MARGIN: i32 = 24; // 九宫格格位距屏四边边距（与 floater.rs 的 MARGIN 同值：默认右下角位边距）

/// 拖动结束后调用：记录窗口所在显示器与显示器内比例位置。
pub fn save(hwnd: HWND) {
    let Some(path) = config_path() else { return };
    unsafe {
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            Log::log("position save: GetWindowRect failed");
            return;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let Some(m) = monitor_rect(mon) else {
            Log::log("position save: GetMonitorInfoW failed");
            return;
        };
        let Some(device) = device_name(mon) else {
            Log::log("position save: device name failed");
            return;
        };
        // 窗口拖出屏幕边缘时比例越界，夹取到 [0,1] 保证下次恢复仍可见
        let rx = ((wr.left - m.left) as f32 / ((m.right - m.left) as f32)).clamp(0.0, 1.0);
        let ry = ((wr.top - m.top) as f32 / ((m.bottom - m.top) as f32)).clamp(0.0, 1.0);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, format!("{device} {rx:.4} {ry:.4}\n")) {
            Ok(_) => Log::log(&format!("position saved: {device} ({rx:.3}, {ry:.3})")),
            Err(e) => Log::log(&format!("position save failed: {e}")),
        }
    }
}

/// 进程内首次显示悬浮窗时调用：换算恢复位置。
/// 返回 None（无配置/解析失败/显示器信息获取失败）时调用方回退默认位置。
pub fn load(win_size: (i32, i32)) -> Option<POINT> {
    let content = std::fs::read_to_string(config_path()?).ok()?;
    let (device, rx, ry) = parse(&content)?;
    // 目标显示器不存在 → 同比例回退主显示器（“大致位置”语义）
    let rect = resolve_monitor(&device)?;
    Some(scale_point(rect, rx, ry, win_size))
}

/// 取显示器设备名（如 `\\.\DISPLAY1`）；失败返回 None。
pub fn device_name(mon: HMONITOR) -> Option<String> {
    unsafe {
        let mut mi = MONITORINFOEXW {
            monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() },
            ..Default::default()
        };
        // GetMonitorInfoW 形参为 *mut MONITORINFO，传 EX 结构需指针转换（cbSize 决定实际填充量）
        if GetMonitorInfoW(mon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            Some(String::from_utf16_lossy(&mi.szDevice).trim_end_matches('\0').to_string())
        } else {
            None
        }
    }
}

/// 按设备名解析显示器整屏矩形；设备名为空或不命中时回退主屏（与记忆模式回退口径一致）。
pub fn resolve_monitor(device: &str) -> Option<RECT> {
    find_monitor(device).or_else(|| {
        let mon = unsafe { MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY) };
        monitor_rect(mon)
    })
}

/// 配置文件路径：%APPDATA%\TimeStick\position.txt；无 APPDATA（服务会话）时返回 None 静默跳过。
fn config_path() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|base| PathBuf::from(base).join("TimeStick").join(CFG_FILE))
}

/// 解析一行配置 `<设备名> <rx> <ry>`；比例越界视为文件损坏返回 None。
fn parse(content: &str) -> Option<(String, f32, f32)> {
    let mut it = content.split_whitespace();
    let device = it.next()?.to_string();
    let rx: f32 = it.next()?.parse().ok()?;
    let ry: f32 = it.next()?.parse().ok()?;
    if !(0.0..=1.0).contains(&rx) || !(0.0..=1.0).contains(&ry) {
        return None;
    }
    Some((device, rx, ry))
}

/// 显示器矩形内按比例换算窗口左上角，并夹取保证窗口完整可见。
/// clamp 前提：窗口宽远小于任何显示器（约 100px vs 最小 640px），min ≤ max 恒成立。
fn scale_point(m: RECT, rx: f32, ry: f32, win_size: (i32, i32)) -> POINT {
    let (w, h) = win_size;
    let x = m.left + (rx * (m.right - m.left) as f32) as i32;
    let y = m.top + (ry * (m.bottom - m.top) as f32) as i32;
    POINT { x: x.clamp(m.left, m.right - w), y: y.clamp(m.top, m.bottom - h) }
}

/// 九宫格格位坐标：整屏四边内缩 GRID_MARGIN 的可用区内按格位摆放窗口左上角。
/// cell 行优先：1=左上、2=上中、3=右上、4=左中、5=居中、6=右中、7=左下、8=下中、9=右下。
/// 不做 clamp：前提同 scale_point（窗口 ≤ 约 256px，远小于最小屏 640 − 2×GRID_MARGIN）。
pub fn grid_point(m: RECT, cell: u8, win_size: (i32, i32)) -> POINT {
    let (w, h) = win_size;
    let row = (cell - 1) / 3;
    let col = (cell - 1) % 3;
    let (l, t) = (m.left + GRID_MARGIN, m.top + GRID_MARGIN);
    let (r, b) = (m.right - GRID_MARGIN, m.bottom - GRID_MARGIN);
    let x = match col { 0 => l, 1 => l + (r - l - w) / 2, _ => r - w };
    let y = match row { 0 => t, 1 => t + (b - t - h) / 2, _ => b - h };
    POINT { x, y }
}

/// EnumDisplayMonitors 回调上下文（经 LPARAM 回传）。
struct FindCtx {
    device: String,
    rect: Option<RECT>,
}

/// 按设备名（如 `\\.\DISPLAY2`）查找显示器整屏矩形；不存在返回 None。
fn find_monitor(device: &str) -> Option<RECT> {
    let mut ctx = FindCtx { device: device.to_string(), rect: None };
    unsafe {
        // 命中后回调返回 FALSE 终止枚举，EnumDisplayMonitors 随之返回 FALSE，不视为失败
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), LPARAM(&mut ctx as *mut FindCtx as isize));
    }
    ctx.rect
}

unsafe extern "system" fn enum_proc(hmon: HMONITOR, _hdc: HDC, _rect: *mut RECT, lparam: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lparam.0 as *mut FindCtx);
        let mut mi = MONITORINFOEXW {
            monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() },
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            let name = String::from_utf16_lossy(&mi.szDevice).trim_end_matches('\0').to_string();
            if name == ctx.device {
                ctx.rect = Some(mi.monitorInfo.rcMonitor);
                return BOOL(0); // 已命中，终止枚举
            }
        }
        BOOL(1)
    }
}

/// 取显示器整屏矩形（rcMonitor，与 default_pos 口径一致，非工作区 rcWork）。
fn monitor_rect(mon: HMONITOR) -> Option<RECT> {
    unsafe {
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() { Some(mi.rcMonitor) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ok() {
        assert_eq!(parse(r"\\.\DISPLAY1 0.5000 0.2500"), Some((r"\\.\DISPLAY1".to_string(), 0.5, 0.25)));
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("only-device"), None);
        assert_eq!(parse(r"\\.\DISPLAY1 abc 0.5"), None);
        assert_eq!(parse(r"\\.\DISPLAY1 1.5 0.5"), None); // 比例越界
        assert_eq!(parse(r"\\.\DISPLAY1 0.5 -0.1"), None);
    }

    #[test]
    fn scale_point_scales_ratio() {
        // 副屏位于主屏左侧（left 为负）的换算
        let m = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        let p = scale_point(m, 0.5, 0.25, (160, 80));
        assert_eq!((p.x, p.y), (-960, 270));
    }

    #[test]
    fn scale_point_clamps_right_bottom() {
        // rx=ry=1.0 时夹取到右/下边缘内缩窗口尺寸处，保证完整可见
        let m = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let p = scale_point(m, 1.0, 1.0, (160, 80));
        assert_eq!((p.x, p.y), (1920 - 160, 1080 - 80));
    }

    #[test]
    fn grid_point_covers_all_cells() {
        // 1920×1080 整屏 + (160, 80) 窗口：九格期望坐标直接断言（不重算公式）
        let m = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let cases = [
            (1u8, (24, 24)),
            (2, (880, 24)),
            (3, (1736, 24)),
            (4, (24, 500)),
            (5, (880, 500)),
            (6, (1736, 500)),
            (7, (24, 976)),
            (8, (880, 976)),
            (9, (1736, 976)),
        ];
        for (cell, (x, y)) in cases {
            let p = grid_point(m, cell, (160, 80));
            assert_eq!((p.x, p.y), (x, y), "格位 {cell}");
        }
    }

    #[test]
    fn grid_point_handles_negative_left_monitor() {
        // 副屏位于主屏左侧（left 为负）：格位同样按内缩区摆放
        let m = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        let p1 = grid_point(m, 1, (160, 80));
        assert_eq!((p1.x, p1.y), (-1896, 24));
        let p5 = grid_point(m, 5, (160, 80));
        assert_eq!((p5.x, p5.y), (-1040, 500));
        let p9 = grid_point(m, 9, (160, 80));
        assert_eq!((p9.x, p9.y), (-184, 976));
    }

    #[test]
    fn grid_point_center_is_symmetric() {
        // 居中格左右边距相等（24px 对称内缩不改变屏中心）
        let m = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let (w, h) = (160, 80);
        let p = grid_point(m, 5, (w, h));
        assert_eq!(p.x - m.left, m.right - (p.x + w));
        assert_eq!(p.y - m.top, m.bottom - (p.y + h));
    }
}
