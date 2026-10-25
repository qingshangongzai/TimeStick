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

//! 黑名单管理对话框：查看 / 添加 / 删除 %APPDATA%\TimeStick\blacklist.txt 的条目。
//! 框架与 about.rs 同款：保留 WS_CAPTION|WS_SYSMENU + 截留 WM_NCCALCSIZE（客户区=整窗，
//! DWM 阴影/动画保留，Win11 经 DWMWA_WINDOW_CORNER_PREFERENCE 取 8px 圆角），owner=主窗口。
//!
//! 数据语义：列表是文件原始行的镜像（跳过空行，`#` 注释行保留可删），「添加」追加输入原文；
//! 「保存并关闭」与右上 ✕ 等价，按列表整文件重写（UTF-8 无 BOM、CRLF）——
//! 归一化（末段 + 小写）只发生在 blacklist::matches 比对，文件里保留用户写的原始形态。
//! 保存只写文件、不通知 detect：复用「1Hz 重读」，≤1s 自然生效。

use std::mem::size_of;
use std::ptr::addr_of_mut;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWM_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DEFAULT_PITCH, DEFAULT_QUALITY, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EndPaint, FONT_WEIGHT, FW_BOLD, FW_NORMAL, FillRect, GetMonitorInfoW, GetSysColorBrush, HBRUSH,
    HDC, HFONT, HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, ScreenToClient, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::SS_PATHELLIPSIS;
use windows::Win32::System::WindowsProgramming::MulDiv;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_SELECTED, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::blacklist;
use crate::log::Log;

const DLG_W: i32 = 520; // 客户区宽（96 DPI 逻辑像素）
const DLG_H: i32 = 460; // 客户区高
const MARGIN: i32 = 24; // 页面边距（桌面端安全区）
const GAP: i32 = 8; // 控件间隙（8px 基准单元）
const LINE_H: i32 = 20; // 说明文本行高（9pt）
const TITLE_H: i32 = 24; // 标题行高（12pt 粗体）
const BTN_H: i32 = 28; // 底部按钮高
const BTN_GAP: i32 = 8; // 按钮水平间距
const BTN_W_ADD: i32 = 72; // 「添加」宽
const BTN_W_DEL: i32 = 96; // 「删除选中」宽
const BTN_W_SAVE: i32 = 112; // 「保存并关闭」宽
const BTN_W_ADOPT: i32 = 88; // 「加入黑名单」宽（「已在黑名单」同宽）
const INPUT_H: i32 = 28; // 输入框高（单行，与按钮同高：可点击感一致）
const CLOSE_SZ: i32 = 32; // 右上角自绘关闭按钮热区边长（与 about.rs 一致）

// 去线留白色板：不透明度阶梯表达层级（白底），与 about.rs 同一套灰阶
const COL_TEXT_PRIMARY: COLORREF = COLORREF(0x001A_1A1A); // 标题：近黑
const COL_TEXT_SECONDARY: COLORREF = COLORREF(0x0075_7575); // 说明：中灰
const COL_EDIT_TEXT: COLORREF = COLORREF(0x0026_2626); // 列表 / 输入 / 按钮文字：85% 黑
const COL_FILL: COLORREF = COLORREF(0x00F7_F7F7); // 列表 / 输入框填充底
const COL_BTN_FILL: COLORREF = COLORREF(0x00EF_EFEF); // 按钮常态
const COL_BTN_FILL_ACTIVE: COLORREF = COLORREF(0x00E0_E0E0); // 按钮按下

const TITLE_TEXT: &str = "黑名单管理";
const FORMAT_TEXT: &str = "每行一个 exe 文件名（或完整路径），# 开头为注释；保存后 1 秒内生效";
const STORE_TEXT: &str = "商店应用会记为 ApplicationFrameHost.exe，拉黑会影响全部此类应用";
/// 「上次全屏应用」行前缀（进入全屏时由主线程记住的目标应用，见 main.rs 的 WM_APP_FS）
const ADOPT_LABEL: &str = "上次全屏应用：";
const ADOPT_BTN_ADD: &str = "加入黑名单";
const ADOPT_BTN_LISTED: &str = "已在黑名单";

// 按钮 ID：WM_COMMAND 按此区分（列表 / 输入框只持有句柄，不需要 ID）
const IDC_ADD: isize = 1;
const IDC_DELETE: isize = 2;
const IDC_SAVE: isize = 3;
const IDC_ADOPT: isize = 4; // 把上次全屏应用加进列表

