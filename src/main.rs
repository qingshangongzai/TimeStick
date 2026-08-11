//! TimeStick（时间贴）：全屏时间悬浮窗
//!
//! 入口：DPI 感知 → 单实例保护 → 隐藏主窗口（托盘/热键/消息）→ 全屏轮询线程 → 消息循环。

#![windows_subsystem = "windows"]

mod detect;
mod floater;
mod log;
mod render;
mod tray;

use std::mem::size_of;
use std::ptr::addr_of_mut;

use windows::core::{w, PCWSTR, Result};
use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM, ERROR_ALREADY_EXISTS};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log::Log;

// 自定义消息（WM_APP 起）
pub const WM_APP_TRAY: u32 = WM_APP + 1; // 托盘回调
pub const WM_APP_FS: u32 = WM_APP + 2;   // 全屏状态变化，wparam: 1=进入 0=退出
pub const WM_APP_EXIT: u32 = WM_APP + 3; // 请求退出

pub const HOTKEY_ID: i32 = 1; // Ctrl+Alt+T：固定模式解除固定

const SINGLE_INSTANCE_MUTEX: PCWSTR = w!("Local\\TimeStick_SingleInstance");

struct App {
    main_hwnd: HWND,
    floater: floater::Floater,
    tray: tray::Tray,
}

impl App {
    fn new(hinst: HINSTANCE) -> Result<Self> {
        let floater = floater::Floater::new(hinst);

        // 隐藏主窗口：承载托盘回调、全局热键与全屏消息
        let class = w!("TimeStickMain");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(app_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: HICON::default(),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class,
            hIconSm: HICON::default(),
        };
        unsafe { let _ = RegisterClassExW(&wc); }
        let main_hwnd = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("TimeStick"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)?
        };
        let tray = tray::Tray::new(main_hwnd)?;
        Ok(App { main_hwnd, floater, tray })
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
                if id == tray::TRAY_CMD_EXIT {
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
    let floater_hwnd = app.floater.hwnd();
    detect::spawn(main_hwnd, floater_hwnd);

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
