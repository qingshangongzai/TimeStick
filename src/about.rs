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

//! 关于对话框：开发信息 / 本项目许可证（Mulan PSL v2）/ 第三方许可通知，
//! 后两者为只读可滚动可复制的完整全文。
//! 视觉遵循《"去线留白"设计语言》：无分割线、填充式许可框（极浅灰底代替边框）、
//! 字重与灰阶表达层级、8px 基准间距。
//! 无边框实现：保留 WS_CAPTION|WS_SYSMENU + 截留 WM_NCCALCSIZE（客户区=整窗），
//! DWM 阴影/动画保留，Win11 经 DWMWA_WINDOW_CORNER_PREFERENCE 取 8px 圆角。

use std::mem::size_of;
use std::ptr::addr_of_mut;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWM_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DEFAULT_PITCH, DEFAULT_QUALITY, DeleteObject, DrawTextW, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    EndPaint, FillRect, FONT_WEIGHT, FW_BOLD, FW_NORMAL, GetMonitorInfoW, GetSysColorBrush, HBRUSH,
    HDC, HFONT, HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, ScreenToClient, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::SS_ICON;
use windows::Win32::System::WindowsProgramming::MulDiv;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TrackMouseEvent, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::icon;
use crate::log::Log;

const DLG_W: i32 = 480; // 客户区宽（96 DPI 逻辑像素）
const DLG_H: i32 = 580; // 客户区高
const MARGIN: i32 = 24; // 页面边距（桌面端安全区）
const GAP: i32 = 8; // 组内间隙（8px 基准单元）
const GROUP_GAP: i32 = 24; // 信息组间距
const LINE_H: i32 = 20; // 正文本行高（9pt）
const NAME_H: i32 = 24; // 名称行高（12pt 粗体）
const OWN_LIC_BOX_H: i32 = 120; // 自身许可证全文框高（固定）
const CLOSE_SZ: i32 = 32; // 自绘关闭按钮边长（右上角贴顶的控制栏按钮）

// 去线留白色板：不透明度阶梯表达层级（白底），对比度均满足 WCAG AA
const COL_TEXT_PRIMARY: COLORREF = COLORREF(0x001A_1A1A); // 名称：近黑粗体
const COL_TEXT_LABEL: COLORREF = COLORREF(0x0033_3333); // 区标题：深灰
const COL_TEXT_SECONDARY: COLORREF = COLORREF(0x0075_7575); // 辅助信息：中灰
const COL_EDIT_TEXT: COLORREF = COLORREF(0x0026_2626); // 许可正文：85% 黑
const COL_EDIT_FILL: COLORREF = COLORREF(0x00F7_F7F7); // 填充式许可框底：约 3% 灰度阶差

/// 木兰官方应用指引第 1 步：补充软件名与版权人后的声明行
const MULAN_NOTICE: &str = "Copyright (c) 2026 青山公仔 · TimeStick（时间贴）is licensed under Mulan PSL v2.";
const LICENSE_TEXT: &str = include_str!("../LICENSE");
const NOTICES: &str = include_str!("../THIRD-PARTY-LICENSES");
const ABOUT_CLASS: PCWSTR = w!("TimeStickAbout");
const CLOSE_CLASS: PCWSTR = w!("TimeStickCloseBtn");

/// 对话框自有资源与子控件句柄（GWLP_USERDATA 挂载，WM_DESTROY 释放）。
/// 图标交给 icon_st（SS_ICON STATIC）持有：该控件销毁时系统自动销毁图标（MSDN
/// Static Control Default Message Processing），此处不可再 DestroyIcon（双重销毁→堆损坏）。
struct AboutState {
    font_name: HFONT,
    font_label: HFONT,
    font_body: HFONT,
    fill_brush: HBRUSH,
    /// owner（主窗口）句柄：WM_DESTROY 时直接通知，不依赖 GetParent
    owner: HWND,
    /// 标题栏小图标（自行加载/销毁；WM_SETICON 不转移所有权）
    icon_sm: HICON,
    dpi: u32,
    icon_st: HWND,
    name_st: HWND,
    ver_st: HWND,
    desc_st: HWND,
    own_label: HWND,
    notice_st: HWND,
    own_edit: HWND,
    tp_label: HWND,
    tp_edit: HWND,
    /// 自绘关闭按钮（无边框窗口无系统标题栏，右上角悬浮 ✕）
    close_btn: HWND,
}