const CLASS_NAME: PCWSTR = w!("TimeStickBlacklist");

/// 对话框自有资源与子控件句柄（GWLP_USERDATA 挂载，WM_DESTROY 释放）。
struct EditState {
    font_title: HFONT,
    font_body: HFONT,
    /// 列表 / 输入框填充底色
    fill_brush: HBRUSH,
    /// 自绘按钮：常态底 / 按下底
    btn_brush: HBRUSH,
    btn_active_brush: HBRUSH,
    /// owner（主窗口）句柄：WM_DESTROY 时直接通知，不依赖 GetParent
    owner: HWND,
    dpi: u32,
    title_st: HWND,
    path_st: HWND,
    format_st: HWND,
    store_st: HWND,
    list: HWND,
    input: HWND,
    add_btn: HWND,
    del_btn: HWND,
    save_btn: HWND,
    /// 「上次全屏应用」行：文本 + 「加入黑名单」按钮（adopt_path 为 None 时两者不占布局）
    app_st: HWND,
    adopt_btn: HWND,
    adopt_path: Option<String>,
    /// 自绘 ✕ 的悬停态（无独立控件，由父窗口绘制与命中）
    close_hover: bool,
}

/// 已打开则前置激活已有窗口，否则创建（owner=主窗口 → 随主窗口销毁自动清理）。
/// `last_fullscreen_exe` 为主线程记住的上次全屏应用完整路径（None 则不显示该行）。
pub fn show(blacklist_hwnd: &mut Option<HWND>, owner: HWND, last_fullscreen_exe: Option<&str>) {
    unsafe {
        if let Some(h) = *blacklist_hwnd {
            let _ = SetForegroundWindow(h);
            return;
        }
        let hinst: HINSTANCE = GetModuleHandleW(None).ok().map(Into::into).unwrap_or_default();
        register_class(hinst);
        // 先按逻辑尺寸隐藏创建，之后按 DPI 调整物理尺寸（触发 WM_SIZE 完成布局）
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("黑名单管理"),
            WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            DLG_W,
            DLG_H,
            Some(owner),
            None,
            Some(hinst),
            None,
        ) else {
            Log::log("blacklist dialog: create window failed");
            return;
        };
        // Win11 圆角（8px 与悬浮窗 CORNER_RADIUS 一致）；Win10 不支持该属性 → 失败忽略（直角降级）
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const std::ffi::c_void,
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        let dpi = sample_dpi(hwnd);
        setup(hwnd, owner, dpi, last_fullscreen_exe);
        resize_and_center(hwnd, owner, dpi);
        *blacklist_hwnd = Some(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

/// 采样窗口 DPI（失败时取系统 DPI，再兜底 96）。
unsafe fn sample_dpi(hwnd: HWND) -> u32 {
    unsafe {
        let mut dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            dpi = GetDpiForSystem();
        }
        if dpi == 0 {
            dpi = 96;
        }
        dpi
    }
}

unsafe fn register_class(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(edit_wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // 每次开窗都会注册，重复注册忽略（与 main.rs 惯例一致）
        let _ = RegisterClassExW(&wc);
    }
}

/// 创建字体 / 画刷 / 全部子控件并挂载 EditState（经 GWLP_USERDATA 回传）。
unsafe fn setup(hwnd: HWND, owner: HWND, dpi: u32, last_fullscreen_exe: Option<&str>) {
    unsafe {
        let mut st = Box::new(EditState {
            font_title: create_font(dpi, FW_BOLD, 12),
            font_body: create_font(dpi, FW_NORMAL, 9),
            fill_brush: CreateSolidBrush(COL_FILL),
            btn_brush: CreateSolidBrush(COL_BTN_FILL),
            btn_active_brush: CreateSolidBrush(COL_BTN_FILL_ACTIVE),
            owner,
            dpi,
            title_st: HWND::default(),
            path_st: HWND::default(),
            format_st: HWND::default(),
            store_st: HWND::default(),
            list: HWND::default(),
            input: HWND::default(),
            add_btn: HWND::default(),
            del_btn: HWND::default(),
            save_btn: HWND::default(),
            app_st: HWND::default(),
            adopt_btn: HWND::default(),
            adopt_path: last_fullscreen_exe.map(str::to_string),
            close_hover: false,
        });
        // 先挂载再建子控件：创建期的 WM_CTLCOLORSTATIC 也能取到配色状态
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, addr_of_mut!(*st) as isize);

        let st_basic = WS_CHILD | WS_VISIBLE;
        st.title_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        set_text(st.title_st, TITLE_TEXT);
        st.path_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        let path = blacklist::config_path().map(|p| p.display().to_string()).unwrap_or_default();
        set_text(st.path_st, &format!("文件：{path}"));
        st.format_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        set_text(st.format_st, FORMAT_TEXT);
        st.store_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        set_text(st.store_st, STORE_TEXT);
        // 列表与输入框不加边框：以填充底色区分层次（去线留白）；
        // 列表不需 LBS_NOTIFY：选中 / 点击均由内置行为完成，不需要通知
        st.list = create_child(
            hwnd,
            w!("ListBox"),
            PCWSTR::null(),
            WINDOW_STYLE(st_basic.0 | WS_VSCROLL.0 | LBS_NOINTEGRALHEIGHT as u32),
            None,
        );
        st.input = create_child(hwnd, w!("Edit"), PCWSTR::null(), WINDOW_STYLE(st_basic.0 | ES_AUTOHSCROLL as u32), None);
        // 自绘按钮：系统主题控件在无 manifest 的进程里是经典 3D 外观，与对话框的填充式层次不协调
        let btn_style = WINDOW_STYLE(st_basic.0 | BS_OWNERDRAW as u32);
        st.add_btn = create_child(hwnd, w!("Button"), w!("添加"), btn_style, Some(IDC_ADD));
        st.del_btn = create_child(hwnd, w!("Button"), w!("删除选中"), btn_style, Some(IDC_DELETE));
        st.save_btn = create_child(hwnd, w!("Button"), w!("保存并关闭"), btn_style, Some(IDC_SAVE));
        // 「上次全屏应用」行：路径用 SS_PATHELLIPSIS 中间省略，不撑破布局；
        // 无记录（本次会话未全屏过）时 adopt_path 为 None，layout 不给它俩分配位置
        st.app_st = create_child(hwnd, w!("Static"), PCWSTR::null(), WINDOW_STYLE(st_basic.0 | SS_PATHELLIPSIS.0), None);
        st.adopt_btn = create_child(hwnd, w!("Button"), PCWSTR::null(), btn_style, Some(IDC_ADOPT));
        if let Some(path) = &st.adopt_path {
            set_text(st.app_st, &format!("{ADOPT_LABEL}{path}"));
        }

        apply_fonts(&st);
        for item in blacklist::lines() {
            let mut buf = wide(&item);
            let _ = SendMessageW(st.list, LB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(buf.as_mut_ptr() as isize)));
        }
        refresh_adopt(&st);
        layout(hwnd, &st);

        // 所有权转移给窗口 userdata（WM_DESTROY 时 Box::from_raw 取回释放）。
        // 若不转移，Box 会随函数返回被 drop → userdata 悬垂 → 二次释放致堆损坏
        let _ = Box::into_raw(st);
    }
}

