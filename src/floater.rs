//! 悬浮窗：无边框分层窗口（WS_EX_LAYERED|TOPMOST|TOOLWINDOW|NOACTIVATE），
//! 支持拖动、固定（鼠标穿透）、关闭（隐藏，后台继续常驻；托盘可随时唤回）。固定状态仅在本次运行内保留。
//!
//! 悬停展开：鼠标未悬停时仅显示居中时间（无背景无按钮），悬停时亮出背景与按钮。
//! 非固定模式由 WM_MOUSEMOVE/TrackMouseEvent 即时驱动；固定模式（鼠标穿透）收不到
//! 鼠标消息，由 250ms 轮询驱动同一状态，兼作 1.5s 自动解除固定。
//!
//! 窗口与 D2D 渲染器延迟到首次全屏显示时创建（ensure_created），
//! 静默驻留期间不加载 D2D/DWrite、不持有窗口，常驻内存最小化。

use std::mem::size_of;

use windows::core::{w, PCWSTR, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetDC, GetMonitorInfoW, GetPixel, MonitorFromWindow, ReleaseDC, HBRUSH, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::{HOVER_DEFAULT, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log::Log;
use crate::render::Renderer;

const ID_TIMER: usize = 1; // 时间刷新（1000ms）
const ID_HOVER: usize = 2; // 固定模式悬停检测（250ms）
const HOVER_TICK_MS: u32 = 250;
const HOVER_UNPIN_MS: u32 = 1500; // 悬停超过 1.5s 自动解除固定
const MARGIN: i32 = 24; // 默认位置距屏幕边缘边距

/// 悬浮窗组件。
pub struct Floater {
    hinst: HINSTANCE,
    /// 内嵌图标句柄（与主窗口类共用，所有权在 App，退出时统一销毁）
    hicon: HICON,
    hicon_sm: HICON,
    hwnd: Option<HWND>,
    renderer: Option<Renderer>,
    /// 固定模式：隐藏按钮 + 鼠标穿透（WS_EX_TRANSPARENT）
    pinned: bool,
    /// 鼠标悬停窗口：悬停展开完整 UI；固定模式下兼作“即将解除”计时反馈
    hovering: bool,
    hover_ms: u32,
    visible: bool,
    /// 当前窗口左上角位置（会话内记忆，拖动后更新）
    pos: Option<POINT>,
    dragging: bool,
    drag_start: POINT,
    win_pos_at_drag: POINT,
}

impl Floater {
    pub fn new(hinst: HINSTANCE, hicon: HICON, hicon_sm: HICON) -> Self {
        Floater {
            hinst,
            hicon,
            hicon_sm,
            hwnd: None,
            renderer: None,
            pinned: false,
            hovering: false,
            hover_ms: 0,
            visible: false,
            pos: None,
            dragging: false,
            drag_start: POINT::default(),
            win_pos_at_drag: POINT::default(),
        }
    }

    /// 取出窗口句柄（退出清理用）。
    pub fn take_hwnd(&mut self) -> Option<HWND> {
        self.hwnd.take()
    }

    /// 首次显示时创建窗口与渲染器（延迟加载 D2D/DWrite，静默驻留内存最小化）。
    fn ensure_created(&mut self) -> Result<()> {
        if self.hwnd.is_some() {
            return Ok(());
        }
        let class = w!("TimeStickFloater");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(floater_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: self.hinst,
            hIcon: self.hicon,
            // 类光标必须显式指定：NULL 时鼠标移入窗口系统会显示“忙/等待”光标
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class,
            hIconSm: self.hicon_sm,
        };
        unsafe { let _ = RegisterClassExW(&wc); }

        let renderer = Renderer::new()?;
        let (w, h) = renderer.size;
        // WS_EX_NOACTIVATE：点击时钟不抢游戏焦点（全屏检测不受干扰）
        let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hwnd = unsafe {
            CreateWindowExW(ex, class, w!("TimeStick"), WS_POPUP, 0, 0, w, h, None, None, Some(self.hinst), None)?
        };
        // WndProc 经 GWLP_USERDATA 获取实例（Floater 位于 Box<App> 内，地址稳定）
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, std::ptr::addr_of_mut!(*self) as isize); }
        self.renderer = Some(renderer);
        self.hwnd = Some(hwnd);
        Ok(())
    }

    /// 全屏进入：显示悬浮窗（位置 = 记忆位置或右下角）。
    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        if let Err(e) = self.ensure_created() {
            Log::log(&format!("floater create failed: {e}"));
            return;
        }
        self.visible = true;
        let pos = self.pos.unwrap_or_else(|| self.default_pos());
        self.pos = Some(pos);
        Log::log(&format!("floater shown at ({}, {})", pos.x, pos.y));
        let hwnd = self.hwnd.unwrap();
        self.render_now(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), ID_TIMER, 1000, None); // 每秒刷新时间
            // hide() 会杀掉 ID_HOVER；固定状态跨全屏会话保留，须按 pinned 恢复，
            // 否则悬停 1.5s 自动解除固定与固定模式悬停展开反馈永久失效
            if self.pinned {
                SetTimer(Some(hwnd), ID_HOVER, HOVER_TICK_MS, None);
            }
        }
        // 窗口恰好出现在光标下方时立即展开（否则要等鼠标移动才触发 WM_MOUSEMOVE）
        self.on_hover_move();
    }

    /// 全屏退出 / 点击 X：隐藏，后台继续运行等待下次全屏（或托盘唤回）。
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.dragging = false;
        self.hovering = false;
        self.hover_ms = 0;
        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            let _ = ReleaseCapture();
            let _ = KillTimer(Some(hwnd), ID_TIMER);
            let _ = KillTimer(Some(hwnd), ID_HOVER);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        Log::log("floater hidden");
    }

    /// 全局热键解除固定（鼠标穿透开启后窗口收不到鼠标消息，只能靠热键）。
    pub fn unpin(&mut self) {
        if self.pinned {
            self.set_pinned(false);
            Log::log("hotkey: unpinned");
        }
    }

    /// 切换固定状态：按位或/按位与取反 WS_EX_TRANSPARENT，实现鼠标穿透。
    /// 固定后启动悬停检测定时器：鼠标停留在窗口上 1.5s 自动解除（穿透模式下
    /// 窗口收不到鼠标消息，但 GetCursorPos 轮询不受影响，比 WH_MOUSE_LL 钩子更轻）。
    fn set_pinned(&mut self, pinned: bool) {
        if self.pinned == pinned {
            return;
        }
        self.pinned = pinned;
        self.hovering = false;
        self.hover_ms = 0;
        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let new_ex = if pinned {
                ex | WS_EX_TRANSPARENT.0 as isize
            } else {
                ex & !(WS_EX_TRANSPARENT.0 as isize)
            };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);
            // FRAMECHANGED：使扩展样式变更立即生效
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            if pinned {
                SetTimer(Some(hwnd), ID_HOVER, HOVER_TICK_MS, None);
            } else {
                let _ = KillTimer(Some(hwnd), ID_HOVER);
                // 解除固定后光标仍在窗口上则恢复展开态（固定模式由轮询驱动，无 WM_MOUSEMOVE）
                if self.visible && self.cursor_over(hwnd) {
                    self.hovering = true;
                    self.arm_leave_tracking(hwnd);
                }
            }
        }
        self.render_now(hwnd); // 重绘以切换悬停展开/收起
        Log::log(if pinned { "pinned (mouse passthrough on)" } else { "unpinned (mouse passthrough off)" });
    }

    /// 悬停检测（固定模式下每 250ms 触发一次）。
    /// 鼠标在窗口矩形内累计时长，超过阈值自动解除固定；移出则清零。
    fn on_hover_tick(&mut self) {
        if !self.pinned || !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        let inside = self.cursor_over(hwnd);
        if inside {
            self.hover_ms += HOVER_TICK_MS;
            if self.hover_ms >= HOVER_UNPIN_MS {
                Log::log("hover timeout: unpinning");
                self.set_pinned(false);
                return;
            }
        } else {
            self.hover_ms = 0;
        }
        if inside != self.hovering {
            self.hovering = inside;
            self.render_now(hwnd); // 图钉实心提示浮现/消失
        }
    }

    /// 光标当前是否位于窗口矩形内（非固定模式首帧检测与固定模式轮询共用）。
    fn cursor_over(&self, hwnd: HWND) -> bool {
        unsafe {
            let mut cur = POINT::default();
            let _ = GetCursorPos(&mut cur);
            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            cur.x >= wr.left && cur.x < wr.right && cur.y >= wr.top && cur.y < wr.bottom
        }
    }

    /// 悬停移动（非固定模式，WM_MOUSEMOVE）：展开完整 UI。
    /// 每次移动重新武装离开跟踪（幂等），移出窗口时系统发 WM_MOUSELEAVE 收起。
    fn on_hover_move(&mut self) {
        if self.pinned || !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        self.arm_leave_tracking(hwnd);
        if !self.hovering {
            self.hovering = true;
            self.hover_ms = 0;
            self.render_now(hwnd);
        }
    }

    /// 悬停离开（WM_MOUSELEAVE）：收起为纯文字。
    fn on_mouseleave(&mut self) {
        if self.pinned || !self.visible || !self.hovering {
            return;
        }
        self.hovering = false;
        self.hover_ms = 0;
        if let Some(hwnd) = self.hwnd {
            self.render_now(hwnd);
        }
    }

    /// 武装鼠标离开跟踪：鼠标移出窗口矩形时系统发 WM_MOUSELEAVE。
    fn arm_leave_tracking(&self, hwnd: HWND) {
        unsafe {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: HOVER_DEFAULT,
            };
            let _ = TrackMouseEvent(&mut tme);
        }
    }

    fn default_pos(&self) -> POINT {
        unsafe {
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let mon = MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY);
            // 仅 show() 在 ensure_created 成功后调用，renderer 必为 Some（规范豁免的 unwrap 点）
            let (w, h) = self.renderer.as_ref().unwrap().size;
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let m = mi.rcMonitor;
                POINT { x: m.right - w - MARGIN, y: m.bottom - h - MARGIN }
            } else {
                POINT { x: 100, y: 100 }
            }
        }
    }

    fn render_now(&self, hwnd: HWND) {
        let Some(renderer) = &self.renderer else { return };
        let t = now_hhmm();
        let dark_bg = self.background_is_dark(hwnd);
        renderer.render(&t, self.pinned, self.hovering, dark_bg);
        let pos = self.pos.unwrap_or(POINT { x: 0, y: 0 });
        renderer.present(hwnd, pos);
    }

    /// 采样悬浮窗上方背景的亮度，决定配色（浅色背景 → 黑色文字 + 白底）。
    /// 窗口矩形上沿 4px 处取 5 个采样点（窗口贴顶时改采下沿外侧）。
    fn background_is_dark(&self, hwnd: HWND) -> bool {
        unsafe {
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return true;
            }
            let hdc = GetDC(None);
            if hdc.is_invalid() {
                return true;
            }
            let sample_y = if wr.top - 4 >= 0 { wr.top - 4 } else { wr.bottom + 4 };
            let cx = (wr.left + wr.right) / 2;
            let mut lum_sum: i64 = 0;
            let mut n: i64 = 0;
            for dx in [-48i32, -24, 0, 24, 48] {
                let c = GetPixel(hdc, cx + dx, sample_y);
                if c.0 != 0xFFFF_FFFF {
                    // COLORREF 为 0x00BBGGRR，失败返回 CLR_INVALID(0xFFFFFFFF)
                    let r = (c.0 & 0xff) as i64;
                    let g = ((c.0 >> 8) & 0xff) as i64;
                    let b = ((c.0 >> 16) & 0xff) as i64;
                    lum_sum += (r * 299 + g * 587 + b * 114) / 1000;
                    n += 1;
                }
            }
            let _ = ReleaseDC(None, hdc);
            if n == 0 {
                crate::log::Log::log("bg sample: all GetPixel failed, treat as dark");
                return true;
            }
            let lum = lum_sum / n;
            crate::log::Log::log(&format!("bg sample lum={} (dark={})", lum, lum < 128));
            lum < 128
        }
    }

    fn on_lbuttondown(&mut self, lparam: LPARAM) {
        if self.pinned || !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        let Some(renderer) = &self.renderer else { return };
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        let p = POINT { x, y };
        if pt_in_rect(&renderer.btn_pin, p) {
            self.set_pinned(true);
            return;
        }
        if pt_in_rect(&renderer.btn_close, p) {
            self.hide();
            return;
        }
        // 其余区域：开始拖动
        unsafe {
            let mut cur = POINT::default();
            let _ = GetCursorPos(&mut cur);
            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            self.dragging = true;
            self.drag_start = cur;
            self.win_pos_at_drag = POINT { x: wr.left, y: wr.top };
            SetCapture(hwnd);
        }
    }

    fn on_mousemove(&mut self) {
        if !self.dragging {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            let mut cur = POINT::default();
            let _ = GetCursorPos(&mut cur);
            let pos = POINT {
                x: self.win_pos_at_drag.x + (cur.x - self.drag_start.x),
                y: self.win_pos_at_drag.y + (cur.y - self.drag_start.y),
            };
            self.pos = Some(pos);
            let _ = SetWindowPos(hwnd, None, pos.x, pos.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    fn on_lbuttonup(&mut self) {
        if self.dragging {
            self.dragging = false;
            unsafe { let _ = ReleaseCapture(); }
            // 拖拽期间鼠标可能移出窗口（capture 不触发 WM_MOUSELEAVE），松手时校验收起；
            // dragging 仅在 on_lbuttondown 取得 hwnd 后置位，此处 hwnd 必存在（规范豁免的 unwrap 点）
            let hwnd = self.hwnd.unwrap();
            if !self.pinned && self.hovering && !self.cursor_over(hwnd) {
                self.hovering = false;
                self.hover_ms = 0;
                self.render_now(hwnd);
            }
        }
    }
}

fn pt_in_rect(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

fn now_hhmm() -> String {
    let st = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", st.wHour, st.wMinute)
}

unsafe extern "system" fn floater_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let this = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Floater;
        if this.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let floater = &mut *this;
        match msg {
            WM_LBUTTONDOWN => {
                floater.on_lbuttondown(lparam);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                floater.on_mousemove();
                floater.on_hover_move();
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                floater.on_mouseleave();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                floater.on_lbuttonup();
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                floater.dragging = false;
                LRESULT(0)
            }
            WM_TIMER => {
                if wparam.0 == ID_HOVER {
                    floater.on_hover_tick();
                } else if let Some(hwnd) = floater.hwnd {
                    floater.render_now(hwnd);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