/// 已打开则前置激活已有窗口，否则创建（owner=主窗口 → 随主窗口销毁自动清理）。
pub fn show(about_hwnd: &mut Option<HWND>, owner: HWND) {
    unsafe {
        if let Some(h) = *about_hwnd {
            let _ = SetForegroundWindow(h);
            return;
        }
        let hinst: HINSTANCE = GetModuleHandleW(None).ok().map(Into::into).unwrap_or_default();
        register_class(hinst);
        register_close_class(hinst);
        // 先按逻辑尺寸隐藏创建，之后按 DPI 调整物理尺寸（触发 WM_SIZE 完成首次布局）
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            ABOUT_CLASS,
            w!("关于 TimeStick"),
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
            Log::log("about: create window failed");
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
        setup(hwnd, owner, dpi);
        resize_and_center(hwnd, owner, dpi);
        *about_hwnd = Some(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

/// 采样窗口 DPI（失败时链系统 DPI，再兜底 96）。
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
            lpfnWndProc: Some(about_wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: ABOUT_CLASS,
            ..Default::default()
        };
        // 每次开窗都会注册，重复注册忽略（与 main.rs 惯例一致）
        let _ = RegisterClassExW(&wc);
    }
}

/// 注册自绘关闭按钮类（重复注册忽略，与主类惯例一致）。
unsafe fn register_close_class(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(close_wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: CLOSE_CLASS,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }
}

/// 创建字体 / 图标 / 全部子控件并挂载 AboutState（经 GWLP_USERDATA 回传）。
unsafe fn setup(hwnd: HWND, owner: HWND, dpi: u32) {
    unsafe {
        // 字重分层：名称 12pt 粗体 / 区标题 9pt 粗体 / 正文 9pt
        let font_name = create_font(dpi, FW_BOLD, 12);
        let font_label = create_font(dpi, FW_BOLD, 9);
        let font_body = create_font(dpi, FW_NORMAL, 9);
        let fill_brush = CreateSolidBrush(COL_EDIT_FILL);
        // 图标交给 SS_ICON STATIC 持有（控件销毁时系统自动销毁），WM_SETICON 不转移所有权；
        // 按 DPI 取物理尺寸帧（150% → 48px 恰命中 .ico 精确帧，避免 32px 位图被拉伸发糊）
        let icon = icon::load(icon::LARGE * dpi as i32 / 96).unwrap_or_default();
        // 标题栏小图标：按 DPI 取原生尺寸帧（150% → 24px），避免系统缩放模糊
        let icon_sm = icon::load(icon::SMALL * dpi as i32 / 96).unwrap_or_default();

        let mut st = Box::new(AboutState {
            font_name,
            font_label,
            font_body,
            fill_brush,
            owner,
            icon_sm,
            dpi,
            icon_st: HWND::default(),
            name_st: HWND::default(),
            ver_st: HWND::default(),
            desc_st: HWND::default(),
            own_label: HWND::default(),
            notice_st: HWND::default(),
            own_edit: HWND::default(),
            tp_label: HWND::default(),
            tp_edit: HWND::default(),
            close_btn: HWND::default(),
        });
        // 先挂载再建子控件：创建期的 WM_CTLCOLORSTATIC 也能取到配色状态
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, addr_of_mut!(*st) as isize);

        let st_basic = WS_CHILD | WS_VISIBLE;
        // 去线留白：不加 WS_BORDER（填充式底色代替边框）；
        // 不加 ES_AUTOHSCROLL：许可文本按框宽软换行（方案决策）
        let st_edit = WINDOW_STYLE((st_basic | WS_VSCROLL).0 | ES_MULTILINE as u32 | ES_READONLY as u32);

        // 顶部开发信息区
        st.icon_st = create_child(hwnd, w!("Static"), PCWSTR::null(), WINDOW_STYLE(st_basic.0 | SS_ICON.0), None);
        let _ = SendMessageW(st.icon_st, STM_SETICON, Some(WPARAM(icon.0 as usize)), Some(LPARAM(0)));
        let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(icon.0 as isize)));
        let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(st.icon_sm.0 as isize)));
        st.name_st = create_child(hwnd, w!("Static"), w!("TimeStick 时间贴"), st_basic, None);
        st.ver_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        set_text(
            st.ver_st,
            &format!("版本 {} · 开发 {}", env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_AUTHORS")),
        );
        st.desc_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        set_text(st.desc_st, env!("CARGO_PKG_DESCRIPTION"));

        // 本项目许可证区：标题 + 声明行 + 固定高全文框
        st.own_label = create_child(hwnd, w!("Static"), w!("本项目许可证"), st_basic, None);
        st.notice_st = create_child(hwnd, w!("Static"), PCWSTR::null(), st_basic, None);
        set_text(st.notice_st, MULAN_NOTICE);
        st.own_edit = create_child(hwnd, w!("Edit"), PCWSTR::null(), st_edit, None);
        set_text(st.own_edit, LICENSE_TEXT);

        // 第三方许可区：标题 + 弹性高全文框
        st.tp_label = create_child(hwnd, w!("Static"), w!("第三方开源许可"), st_basic, None);
        st.tp_edit = create_child(hwnd, w!("Edit"), PCWSTR::null(), st_edit, None);
        set_text(st.tp_edit, NOTICES);

        // 自绘关闭按钮（无边框窗口无系统标题栏；位置由 layout 摆放）
        st.close_btn = create_child(hwnd, CLOSE_CLASS, PCWSTR::null(), st_basic, None);

        apply_fonts(&st);

        // 所有权转移给窗口 userdata（WM_DESTROY 时 Box::from_raw 取回释放）。
        // 若不转移，Box 会随函数返回被 drop → userdata 悬垂 → 二次释放致堆损坏
        let _ = Box::into_raw(st);
    }
}

