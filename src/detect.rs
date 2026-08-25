//! 全屏检测轮询线程：1Hz 轮询前台窗口，进入全屏延迟 1.5s 防抖后通知主线程。

use std::mem::size_of;
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, PostMessageW, GA_ROOT,
    GWL_STYLE, WS_CAPTION, WS_MAXIMIZE, WS_THICKFRAME,
};

use crate::WM_APP_FS;

/// 启动全屏检测线程。轮询优于事件监听：播放器原地切换全屏（前台窗口不变）时
/// WinEventHook 可能不触发，而 1Hz 轮询开销可忽略。
pub fn spawn(main_hwnd: HWND) {
    // 0.62 中 HWND 未实现 Send，闭包以 usize 传递句柄
    let main_hwnd = main_hwnd.0 as usize;
    thread::spawn(move || {
        let main_hwnd = HWND(main_hwnd as *mut std::ffi::c_void);
        let mut prev = false;
        loop {
            thread::sleep(Duration::from_millis(1000));
            let cur = is_fullscreen();
            if cur == prev {
                continue;
            }
            if cur {
                // 防抖：1.5s 后复核，仍全屏才通知显示，避免频繁切换闪烁
                thread::sleep(Duration::from_millis(1500));
                if !is_fullscreen() {
                    continue;
                }
            }
            prev = cur;
            unsafe { let _ = PostMessageW(Some(main_hwnd), WM_APP_FS, WPARAM(cur as usize), LPARAM(0)); }
        }
    });
}

/// 判定前台窗口是否完整覆盖其所在显示器（副屏全屏同样触发；悬浮窗固定主屏显示）。
/// 排除规则：带 WS_MAXIMIZE 的窗口（任务栏自动隐藏时最大化同样覆盖全屏，属误报）。
fn is_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        let root = GetAncestor(fg, GA_ROOT);
        let style = GetWindowLongPtrW(root, GWL_STYLE);
        // 误报过滤：任务栏自动隐藏时，"带边框的最大化"窗口同样覆盖全屏，须排除。
        // 但浏览器 F11 全屏等"无边框最大化"（不带 WS_CAPTION/WS_THICKFRAME）是真实全屏，放行。
        if style & (WS_MAXIMIZE.0 as isize) != 0 && style & ((WS_CAPTION | WS_THICKFRAME).0 as isize) != 0 {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(root, &mut rect).is_err() {
            return false;
        }
        let mon = MonitorFromWindow(root, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        const TOL: i32 = 2; // ±2px 容差
        rect.left <= m.left + TOL
            && rect.top <= m.top + TOL
            && rect.right >= m.right - TOL
            && rect.bottom >= m.bottom - TOL
    }
}