/// 向全部子控件下发当前两档字体（列表须在添加条目之前设好，条目行高按字体计算）。
unsafe fn apply_fonts(st: &EditState) {
    unsafe {
        let set_font = |h: HWND, f: HFONT| {
            let _ = SendMessageW(h, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
        };
        set_font(st.title_st, st.font_title);
        for h in [st.path_st, st.format_st, st.store_st, st.app_st, st.list, st.input, st.add_btn, st.del_btn, st.save_btn, st.adopt_btn] {
            set_font(h, st.font_body);
        }
    }
}

/// 按新 DPI 重建两档字体（先建新并下发控件，再删旧，避免控件引用悬垂）。
unsafe fn rebuild_fonts(st: &mut EditState, dpi: u32) {
    unsafe {
        let old = [st.font_title, st.font_body];
        st.font_title = create_font(dpi, FW_BOLD, 12);
        st.font_body = create_font(dpi, FW_NORMAL, 9);
        apply_fonts(st);
        for f in old {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
    }
}

/// Segoe UI 字体：高度 = -字号(pt) × dpi / 72（负值表示字符高度）。
unsafe fn create_font(dpi: u32, weight: FONT_WEIGHT, pt: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -MulDiv(pt, dpi as i32, 72),
            0,
            0,
            0,
            weight.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    }
}

/// 逻辑尺寸 → 物理尺寸 → owner 所在显示器工作区居中。
/// 无边框（WM_NCCALCSIZE 截留）后客户区=整窗，无需 AdjustWindowRect 扩边。
unsafe fn resize_and_center(hwnd: HWND, owner: HWND, dpi: u32) {
    unsafe {
        let w = DLG_W * dpi as i32 / 96;
        let h = DLG_H * dpi as i32 / 96;
        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (x, y) = if GetMonitorInfoW(monitor, &mut mi).as_bool() {
            (
                mi.rcWork.left + (mi.rcWork.right - mi.rcWork.left - w) / 2,
                mi.rcWork.top + (mi.rcWork.bottom - mi.rcWork.top - h) / 2,
            )
        } else {
            (CW_USEDEFAULT, CW_USEDEFAULT)
        };
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

unsafe extern "system" fn edit_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let st_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EditState;
        if st_ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let st = &mut *st_ptr;
        match msg {
            WM_SIZE => {
                layout(hwnd, st);
                LRESULT(0)
            }
            // 无边框核心：客户区=整窗（标题栏视觉消失，DWM 阴影/动画保留）；
            // wparam=FALSE 的罕见路径走默认处理
            WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),
            // 顶部标题带可拖动（STATIC 对鼠标 HTTRANSPARENT，带内标签天然可拖）；
            // ✕ 热区是父窗口自绘，须让开，否则点击被当作拖拽起点
            WM_NCHITTEST => {
                let pt = screen_pt(hwnd, lparam);
                let band = (MARGIN + TITLE_H) * st.dpi as i32 / 96;
                if pt.y >= 0 && pt.y < band && !pt_in_rect(&close_rect(hwnd, st.dpi), pt) {
                    LRESULT(HTCAPTION as isize)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            // 自绘 ✕ 悬停反馈：进入时武装 TME_LEAVE，移出由 WM_MOUSELEAVE 收起
            WM_MOUSEMOVE => {
                if !st.close_hover && pt_in_rect(&close_rect(hwnd, st.dpi), client_pt(lparam)) {
                    st.close_hover = true;
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    let rc = close_rect(hwnd, st.dpi);
                    let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_MOUSELEAVE => {
                if st.close_hover {
                    st.close_hover = false;
                    let rc = close_rect(hwnd, st.dpi);
                    let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                // 仅 ✕ 热区会走到这里（其余客户区由 WM_NCHITTEST 归为 HTCAPTION / 子控件）
                if pt_in_rect(&close_rect(hwnd, st.dpi), client_pt(lparam)) {
                    save_and_close(hwnd, st);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                draw_close(st, hwnd, hdc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            // 打开即把焦点交给输入框：直接敲 exe 名就能点「添加」
            WM_SETFOCUS => {
                let _ = SetFocus(Some(st.input));
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xffff) as isize;
                if (wparam.0 >> 16) as u16 == BN_CLICKED as u16 {
                    match id {
                        IDC_ADD => add_from_input(st),
                        IDC_DELETE => delete_selected(st),
                        IDC_ADOPT => adopt_last_fullscreen(st),
                        IDC_SAVE => save_and_close(hwnd, st),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_DRAWITEM => {
                draw_button(st, &*(lparam.0 as *const DRAWITEMSTRUCT));
                LRESULT(1)
            }
            // 说明文字透明底 + 按层级给色；标题近黑，其余中灰
            WM_CTLCOLORSTATIC => {
                let hdc = HDC(wparam.0 as *mut _);
                let ctl = HWND(lparam.0 as *mut _);
                let _ = SetBkMode(hdc, TRANSPARENT);
                let _ = SetTextColor(hdc, if ctl == st.title_st { COL_TEXT_PRIMARY } else { COL_TEXT_SECONDARY });
                LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
            }
            // 列表 / 输入框：填充式底色代替边框
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                let hdc = HDC(wparam.0 as *mut _);
                let _ = SetTextColor(hdc, COL_EDIT_TEXT);
                let _ = SetBkColor(hdc, COL_FILL);
                LRESULT(st.fill_brush.0 as isize)
            }
            // 主窗口推来「上次全屏应用」：行文本是打开时的快照，对话框开着又去切全屏时靠这条刷新
            // lparam = 主窗口调用期间存活的 UTF-16 字符串指针（同线程 SendMessage，无生命周期风险）
            crate::WM_APP_BLACKLIST_APP => {
                if lparam.0 != 0
                    && let Ok(path) = PCWSTR::from_raw(lparam.0 as *const u16).to_string()
                {
                    set_adopt_path(hwnd, st, &path);
                }
                LRESULT(0)
            }
            // ✕ / Alt+F4 与「保存并关闭」等价：所见即所得，无脏状态跟踪
            WM_CLOSE => {
                save_and_close(hwnd, st);
                LRESULT(0)
            }
            // 跨显示器拖动到不同缩放率：按新 DPI 重建字体，并套用系统建议矩形
            // （lparam 指向按新旧 DPI 比例缩放后的窗口矩形，保持窗口在光标下方）
            WM_DPICHANGED => {
                let new_dpi = (wparam.0 >> 16) as u32;
                if new_dpi != 0 && new_dpi != st.dpi && lparam.0 != 0 {
                    st.dpi = new_dpi;
                    rebuild_fonts(st, new_dpi);
                    let rc = &*(lparam.0 as *const RECT);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        rc.left,
                        rc.top,
                        rc.right - rc.left,
                        rc.bottom - rc.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = DeleteObject(HGDIOBJ(st.font_title.0));
                let _ = DeleteObject(HGDIOBJ(st.font_body.0));
                let _ = DeleteObject(HGDIOBJ(st.fill_brush.0));
                let _ = DeleteObject(HGDIOBJ(st.btn_brush.0));
                let _ = DeleteObject(HGDIOBJ(st.btn_active_brush.0));
                // 同步直调主窗口清空 blacklist_hwnd（owner 已销毁则忽略，进程即将结束无泄漏）
                let _ = SendMessageW(st.owner, crate::WM_APP_BLACKLIST_CLOSED, Some(WPARAM(0)), Some(LPARAM(0)));
                // 清空 userdata 后取回 Box 释放（销毁后期消息经判空兜底）
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(st_ptr));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// 「添加」：把输入框里的原始行追加到列表末尾（trim 后非空才可加）。
unsafe fn add_from_input(st: &EditState) {
    unsafe {
        let text = control_text(st.input);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut buf = wide(text);
        // LB_ADDSTRING 复制字符串，临时缓冲即可；失败返回 LB_ERR / LB_ERRSPACE
        let idx = SendMessageW(st.list, LB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(buf.as_mut_ptr() as isize))).0;
        if idx >= 0 {
            let _ = SendMessageW(st.list, LB_SETCURSEL, Some(WPARAM(idx as usize)), Some(LPARAM(0)));
        }
        set_text(st.input, "");
        let _ = SetFocus(Some(st.input));
        refresh_adopt(st);
    }
}

/// 「删除选中」：移除当前高亮行（无选中时无操作）。
unsafe fn delete_selected(st: &EditState) {
    unsafe {
        let sel = SendMessageW(st.list, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0;
        // 无选中时 LB_GETCURSEL 返回 LB_ERR
        if sel >= 0 {
            let _ = SendMessageW(st.list, LB_DELETESTRING, Some(WPARAM(sel as usize)), Some(LPARAM(0)));
            refresh_adopt(st);
        }
    }
}

/// 「加入黑名单」：把上次全屏应用追加到列表（列表是编辑中的镜像，不直接写文件）。
unsafe fn adopt_last_fullscreen(st: &EditState) {
    unsafe {
        let Some(path) = st.adopt_path.as_deref() else { return };
        // 按钮在「已在黑名单」时已禁用，这里再判一次是为了不产生重复行
        if !list_items(st.list).iter().any(|line| blacklist::is_same_app(line, path)) {
            let mut buf = wide(path);
            let idx = SendMessageW(st.list, LB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(buf.as_mut_ptr() as isize))).0;
            if idx >= 0 {
                let _ = SendMessageW(st.list, LB_SETCURSEL, Some(WPARAM(idx as usize)), Some(LPARAM(0)));
            }
            Log::log(&format!("blacklist adopted: {path}"));
        }
        refresh_adopt(st);
    }
}

/// 同步「加入黑名单」按钮：列表里已有同一应用 → 文案改「已在黑名单」并禁用。
/// 状态由列表实时推导，不另存「已加入」标志；无上次全屏应用时该行不参与布局，直接跳过。
unsafe fn refresh_adopt(st: &EditState) {
    unsafe {
        let Some(path) = st.adopt_path.as_deref() else { return };
        let listed = list_items(st.list).iter().any(|line| blacklist::is_same_app(line, path));
        set_text(st.adopt_btn, if listed { ADOPT_BTN_LISTED } else { ADOPT_BTN_ADD });
        let _ = EnableWindow(st.adopt_btn, !listed);
    }
}

/// 更新「上次全屏应用」行：路径变化时刷文本、重排布局（首次从无记录变为有记录时该行才占位）并重算按钮态。
unsafe fn set_adopt_path(hwnd: HWND, st: &mut EditState, path: &str) {
    unsafe {
        if st.adopt_path.as_deref() == Some(path) {
            return;
        }
        st.adopt_path = Some(path.to_string());
        set_text(st.app_st, &format!("{ADOPT_LABEL}{path}"));
        layout(hwnd, st);
        refresh_adopt(st);
    }
}

/// 按列表整文件重写后销毁窗口（「保存并关闭」、✕、Alt+F4 共用）。
unsafe fn save_and_close(hwnd: HWND, st: &EditState) {
    unsafe {
        let items = list_items(st.list);
        blacklist::save_lines(&items);
        Log::log(&format!("blacklist saved from dialog: {} entries", items.len()));
        let _ = DestroyWindow(hwnd);
    }
}

/// 取出列表全部条目（即文件的原始行镜像）；空列表 → 空 Vec。
unsafe fn list_items(list: HWND) -> Vec<String> {
    unsafe {
        let count = SendMessageW(list, LB_GETCOUNT, Some(WPARAM(0)), Some(LPARAM(0))).0;
        let mut items = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(i as usize)), Some(LPARAM(0))).0;
            if len <= 0 {
                continue; // LB_ERR 或空行：跳过
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = SendMessageW(list, LB_GETTEXT, Some(WPARAM(i as usize)), Some(LPARAM(buf.as_mut_ptr() as isize))).0;
            if n > 0 {
                items.push(String::from_utf16_lossy(&buf[..n as usize]));
            }
        }
        items
    }
}

/// 父窗口 WM_PAINT：绘制右上角 ✕（悬停近黑，常态中灰）。
unsafe fn draw_close(st: &EditState, hwnd: HWND, hdc: HDC) {
    unsafe {
        let mut rc = close_rect(hwnd, st.dpi);
        let _ = SetBkMode(hdc, TRANSPARENT);
        // 悬浮 → 近黑（PRIMARY），常态 → 中灰（SECONDARY），与 about.rs 关闭按钮同一语义
        let _ = SetTextColor(hdc, if st.close_hover { COL_TEXT_PRIMARY } else { COL_TEXT_SECONDARY });
        let _ = SelectObject(hdc, st.font_title.into());
        let mut text = wide("✕");
        let _ = DrawTextW(hdc, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 自绘按钮（BS_OWNERDRAW）：填充式底 + 居中文字，按下时底色加深、禁用时文字转中灰；无边框无 3D 立体感。
unsafe fn draw_button(st: &EditState, dis: &DRAWITEMSTRUCT) {
    unsafe {
        let pressed = dis.itemState.0 & ODS_SELECTED.0 != 0;
        let disabled = dis.itemState.0 & ODS_DISABLED.0 != 0;
        let _ = FillRect(dis.hDC, &dis.rcItem, if pressed { st.btn_active_brush } else { st.btn_brush });
        let _ = SetBkMode(dis.hDC, TRANSPARENT);
        let _ = SetTextColor(dis.hDC, if disabled { COL_TEXT_SECONDARY } else { COL_EDIT_TEXT });
        let old = SelectObject(dis.hDC, st.font_body.into());
        let mut text = wide(&control_text(dis.hwndItem));
        let mut rc = dis.rcItem;
        let _ = DrawTextW(dis.hDC, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let _ = SelectObject(dis.hDC, old);
    }
}

/// 按客户区与 DPI 重排子控件（8px 基准网格）：顶部标题与说明固定高，
/// 底部按钮行贴页面底边、输入框在其上，列表吃掉剩余高度。
fn layout(hwnd: HWND, st: &EditState) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let cw = rc.right;
        let ch = rc.bottom;
        let k = st.dpi as i32;
        let m = MARGIN * k / 96;
        let line = LINE_H * k / 96;
        let gap = GAP * k / 96;
        let title_h = TITLE_H * k / 96;
        let btn_h = BTN_H * k / 96;
        let btn_gap = BTN_GAP * k / 96;
        let input_h = INPUT_H * k / 96;
        let w = cw - 2 * m;

        let mut y = m;
        place(st.title_st, m, y, w, title_h);
        y += title_h;
        for hint in [st.path_st, st.format_st, st.store_st] {
            place(hint, m, y, w, line);
            y += line;
        }
        // 「上次全屏应用」行：路径（中间省略）+ 右侧按钮，仅有记录时占一行
        if st.adopt_path.is_some() {
            let adopt_w = BTN_W_ADOPT * k / 96;
            place(st.app_st, m, y, w - adopt_w - gap, line);
            place(st.adopt_btn, cw - m - adopt_w, y, adopt_w, line);
            y += line;
        }
        y += gap;
        let btn_y = ch - m - btn_h;
        let input_y = btn_y - gap - input_h;
        place(st.list, m, y, w, input_y - gap - y);
        place(st.input, m, input_y, w, input_h);
        let save_x = cw - m - BTN_W_SAVE * k / 96;
        let del_x = save_x - btn_gap - BTN_W_DEL * k / 96;
        let add_x = del_x - btn_gap - BTN_W_ADD * k / 96;
        place(st.add_btn, add_x, btn_y, BTN_W_ADD * k / 96, btn_h);
        place(st.del_btn, del_x, btn_y, BTN_W_DEL * k / 96, btn_h);
        place(st.save_btn, save_x, btn_y, BTN_W_SAVE * k / 96, btn_h);
    }
}

/// 右上角自绘 ✕ 的热区（客户区坐标，贴右上角，与 about.rs 关闭按钮同位同尺寸）。
fn close_rect(hwnd: HWND, dpi: u32) -> RECT {
    let mut rc = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut rc); }
    let sz = CLOSE_SZ * dpi as i32 / 96;
    RECT { left: rc.right - sz, top: 0, right: rc.right, bottom: sz }
}

/// 客户区鼠标消息的 lparam → 客户区坐标（WM_MOUSEMOVE / WM_LBUTTONDOWN 等）。
fn client_pt(lparam: LPARAM) -> POINT {
    // 手工符号扩展：lparam 低/高 16 位是带符号的 x / y
    POINT { x: (lparam.0 as u16) as i16 as i32, y: ((lparam.0 >> 16) as u16) as i16 as i32 }
}

/// 非客户区鼠标消息的 lparam → 客户区坐标（WM_NCHITTEST 的 lparam 是屏幕坐标）。
fn screen_pt(hwnd: HWND, lparam: LPARAM) -> POINT {
    let mut pt = client_pt(lparam);
    unsafe { let _ = ScreenToClient(hwnd, &mut pt); }
    pt
}

fn pt_in_rect(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// 读控件文本（UTF-8）；控件为空 / 读取失败 → 空串。
unsafe fn control_text(h: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// UTF-8 → UTF-16 + 结尾 null（Win32 文本 API 用）。
fn wide(s: &str) -> Vec<u16> {
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    buf.push(0);
    buf
}

/// 运行时中文文本经 encode_utf16 填充（规范 §4.5）。
fn set_text(hwnd: HWND, s: &str) {
    let buf = wide(s);
    unsafe { let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(buf.as_ptr())); }
}

/// 创建标准子控件（创建失败降级为 invalid 句柄，布局时 SetWindowPos 自然失败）。
unsafe fn create_child(parent: HWND, class: PCWSTR, text: PCWSTR, style: WINDOW_STYLE, id: Option<isize>) -> HWND {
    unsafe {
        CreateWindowExW(WINDOW_EX_STYLE(0), class, text, style, 0, 0, 0, 0, Some(parent), id.map(|v| HMENU(v as _)), None, None)
            .unwrap_or_default()
    }
}

/// 移动子控件（保持 Z 序与激活状态不变）。
unsafe fn place(h: HWND, x: i32, y: i32, w: i32, hgt: i32) {
    unsafe {
        let _ = SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}
