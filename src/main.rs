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

//! TimeStick（时间贴）：全屏时间悬浮窗
//!
//! 入口：DPI 感知 → 单实例保护 → 隐藏主窗口（托盘/热键/消息）→ 全屏轮询线程 → 消息循环。

#![windows_subsystem = "windows"]

mod about;
mod detect;
mod floater;
mod icon;
mod log;
mod position;
mod render;
mod tray;

use std::mem::size_of;
use std::ptr::addr_of_mut;

use windows::core::{w, PCWSTR, Result};
use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM, ERROR_ALREADY_EXISTS};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::Ime::ImmDisableIME;
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log::Log;

// 自定义消息（WM_APP 起）
pub const WM_APP_TRAY: u32 = WM_APP + 1; // 托盘回调
pub const WM_APP_FS: u32 = WM_APP + 2;   // 全屏状态变化，wparam: 1=进入 0=退出
pub const WM_APP_EXIT: u32 = WM_APP + 3; // 请求退出
pub const WM_APP_ABOUT_CLOSED: u32 = WM_APP + 4; // 关于对话框已销毁（无参数约定）

pub const HOTKEY_ID: i32 = 1; // Ctrl+Alt+T：固定模式解除固定

const SINGLE_INSTANCE_MUTEX: PCWSTR = w!("Local\\TimeStick_SingleInstance");

struct App {
    main_hwnd: HWND,
    floater: floater::Floater,
    tray: tray::Tray,
    /// 内嵌图标句柄（与悬浮窗类共用，退出时销毁，见 src/icon.rs）
    hicon: HICON,
    hicon_sm: HICON,
    /// 关于对话框句柄（防重入；销毁经 WM_APP_ABOUT_CLOSED 清空）
    about_hwnd: Option<HWND>,
}

impl App {
    fn new(hinst: HINSTANCE) -> Result<Self> {
        // 内嵌图标：窗口类 hIcon/hIconSm 与悬浮窗类共用；加载失败保持默认
        let hicon = icon::load(icon::LARGE).unwrap_or_default();
        let hicon_sm = icon::load(icon::SMALL).unwrap_or_default();
        let floater = floater::Floater::new(hinst, hicon, hicon_sm);

        // 隐藏主窗口：承载托盘回调、全局热键与全屏消息
        let class = w!("TimeStickMain");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(app_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: hicon,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class,
            hIconSm: hicon_sm,
        };
        unsafe { let _ = RegisterClassExW(&wc); }
        let main_hwnd = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("TimeStick"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)?
        };
        let tray = tray::Tray::new(main_hwnd)?;
        Ok(App { main_hwnd, floater, tray, hicon, hicon_sm, about_hwnd: None })
    }

    fn request_exit(&mut self) {
        Log::log("requesting exit");
        self.tray.remove();
        unsafe {
            let _ = UnregisterHotKey(Some(self.main_hwnd), HOTKEY_ID);
            if let Some(hwnd) = self.floater.take_hwnd() {
                let _ = DestroyWindow(hwnd);
            }
            // 触发 WM_DESTROY → PostQuitMessage 结束消息循环
            let _ = DestroyWindow(self.main_hwnd);
        }
        // 销毁内嵌图标句柄（悬浮窗类共用同一份，此处统一释放）
        if !self.hicon.is_invalid() {
            unsafe { let _ = DestroyIcon(self.hicon); }
        }
        if !self.hicon_sm.is_invalid() {
            unsafe { let _ = DestroyIcon(self.hicon_sm); }
        }
    }
}

unsafe extern "system" fn app_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if app_ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let app = &mut *app_ptr;
        match msg {
            WM_APP_TRAY => {
                app.tray.on_message(lparam);
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xffff;
                if id == tray::TRAY_CMD_SHOW {
                    Log::log("tray: show floater");
                    app.floater.show();
                } else if id == tray::TRAY_CMD_ABOUT {
                    Log::log("tray: about");
                    about::show(&mut app.about_hwnd, app.main_hwnd);
                } else if id == tray::TRAY_CMD_EXIT {
                    app.request_exit();
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                if wparam.0 as i32 == HOTKEY_ID {
                    Log::log("hotkey Ctrl+Alt+T pressed");
                    app.floater.unpin();
                }
                LRESULT(0)
            }
            WM_APP_FS => {
                if wparam.0 != 0 {
                    Log::log("fullscreen entered, showing floater");
                    app.floater.show();
                } else {
                    Log::log("fullscreen exited, hiding floater");
                    app.floater.hide();
                }
                LRESULT(0)
            }
            WM_APP_EXIT => {
                app.request_exit();
                LRESULT(0)
            }
            WM_APP_ABOUT_CLOSED => {
                app.about_hwnd = None;
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn main() -> Result<()> {
    unsafe {
        // DPI 感知必须在创建任何窗口之前声明，保证多倍率缩放下文字清晰、矩形比较准确
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // 禁用主线程 IMM 输入法：本进程无任何文本输入需求，防止隐藏主窗口被托盘菜单
        // 激活到前台时，系统把当前键盘布局的输入法（如搜狗，含 TSF 配套）加载进进程。
        // DLL 一经加载永不卸载，曾实测 +33MB 永久驻留；ImmDisableIME 线程级生效、
        // 不影响其他进程与 detect 后台线程，须在任何窗口创建前调用。
        // （实测：仅禁 TSF 无效；仅此一项即可阻止整条加载链，见 内存优化方案.md）
        let _ = ImmDisableIME(GetCurrentThreadId());
        // 单实例保护：已有实例运行则静默退出
        let _mutex = CreateMutexW(None, false, SINGLE_INSTANCE_MUTEX)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
    }
    Log::init();
    Log::log("TimeStick starting");
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
    let mut app = Box::new(App::new(hinst)?);
    unsafe {
        // 主窗口 WndProc 经 GWLP_USERDATA 获取 App 实例（Box 保证地址稳定，全程主线程访问）
        SetWindowLongPtrW(app.main_hwnd, GWLP_USERDATA, addr_of_mut!(*app) as isize);
        // 全局热键：固定模式下唯一解除方式
        match RegisterHotKey(Some(app.main_hwnd), HOTKEY_ID, MOD_CONTROL | MOD_ALT, 'T' as u32) {
            Ok(_) => Log::log("hotkey registered: Ctrl+Alt+T"),
            Err(e) => Log::log(&format!("hotkey register failed: {e}")),
        }
    }
    let main_hwnd = app.main_hwnd;
    detect::spawn(main_hwnd);

    // 消息循环
    let mut msg = MSG::default();
    loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            break; // WM_QUIT
        }
        if r.0 < 0 {
            Log::log("GetMessageW error");
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
    Log::log("TimeStick exiting");
    Ok(())
}