/// 向全部文本子控件下发当前三档字体。
unsafe fn apply_fonts(st: &AboutState) {
    unsafe {
        let set_font = |h: HWND, f: HFONT| {
            let _ = SendMessageW(h, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
        };
        set_font(st.name_st, st.font_name);
        set_font(st.own_label, st.font_label);
        set_font(st.tp_label, st.font_label);
        for h in [st.ver_st, st.desc_st, st.notice_st, st.own_edit, st.tp_edit] {
            set_font(h, st.font_body);
        }
    }
}

/// 按新 DPI 重建三档字体（先建新并下发控件，再删旧，避免控件引用悬垂）。
unsafe fn rebuild_fonts(st: &mut AboutState, dpi: u32) {
    unsafe {
        let old = [st.font_name, st.font_label, st.font_body];
        st.font_name = create_font(dpi, FW_BOLD, 12);
        st.font_label = create_font(dpi, FW_BOLD, 9);
        st.font_body = create_font(dpi, FW_NORMAL, 9);
        apply_fonts(st);
        for f in old {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
    }
}

/// 按新 DPI 重载图标（同 rebuild_fonts 的"先换绑再销毁旧"次序）：
/// 大图标经 STM_SETICON 换绑 icon_st——返回值是旧句柄，由本方销毁
/// （控件只销毁自己当前持有的图标，MSDN STM_SETICON Return value）；
/// 窗口大/小图标先换绑到新句柄再销毁旧句柄，避免标题栏引用已销毁句柄。
unsafe fn reload_icons(st: &mut AboutState, hwnd: HWND, dpi: u32) {
    unsafe {
        let new_icon = icon::load(icon::LARGE * dpi as i32 / 96).unwrap_or_default();
        if new_icon.is_invalid() {
            return;
        }
        let old = SendMessageW(
            st.icon_st,
            STM_SETICON,
            Some(WPARAM(new_icon.0 as usize)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(new_icon.0 as isize)));
        if old.0 != 0 {
            let _ = DestroyIcon(HICON(old.0 as *mut _));
        }
        let new_sm = icon::load(icon::SMALL * dpi as i32 / 96).unwrap_or_default();
        if !new_sm.is_invalid() {
            let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(new_sm.0 as isize)));
            if !st.icon_sm.is_invalid() {
                let _ = DestroyIcon(st.icon_sm);
            }
            st.icon_sm = new_sm;
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
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
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

unsafe extern "system" fn about_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let st_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AboutState;
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
            // 顶部图标/名称带可拖动（STATIC 对鼠标 HTTRANSPARENT，带内标签天然可拖）；
            // 关闭按钮是子窗口先行命中测试，不受此影响。lparam 为屏幕坐标，手工符号扩展
            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: (lparam.0 as u16) as i16 as i32,
                    y: ((lparam.0 >> 16) as u16) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let band = (MARGIN + icon::LARGE) * st.dpi as i32 / 96;
                if pt.y >= 0 && pt.y < band && pt.x >= 0 && pt.x < rc.right {
                    LRESULT(HTCAPTION as isize)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            // 关闭按钮经此间接取区标题字体：父窗口 DPI 重建字体后下次重绘自动跟随
            WM_GETFONT => LRESULT(st.font_label.0 as isize),
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            // 跨显示器拖动到不同缩放率：按新 DPI 重建字体，并套用系统建议矩形
            // （lparam 指向按新旧 DPI 比例缩放后的窗口矩形，保持窗口在光标下方）
            WM_DPICHANGED => {
                let new_dpi = (wparam.0 >> 16) as u32;
                if new_dpi != 0 && new_dpi != st.dpi && lparam.0 != 0 {
                    st.dpi = new_dpi;
                    rebuild_fonts(st, new_dpi);
                    reload_icons(st, hwnd, new_dpi);
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
            // 只读 EDIT 与 STATIC 都经此消息取色（去线留白：STATIC 文字透明无底条，EDIT 浅灰填充）
            WM_CTLCOLORSTATIC => {
                let hdc = HDC(wparam.0 as *mut _);
                let ctl = HWND(lparam.0 as *mut _);
                on_ctl_color(st, hdc, ctl)
            }
            WM_DESTROY => {
                let _ = DeleteObject(HGDIOBJ(st.font_name.0));
                let _ = DeleteObject(HGDIOBJ(st.font_label.0));
                let _ = DeleteObject(HGDIOBJ(st.font_body.0));
                let _ = DeleteObject(HGDIOBJ(st.fill_brush.0));
                // 图标不在此销毁：icon_st（SS_ICON STATIC）销毁时系统自动销毁（双重销毁曾致堆损坏）；
                // 小图标无控件持有，由本状态自行销毁
                if !st.icon_sm.is_invalid() {
                    let _ = DestroyIcon(st.icon_sm);
                }
                // 同步直调主窗口清空 about_hwnd（owner 已销毁则忽略，进程即将结束无泄漏）
                let _ = SendMessageW(st.owner, crate::WM_APP_ABOUT_CLOSED, Some(WPARAM(0)), Some(LPARAM(0)));
                // 清空 userdata 后取回 Box 释放（销毁后期消息经判空兜底）
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(st_ptr));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// 自绘关闭按钮 wndproc：状态仅一个 hover 布尔（GWLP_USERDATA 存 isize 0/1）。
/// ✕ 字形 U+2715；若 Segoe UI 缺字形显示方框，换成 w!("×")（U+00D7，验收时目检裁定）。
unsafe extern "system" fn close_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEMOVE => {
                if GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 0 {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 1);
                    // 进入 hover 态后跟踪离开，TME_LEAVE 会在鼠标移出时发 WM_MOUSELEAVE
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Ok(parent) = GetParent(hwnd) {
                    let _ = DestroyWindow(parent);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                // 底色与对话框一致（COLOR_WINDOW）→ 按钮呈"悬浮 ✕"而非色块
                let _ = FillRect(hdc, &ps.rcPaint, GetSysColorBrush(COLOR_WINDOW));
                // 字体经父窗口 WM_GETFONT 间接获取：父窗口 DPI 重建字体后自动跟随
                let mut font = None;
                if let Ok(parent) = GetParent(hwnd) {
                    let f = SendMessageW(parent, WM_GETFONT, Some(WPARAM(0)), Some(LPARAM(0))).0 as *mut _;
                    font = Some(HGDIOBJ(f));
                }
                if let Some(font) = font.filter(|f| !f.is_invalid()) {
                    let _ = SelectObject(hdc, font);
                }
                let _ = SetBkMode(hdc, TRANSPARENT);
                // 悬浮 → 近黑（PRIMARY），常态 → 中灰（SECONDARY）
                let hovered = GetWindowLongPtrW(hwnd, GWLP_USERDATA) != 0;
                let _ = SetTextColor(hdc, if hovered { COL_TEXT_PRIMARY } else { COL_TEXT_SECONDARY });
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let mut text: Vec<u16> = "✕".encode_utf16().collect();
                text.push(0);
                let _ = DrawTextW(hdc, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// WM_CTLCOLORSTATIC 统一配色：STATIC 透明底 + 按层级给文字色；只读 EDIT 浅灰填充底。
unsafe fn on_ctl_color(st: &AboutState, hdc: HDC, ctl: HWND) -> LRESULT {
    unsafe {
        if ctl == st.own_edit || ctl == st.tp_edit {
            let _ = SetTextColor(hdc, COL_EDIT_TEXT);
            let _ = SetBkColor(hdc, COL_EDIT_FILL);
            LRESULT(st.fill_brush.0 as isize)
        } else {
            // 透明背景模式：文字后面不画底色（无灰黑条）
            let _ = SetBkMode(hdc, TRANSPARENT);
            let color = if ctl == st.name_st {
                COL_TEXT_PRIMARY
            } else if ctl == st.own_label || ctl == st.tp_label {
                COL_TEXT_LABEL
            } else {
                COL_TEXT_SECONDARY
            };
            let _ = SetTextColor(hdc, color);
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
    }
}

/// 按客户区与 DPI 重排子控件（8px 基准网格）：顶部信息组固定高，
/// 自身许可框固定高，第三方许可框吃掉剩余高度（延伸到底部边距）。
fn layout(hwnd: HWND, st: &AboutState) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let cw = rc.right;
        let ch = rc.bottom;
        let k = st.dpi as i32;
        let m = MARGIN * k / 96;
        let line = LINE_H * k / 96;
        let gap = GAP * k / 96;
        let group = GROUP_GAP * k / 96;
        let name_h = NAME_H * k / 96;
        let w = cw - 2 * m;
        let icon_sz = icon::LARGE * k / 96;
        let own_h = OWN_LIC_BOX_H * k / 96;
        let close_sz = CLOSE_SZ * k / 96;

        let mut y = m;
        // 关闭按钮：右上角贴边（控制栏惯例——顶部/右边缘与窗口平齐，非页面边距）
        place(st.close_btn, cw - close_sz, 0, close_sz, close_sz);
        // 顶部信息组：图标与名称并排（组内 8px），版本 / 描述整行
        place(st.icon_st, m, y, icon_sz, icon_sz);
        place(st.name_st, m + icon_sz + gap, y + (icon_sz - name_h) / 2, w - icon_sz - gap, name_h);
        y += icon_sz + gap;
        place(st.ver_st, m, y, w, line);
        y += line;
        place(st.desc_st, m, y, w, line);
        y += line + group;
        // 本项目许可证组：标题 + 声明行 + 固定高全文框
        place(st.own_label, m, y, w, line);
        y += line;
        place(st.notice_st, m, y, w, line);
        y += line + gap;
        place(st.own_edit, m, y, w, own_h);
        y += own_h + group;
        // 第三方许可组：标题 + 弹性高全文框（吃掉剩余高度，延伸到底部边距）
        place(st.tp_label, m, y, w, line);
        y += line;
        place(st.tp_edit, m, y, w, ch - m - y);
    }
}

/// 创建标准子控件（创建失败降级为 invalid 句柄，布局时 SetWindowPos 自然失败）。
unsafe fn create_child(
    parent: HWND,
    class: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    id: Option<isize>,
) -> HWND {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            text,
            style,
            0,
            0,
            0,
            0,
            Some(parent),
            id.map(|v| HMENU(v as _)),
            None,
            None,
        )
        .unwrap_or_default()
    }
}

/// 运行时中文文本经 encode_utf16 填充（规范 §4.5）。
fn set_text(hwnd: HWND, s: &str) {
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    buf.push(0);
    unsafe { let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(buf.as_ptr())); }
}

/// 移动子控件（保持 Z 序与激活状态不变）。
unsafe fn place(h: HWND, x: i32, y: i32, w: i32, hgt: i32) {
    unsafe {
        let _ = SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}
