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

//! 系统托盘：优先加载 exe 内嵌图标（与文件图标同一设计源，见 src/icon.rs），
//! 失败时回退到 GDI 绘制的表盘；右键弹出菜单【显示悬浮窗】【直接退出】。

use std::mem::size_of;
use std::ptr::null_mut;

use windows::core::{w, BOOL, HRESULT, Result};
use windows::Win32::Foundation::{COLORREF, GetLastError, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, Ellipse, GetStockObject, LineTo, MoveToEx, SelectObject, BI_RGB, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HBRUSH, HBITMAP, HGDIOBJ, HOLLOW_BRUSH, HPEN, PS_SOLID,
    RGBQUAD,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    ICONINFO, MF_STRING, MENU_ITEM_FLAGS, PostMessageW, SetForegroundWindow, TPM_RIGHTBUTTON,
    TrackPopupMenu, HICON, WM_NULL, WM_RBUTTONUP,
};

use crate::icon;
use crate::log::Log;

pub const TRAY_CMD_SHOW: usize = 1000; // 显示悬浮窗
pub const TRAY_CMD_EXIT: usize = 1001; // 直接退出
const TRAY_ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Result<Self> {
        // 优先加载内嵌图标（与 exe 图标同一设计源，见 src/icon.rs）；
        // 失败时回退到 GDI 绘制的表盘，保证托盘图标永不为空。
        let icon = match icon::load(icon::LARGE) {
            Some(h) => {
                Log::log("tray icon: embedded resource");
                h
            }
            None => {
                Log::log("tray icon: fallback to GDI clock");
                create_clock_icon()?
            }
        };
        let mut tip = [0u16; 128];
        for (i, c) in "TimeStick 时间贴".encode_utf16().take(127).enumerate() {
            tip[i] = c;
        }
        let nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: crate::WM_APP_TRAY,
            hIcon: icon,
            szTip: tip,
            ..Default::default()
        };
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() {
            return Err(HRESULT::from_win32(unsafe { GetLastError().0 }).into());
        }
        Log::log("tray icon added");
        Ok(Tray { hwnd, icon, added: true })
    }

    /// 处理托盘回调消息（uCallbackMessage = WM_APP_TRAY，lparam 为鼠标消息）。
    pub fn on_message(&self, lparam: LPARAM) {
        if (lparam.0 as u32) == WM_RBUTTONUP {
            self.show_menu();
        }
    }

    fn show_menu(&self) {
        unsafe {
            // 菜单正常关闭需要窗口处于前台
            let _ = SetForegroundWindow(self.hwnd);
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MENU_ITEM_FLAGS(MF_STRING.0), TRAY_CMD_SHOW, w!("显示悬浮窗"));
            let _ = AppendMenuW(menu, MENU_ITEM_FLAGS(MF_STRING.0), TRAY_CMD_EXIT, w!("直接退出"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // 不启用 TPM_RETURNCMD/TPM_NONOTIFY：菜单命令经 WM_COMMAND 送达主窗口
            let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, None, self.hwnd, None);
            // 补发空消息完成前台切换（KB135788 标准套路），否则下次右键托盘可能需点两次
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                ..Default::default()
            };
            unsafe { let _ = Shell_NotifyIconW(NIM_DELETE, &nid); }
            self.added = false;
            Log::log("tray icon removed");
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        unsafe { let _ = DestroyIcon(self.icon); }
    }
}

/// 用 GDI 绘制一个表盘图标（白色圆环 + 指针 + 中心点），
/// 再经 CreateIconIndirect 转为 HICON —— 免去 .ico 资源文件，保持单 exe 自包含。
fn create_clock_icon() -> Result<HICON> {
    unsafe {
        let s = icon::LARGE;
        let mut bits: *mut std::ffi::c_void = null_mut();
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: s,
                biHeight: -s,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let hbm = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let hdc = CreateCompatibleDC(None);
        let old = SelectObject(hdc, HGDIOBJ(hbm.0));

        // 清空为透明
        std::ptr::write_bytes(bits, 0, (s * s) as usize);

        // 白色轮廓 + 指针 + 中心点
        let pen: HPEN = CreatePen(PS_SOLID, 2, COLORREF(0x00FF_FFFF));
        let _ = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
        let _ = Ellipse(hdc, 3, 3, s - 3, s - 3);
        let _ = MoveToEx(hdc, 16, 16, None);
        let _ = LineTo(hdc, 16, 6);
        let _ = MoveToEx(hdc, 16, 16, None);
        let _ = LineTo(hdc, 25, 16);
        let brush: HBRUSH = CreateSolidBrush(COLORREF(0x00FF_FFFF));
        let _ = SelectObject(hdc, HGDIOBJ(brush.0));
        let _ = Ellipse(hdc, 14, 14, 18, 18);

        // GDI 不写入 Alpha 通道：手动为所有非黑像素补上 255 Alpha
        let px = bits as *mut u32;
        for i in 0..(s * s) as usize {
            let p = *px.add(i);
            if p & 0x00FF_FFFF != 0 {
                *px.add(i) = p | 0xFF00_0000;
            }
        }

        // 掩码位图全 0（全部不透明），透明与否由颜色位图 Alpha 决定
        let mask: HBITMAP = CreateBitmap(s, s, 1, 1, None);
        let info = ICONINFO {
            fIcon: BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: hbm,
        };
        let icon = CreateIconIndirect(&info)?;

        let _ = SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        let _ = DeleteDC(hdc);
        Ok(icon)
    }
}
