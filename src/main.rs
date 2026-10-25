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
mod autostart;
mod blacklist;
mod blacklist_edit;
mod detect;
mod floater;
mod icon;
mod log;
mod position;
mod render;
mod settings;
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
pub const WM_APP_BLACKLIST_CLOSED: u32 = WM_APP + 5; // 黑名单管理对话框已销毁（无参数约定）
pub const WM_APP_BLACKLIST_APP: u32 = WM_APP + 6; // 上次全屏应用更新，lparam: UTF-16 字符串指针（同线程 SendMessage，调用期间有效）

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
    /// 黑名单管理对话框句柄（防重入；销毁经 WM_APP_BLACKLIST_CLOSED 清空）
    blacklist_hwnd: Option<HWND>,
    /// 上次进入全屏时前台应用的完整路径（对话框「加入黑名单」用；退出全屏不清空，下次进入覆盖）
    last_fullscreen_exe: Option<String>,
    /// 开机自启动勾选状态（真源是注册表，此处仅作菜单显示缓存，见 src/autostart.rs）
    auto_start: bool,
}

impl App {
    fn new(hinst: HINSTANCE) -> Result<Self> {
        // 内嵌图标：窗口类 hIcon/hIconSm 与悬浮窗类共用；加载失败保持默认
        let hicon = icon::load(icon::LARGE).unwrap_or_default();
        let hicon_sm = icon::load(icon::SMALL).unwrap_or_default();
        let mut floater = floater::Floater::new(hinst, hicon, hicon_sm);
        // 恢复上次偏好（窗口未建，set 只改字段，恒成功）
        let pref = settings::load();
        let _ = floater.set_show_seconds(pref.show_seconds);
        let _ = floater.set_seven_seg(pref.seven_seg);
        let _ = floater.set_size_level(pref.size_level);
        floater.set_color_mode(pref.color_mode);

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
        Ok(App { main_hwnd, floater, tray, hicon, hicon_sm, about_hwnd: None, blacklist_hwnd: None, last_fullscreen_exe: None, auto_start: autostart::is_enabled() })
    }

    /// 从各真源收集当前偏好并持久化（切换生效后调用）。
    fn save_prefs(&self) {
        settings::save(&settings::Settings {
            show_seconds: self.floater.show_seconds(),
            seven_seg: self.floater.seven_seg(),
            size_level: self.floater.size_level(),
            color_mode: self.floater.color_mode(),
        });
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
                app.tray.on_message(lparam, &tray::MenuState {
                    show_seconds: app.floater.show_seconds(),
                    seven_seg: app.floater.seven_seg(),
                    size_level: app.floater.size_level(),
                    color_mode: app.floater.color_mode(),
                    auto_start: app.auto_start,
                });
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xffff;
                match id {
                    tray::TRAY_CMD_SHOW => {
                        Log::log("tray: show floater");
                        app.floater.show();
                    }
                    tray::TRAY_CMD_BLACKLIST => {
                        Log::log("tray: blacklist manager");
                        blacklist_edit::show(&mut app.blacklist_hwnd, app.main_hwnd, app.last_fullscreen_exe.as_deref());
                    }
                    tray::TRAY_CMD_ABOUT => {
                        Log::log("tray: about");
                        about::show(&mut app.about_hwnd, app.main_hwnd);
                    }
                    tray::TRAY_CMD_EXIT => {
                        app.request_exit();
                    }
                    tray::TRAY_CMD_FORMAT_HM => {
                        Log::log("tray: format HH:MM");
                        // 重建失败时本次切换未生效，不持久化，避免重启后恢复未生效的格式
                        if app.floater.set_show_seconds(false) {
                            app.save_prefs();
                        }
                    }
                    tray::TRAY_CMD_FORMAT_HMS => {
                        Log::log("tray: format HH:MM:SS");
                        if app.floater.set_show_seconds(true) {
                            app.save_prefs();
                        }
                    }
                    tray::TRAY_CMD_STYLE_DEFAULT => {
                        Log::log("tray: style default font");
                        if app.floater.set_seven_seg(false) {
                            app.save_prefs();
                        }
                    }
                    tray::TRAY_CMD_STYLE_SEG => {
                        Log::log("tray: style seven-seg");
                        if app.floater.set_seven_seg(true) {
                            app.save_prefs();
                        }
                    }
                    // 档位索引由命令 ID 区间算出（ID 连续，起点为极小档，见 tray.rs）
                    tray::TRAY_CMD_SIZE_XS..=tray::TRAY_CMD_SIZE_XL => {
                        let level = (id - tray::TRAY_CMD_SIZE_XS) as u8;
                        Log::log(&format!("tray: size level {level}"));
                        if app.floater.set_size_level(level) {
                            app.save_prefs();
                        }
                    }
                    // 配色模式为纯字段切换、无失败路径（不重建渲染器），切换后直接持久化
                    tray::TRAY_CMD_COLOR_AUTO => {
                        Log::log("tray: color auto");
                        app.floater.set_color_mode(settings::ColorMode::Auto);
                        app.save_prefs();
                    }
                    tray::TRAY_CMD_COLOR_WHITE => {
                        Log::log("tray: color white");
                        app.floater.set_color_mode(settings::ColorMode::White);
                        app.save_prefs();
                    }
                    tray::TRAY_CMD_COLOR_BLACK => {
                        Log::log("tray: color black");
                        app.floater.set_color_mode(settings::ColorMode::Black);
                        app.save_prefs();
                    }
                    tray::TRAY_CMD_AUTO_START => {
                        Log::log(if app.auto_start { "tray: disable auto-start" } else { "tray: enable auto-start" });
                        // 注册表为真源：写入失败不翻转缓存，菜单勾选保持与实际一致
                        if let Err(e) = autostart::set_enabled(!app.auto_start) {
                            Log::log(&format!("auto-start toggle failed: {e}"));
                        } else {
                            app.auto_start = !app.auto_start;
                        }
                    }
                    _ => {} // 未知命令：忽略
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
                    // 此刻前台仍是目标应用（悬浮窗以 SW_SHOWNOACTIVATE 显示，不夺前台）；
                    // 托盘菜单与管理对话框都会抢前台，过后再查只能查到 timestick 自己；
                    // 取不到时保留上次值，宁可显示旧值也不清空
                    if let Some(exe) = detect::foreground_exe() {
                        // 对话框开着时同步推一次：那一行是快照，不推就会一直显示旧值
                        if let Some(dlg) = app.blacklist_hwnd {
                            let mut buf: Vec<u16> = exe.encode_utf16().collect();
                            buf.push(0);
                            let _ = SendMessageW(dlg, WM_APP_BLACKLIST_APP, Some(WPARAM(0)), Some(LPARAM(buf.as_ptr() as isize)));
                        }
                        app.last_fullscreen_exe = Some(exe);
                    }
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
            WM_APP_BLACKLIST_CLOSED => {
                app.blacklist_hwnd = None;
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
