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

//! 全屏检测轮询线程：1Hz 轮询前台窗口，进入全屏延迟 1.5s 防抖后通知主线程；
//! 命中应用黑名单的全屏不显示悬浮窗（组合判定见 fullscreen_state）。

use std::mem::size_of;
use std::ptr::addr_of_mut;
use std::thread;
use std::time::Duration;

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, PostMessageW, GA_ROOT, GWL_STYLE, WS_CAPTION, WS_MAXIMIZE, WS_THICKFRAME,
};

use crate::blacklist;
use crate::log::Log;
use crate::WM_APP_FS;

const EXE_PATH_CAP: usize = 1024; // 进程完整路径缓冲（UTF-16 字符数）；超长路径查询失败 → 按未命中降级

/// 前台判定结果：决定是否通知显示，并从状态变化得知隐藏原因（黑名单命中需单独记日志）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum FullscreenState {
    /// 非全屏
    Normal,
    /// 全屏但命中黑名单
    Blocked,
    /// 全屏且未命中黑名单
    Fullscreen,
}

/// 启动全屏检测线程。轮询优于事件监听：播放器原地切换全屏（前台窗口不变）时
/// WinEventHook 可能不触发，而 1Hz 轮询开销可忽略。
/// 命中黑名单的全屏折叠为 `FullscreenState::Blocked`，隐藏路径与退出全屏一致（wparam=0）。
pub fn spawn(main_hwnd: HWND) {
    // 0.62 中 HWND 未实现 Send，闭包以 usize 传递句柄
    let main_hwnd = main_hwnd.0 as usize;
    thread::spawn(move || {
        let main_hwnd = HWND(main_hwnd as *mut std::ffi::c_void);
        let mut prev = FullscreenState::Normal;
        loop {
            thread::sleep(Duration::from_millis(1000));
            let cur = fullscreen_state();
            if cur == prev {
                continue;
            }
            if cur == FullscreenState::Fullscreen {
                // 防抖：1.5s 后复核，仍应显示才通知，避免频繁切换闪烁。
                // 复核同样走组合判定：期间被加入黑名单则不再显示
                thread::sleep(Duration::from_millis(1500));
                if fullscreen_state() != FullscreenState::Fullscreen {
                    continue;
                }
            }
            // 仅状态变化时记一行：1Hz 轮询下每次命中都记会刷屏
            if cur == FullscreenState::Blocked
                && let Some(exe) = foreground_exe()
            {
                Log::log(&format!("blacklist hit: {exe}"));
            }
            prev = cur;
            let show = (cur == FullscreenState::Fullscreen) as usize;
            unsafe { let _ = PostMessageW(Some(main_hwnd), WM_APP_FS, WPARAM(show), LPARAM(0)); }
        }
    });
}

/// 判定前台窗口是否完整覆盖其所在显示器（副屏全屏同样触发；悬浮窗固定主屏显示）。
/// 排除规则：① 桌面窗口（点击桌面空白处 / Win+D 后成为前台，见 is_desktop_window）；
/// ② 带 WS_MAXIMIZE 的窗口（任务栏自动隐藏时最大化同样覆盖全屏，属误报）。
fn is_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        let root = GetAncestor(fg, GA_ROOT);
        // 误报过滤①：桌面窗口铺满屏幕但不是全屏应用，须最先排除
        if is_desktop_window(root) {
            return false;
        }
        let style = GetWindowLongPtrW(root, GWL_STYLE);
        // 误报过滤②：任务栏自动隐藏时，"带边框的最大化"窗口同样覆盖全屏，须排除。
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

/// 组合判定：几何全屏 + 黑名单。is_fullscreen 只管几何，黑名单在此组合（短路：非全屏时
/// 不查进程、不读配置，静默态与改动前开销完全一致）。
fn fullscreen_state() -> FullscreenState {
    if !is_fullscreen() {
        return FullscreenState::Normal;
    }
    if is_blacklisted() { FullscreenState::Blocked } else { FullscreenState::Fullscreen }
}

/// 前台根窗口所属进程是否命中黑名单；取不到进程名时按未命中处理。
fn is_blacklisted() -> bool {
    match foreground_exe() {
        Some(path) => blacklist::matches(&path),
        // 保护进程 / 提权进程无法查询（实测 winlogon、lsass 等 OpenProcess 返回 ERROR_ACCESS_DENIED=5）
        None => false,
    }
}

/// 取前台根窗口所属进程的完整 exe 路径；无前台窗口 / 进程查询被拒 / 路径超长 → None。
/// 取窗口径与 is_fullscreen 一致（GetAncestor GA_ROOT），UWP 宿主窗口同样适用。
/// 跨模块调用：悬浮窗「拉黑」按钮在主线程调用（WS_EX_NOACTIVATE 窗口点击不夺前台）。
pub fn foreground_exe() -> Option<String> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return None;
        }
        let root = GetAncestor(fg, GA_ROOT);
        let mut pid = 0u32;
        // 取裸指针：Option<*mut u32> 形参不会做 &mut → *mut 的隐式 coercion
        GetWindowThreadProcessId(root, Some(addr_of_mut!(pid)));
        if pid == 0 {
            return None;
        }
        // PROCESS_QUERY_LIMITED_INFORMATION 是最小查询权限，无需 PROCESS_VM_READ
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; EXE_PATH_CAP];
        let mut len = EXE_PATH_CAP as u32;
        let r = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), addr_of_mut!(len));
        // 句柄即时释放：每次全屏轮询都会调用本函数，泄漏会按秒累积
        let _ = CloseHandle(handle);
        r.ok()?;
        // len 为写入字符数（不含结尾 null）
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// 判定窗口是否为桌面本体（类名 Progman）或壁纸宿主（WorkerW）。
/// 点击桌面空白处 / Win+D 后前台即桌面窗口，矩形天然铺满屏幕，属全屏误报；
/// 壁纸引擎等场景前台还可能是 WorkerW 而非 Progman，故按类名统一排除。
fn is_desktop_window(hwnd: HWND) -> bool {
    const CLASS_NAME_CAP: usize = 256; // 类名缓冲容量：Win32 类名文档上限 256 字符
    let mut name = [0u16; CLASS_NAME_CAP];
    // GetClassNameW 失败返回 0：按非桌面处理，交由几何判定兜底
    let len = unsafe { GetClassNameW(hwnd, &mut name) };
    if len <= 0 {
        return false;
    }
    let name = &name[..len as usize];
    // w! 字面量自带结尾 null，满足 as_wide 读取到终止符的前置条件
    unsafe { name == w!("Progman").as_wide() || name == w!("WorkerW").as_wide() }
}
