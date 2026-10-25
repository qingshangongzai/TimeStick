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

//! 悬浮窗：无边框分层窗口（WS_EX_LAYERED|TOPMOST|TOOLWINDOW|NOACTIVATE），
//! 支持拖动、固定（鼠标穿透）、关闭（隐藏，后台继续常驻；托盘可随时唤回）。固定状态仅在本次运行内保留。
//!
//! 悬停展开：鼠标未悬停时仅显示居中时间（无背景无按钮），悬停时亮出背景与按钮。
//! 按钮依次为图钉 / 拉黑 / 关闭：拉黑把当前全屏应用写入黑名单（见 blacklist.rs）后立即隐藏，
//! 下次该应用全屏不再显示（隐藏的判定在 detect.rs）。
//! 非固定模式由 WM_MOUSEMOVE/TrackMouseEvent 即时驱动；固定模式（鼠标穿透）收不到
//! 鼠标消息，由 250ms 轮询驱动同一状态，兼作 1.5s 自动解除固定。
//!
//! 窗口与 D2D 渲染器延迟到首次全屏显示时创建（ensure_created），隐藏时销毁（hide），
//! 静默驻留期间不加载 D2D/DWrite、不持有窗口，常驻内存最小化。

use std::mem::size_of;

use windows::core::{w, PCWSTR, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetDC, GetMonitorInfoW, GetPixel, MonitorFromWindow, ReleaseDC, HBRUSH, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::{HOVER_DEFAULT, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::blacklist;
use crate::detect;
use crate::log::Log;
use crate::position;
use crate::render::{self, Renderer};
use crate::settings;

const ID_TIMER: usize = 1; // 时间刷新（每次触发后按距下次显示变化的毫秒数重排）
const ID_HOVER: usize = 2; // 固定模式悬停检测（250ms）
const ID_BG: usize = 3; // 背景亮度采样（与时间刷新解耦）
const HOVER_TICK_MS: u32 = 250;
const BG_TICK_MS: u32 = 250; // 采样周期：配色翻转延迟 ≤250ms
const HOVER_UNPIN_MS: u32 = 1500; // 悬停超过 1.5s 自动解除固定
const TIMER_MIN_MS: u32 = 50; // 时间刷新定时器重排下限：防 0 值忙等
const MARGIN: i32 = 24; // 默认位置距屏幕边缘边距
const DARK_LUMA: i64 = 96; // 迟滞下沿：低于此判暗背景（白字）
const LIGHT_LUMA: i64 = 160; // 迟滞上沿：高于此判亮背景（黑字）
const BG_SAMPLE_GAP: i32 = 4; // 采样点距窗口边缘的像素间距
const BG_SAMPLE_DY: i32 = 8; // 左右两侧采样点相对窗口垂直中心的偏移
const BG_SAMPLE_DX: [i32; 5] = [-48, -24, 0, 24, 48]; // 上沿 5 点的水平偏移

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
    /// 显示秒（HH:MM:SS）；false 为 HH:MM。勾选状态唯一真源，托盘菜单每次弹出时读取
    show_seconds: bool,
    /// 七段数码管显示风格；false 为默认字体。勾选状态唯一真源，托盘菜单每次弹出时读取
    seven_seg: bool,
    /// 显示大小档位索引（render::FONT_LEVELS 下标）；勾选状态唯一真源，托盘菜单每次弹出时读取
    size_level: u8,
    /// 背景亮度判定缓存（仅 `sample_background` 写入）；构造 `true`，与"采样失败按暗处理"一致
    dark_bg: bool,
    /// 显示配色模式；勾选状态唯一真源，托盘每次弹菜单时读取
    color_mode: settings::ColorMode,
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
            show_seconds: false,
            seven_seg: false,
            size_level: render::SIZE_LEVEL_DEFAULT,
            dark_bg: true,
            color_mode: settings::ColorMode::Auto,
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

    /// 当前是否显示秒（供托盘菜单勾选状态；真源唯一，Tray 不存副本）。
    pub fn show_seconds(&self) -> bool {
        self.show_seconds
    }

    /// 当前是否七段数码管风格（供托盘菜单勾选状态；真源唯一，Tray 不存副本）。
    pub fn seven_seg(&self) -> bool {
        self.seven_seg
    }

    /// 当前显示配色模式（供托盘菜单勾选状态；真源唯一，Tray 不存副本）。
    pub fn color_mode(&self) -> settings::ColorMode {
        self.color_mode
    }

    /// 切换时间格式（HH:MM ↔ HH:MM:SS）。
    /// 窗口可见时重建渲染器（尺寸创建时定死）并按新尺寸调整窗口，右缘夹取；
    /// 重建失败则保持原渲染器与格式。窗口不可见时只改字段，下次 show() 按新格式建。
    /// 返回是否成功生效——失败时调用方不应持久化该选择（避免重启后恢复未生效的格式）。
    pub fn set_show_seconds(&mut self, show_seconds: bool) -> bool {
        if self.show_seconds == show_seconds {
            return true;
        }
        self.show_seconds = show_seconds;
        if !self.recreate() {
            // 重建失败：还原字段，保持「字段与渲染器一致」的不变式
            self.show_seconds = !show_seconds;
            false
        } else {
            true
        }
    }

    /// 切换显示风格（默认字体 ↔ 七段数码管）：语义同 set_show_seconds。
    pub fn set_seven_seg(&mut self, seven_seg: bool) -> bool {
        if self.seven_seg == seven_seg {
            return true;
        }
        self.seven_seg = seven_seg;
        if !self.recreate() {
            self.seven_seg = !seven_seg;
            false
        } else {
            true
        }
    }

    /// 当前显示大小档位（render::FONT_LEVELS 下标，供托盘菜单勾选状态）。
    pub fn size_level(&self) -> u8 {
        self.size_level
    }

    /// 切换显示大小档位：语义同 set_show_seconds。
    pub fn set_size_level(&mut self, level: u8) -> bool {
        if self.size_level == level {
            return true;
        }
        let old = self.size_level;
        self.size_level = level;
        if !self.recreate() {
            // 重建失败：还原字段，保持「字段与渲染器一致」的不变式
            self.size_level = old;
            false
        } else {
            true
        }
    }

    /// 切换显示配色：仅重绘，不重建渲染器；切到自动时恢复采样定时器。
    /// 固定色模式停掉 `ID_BG`：不再读屏（HDR / 独占全屏下的 `GetPixel` 盲区随之绕过）。
    pub fn set_color_mode(&mut self, mode: settings::ColorMode) {
        if self.color_mode == mode {
            return;
        }
        self.color_mode = mode;
        // 窗口未建：只改字段，show() 时按新模式生效
        let Some(hwnd) = self.hwnd else { return };
        if mode == settings::ColorMode::Auto {
            unsafe { SetTimer(Some(hwnd), ID_BG, BG_TICK_MS, None); }
            // 立即定色，不等下一次 ID_BG；随后统一重绘，故忽略返回值
            self.sample_background(hwnd);
        } else {
            unsafe { let _ = KillTimer(Some(hwnd), ID_BG); }
        }
        self.render_now(hwnd);
    }

    /// 按当前 show_seconds/seven_seg/size_level 重建渲染器并适配窗口（三个 setter 共用）。
    /// 窗口不可见返回 true（下次 show() 的 ensure_created 按新字段建）；
    /// 重建失败恢复旧渲染器并返回 false（调用方回滚字段）。
    fn recreate(&mut self) -> bool {
        let Some(hwnd) = self.hwnd else { return true };
        // hwnd 与 renderer 同生共死（ensure_created / hide），此处必为 Some；先 take 以便失败时放回
        let old = self.renderer.take();
        // 旧尺寸用于中心锚点；真为 None 时退化为左上角锚点，无害
        let old_size = old.as_ref().map_or((0, 0), |r| r.size);
        // 索引不变量：size_level 的两个写入入口均保证合法 —— settings::parse 校验上界、
        // tray 命令 ID 由 TRAY_CMD_SIZE_XS + n（n ∈ 0..4）计算，故不做运行期越界兜底
        match Renderer::new(self.show_seconds, self.seven_seg, render::FONT_LEVELS[self.size_level as usize]) {
            Ok(renderer) => {
                let (w, h) = renderer.size;
                self.renderer = Some(renderer);
                // 中心锚点：尺寸变化时窗口中心保持不动（视觉上向四周等向扩张）
                let mut pos = self.pos.unwrap_or(POINT { x: 0, y: 0 });
                pos.x -= (w - old_size.0) / 2;
                pos.y -= (h - old_size.1) / 2;
                unsafe {
                    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                    // 四向夹取保证窗口完整可见（窗口最大 256px 远小于任何显示器，min ≤ max 恒成立）；
                    // GetMonitorInfoW 失败跳过夹取仅改尺寸（降级，窗口可拖回）
                    if GetMonitorInfoW(mon, &mut mi).as_bool() {
                        let m = mi.rcMonitor;
                        pos.x = pos.x.clamp(m.left, m.right - w);
                        pos.y = pos.y.clamp(m.top, m.bottom - h);
                    }
                }
                self.pos = Some(pos);
                unsafe {
                    let _ = SetWindowPos(hwnd, None, pos.x, pos.y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                self.render_now(hwnd);
                // 按新格式对齐刷新定时器：切换即刻起秒模式 1s 内、分模式对齐下一整分
                unsafe { SetTimer(Some(hwnd), ID_TIMER, self.timer_ms_to_next(), None); }
                true
            }
            Err(e) => {
                self.renderer = old;
                Log::log(&format!("renderer recreate failed, keep previous style: {e}"));
                false
            }
        }
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

        let renderer = Renderer::new(self.show_seconds, self.seven_seg, render::FONT_LEVELS[self.size_level as usize])?;
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
        // 首次显示：恢复上次会话位置；无记录或换算失败 → 默认主屏右下角
        let pos = self.pos.unwrap_or_else(|| {
            // ensure_created 刚成功，renderer 必为 Some（规范豁免的 unwrap 点）
            let size = self.renderer.as_ref().unwrap().size;
            position::load(size).unwrap_or_else(|| self.default_pos())
        });
        self.pos = Some(pos);
        Log::log(&format!("floater shown at ({}, {})", pos.x, pos.y));
        let hwnd = self.hwnd.unwrap();
        // 重建后恢复固定模式的鼠标穿透样式（ID_HOVER 定时器由下方既有分支恢复）
        if self.pinned {
            self.apply_pinned_style(hwnd, true);
        }
        self.render_now(hwnd);
        // 窗口在 ensure_created 建于 (0,0)，须等上面 present 的 UpdateLayeredWindow 移到目标位置后再首采；
        // 首帧沿用旧判定，采样翻转才补一帧（代价 ≤1 次额外绘制）。固定色模式不读屏也不起定时器，
        // 否则 bright 场景会白记一条 bg flip。
        if self.color_mode == settings::ColorMode::Auto && self.sample_background(hwnd) {
            self.render_now(hwnd);
        }
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), ID_TIMER, 1000, None); // 每秒刷新时间
            if self.color_mode == settings::ColorMode::Auto {
                SetTimer(Some(hwnd), ID_BG, BG_TICK_MS, None); // 背景采样与时间刷新解耦；固定色模式不读屏
            }
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
            // 窗口销毁前必杀采样定时器，否则重建后采样永久失效
            let _ = KillTimer(Some(hwnd), ID_BG);
            let _ = DestroyWindow(hwnd);
        }
        // 隐藏即销毁：窗口与渲染器随隐藏释放，内存回落至静默态；再次显示经 ensure_created 重建
        self.hwnd = None;
        self.renderer = None; // Renderer::drop：SelectObject 还原 → DeleteObject → DeleteDC（render.rs 既有实现）
        Log::log("floater hidden");
    }

    /// 全局热键解除固定（鼠标穿透开启后窗口收不到鼠标消息，只能靠热键）。
    pub fn unpin(&mut self) {
        if self.pinned {
            self.set_pinned(false);
            Log::log("hotkey: unpinned");
        }
    }

    /// 应用/移除鼠标穿透扩展样式（窗口重建后须恢复，见 show()）。
    fn apply_pinned_style(&self, hwnd: HWND, pinned: bool) {
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
        self.apply_pinned_style(hwnd, pinned);
        unsafe {
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

    /// 折算给渲染层的背景判定：固定色模式忽略采样结果（自动=采样、常驻白=`true`、常驻黑=`false`）。
    fn effective_dark_bg(&self) -> bool {
        match self.color_mode {
            settings::ColorMode::Auto => self.dark_bg,
            settings::ColorMode::White => true,
            settings::ColorMode::Black => false,
        }
    }

    fn render_now(&self, hwnd: HWND) {
        let Some(renderer) = &self.renderer else { return };
        let t = if self.show_seconds { now_hms() } else { now_hhmm() };
        renderer.render(&t, self.pinned, self.hovering, self.effective_dark_bg());
        let pos = self.pos.unwrap_or(POINT { x: 0, y: 0 });
        renderer.present(hwnd, pos);
    }

    /// ID_TIMER 距下一次触发的毫秒数：对齐到下一次时间显示变化
    /// （秒模式 → 下一整秒；分模式 → 下一整分）。TIMER_MIN_MS 下限防 0 值忙等。
    fn timer_ms_to_next(&self) -> u32 {
        let st = unsafe { GetLocalTime() };
        let ms = if self.show_seconds {
            1000 - st.wMilliseconds as u32
        } else {
            (60 - st.wSecond as u32) * 1000 - st.wMilliseconds as u32
        };
        ms.max(TIMER_MIN_MS)
    }

    /// 采样 9 点亮度并取低中位数（无有效点返回 None）。
    /// 几何与 scripts/measure_bg_lum.ps1 严格一致：上沿外 4px 的 5 点（贴顶改下沿外）+ 左右两侧外 4px 各 2 点。
    /// 单条采样线易被黑边/字幕带偏，扩面提高鲁棒性；两侧点落在画面中部，与黑边场景天然分离。
    fn sample_luma(&self, hwnd: HWND) -> Option<i64> {
        unsafe {
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return None;
            }
            let hdc = GetDC(None);
            if hdc.is_invalid() {
                return None;
            }
            let edge_y = if wr.top - BG_SAMPLE_GAP >= 0 { wr.top - BG_SAMPLE_GAP } else { wr.bottom + BG_SAMPLE_GAP };
            let cx = (wr.left + wr.right) / 2;
            let cy = (wr.top + wr.bottom) / 2;
            let mut pts = [(0i32, 0i32); 9];
            for (i, dx) in BG_SAMPLE_DX.iter().enumerate() {
                pts[i] = (cx + dx, edge_y);
            }
            pts[5] = (wr.left - BG_SAMPLE_GAP, cy - BG_SAMPLE_DY);
            pts[6] = (wr.left - BG_SAMPLE_GAP, cy + BG_SAMPLE_DY);
            pts[7] = (wr.right + BG_SAMPLE_GAP, cy - BG_SAMPLE_DY);
            pts[8] = (wr.right + BG_SAMPLE_GAP, cy + BG_SAMPLE_DY);
            // 有效点用定长数组 + 计数，不分配 Vec；失败点（CLR_INVALID）丢弃
            let mut values = [0i64; 9];
            let mut n = 0usize;
            for (x, y) in pts {
                let c = GetPixel(hdc, x, y);
                if c.0 != 0xFFFF_FFFF {
                    // COLORREF 为 0x00BBGGRR，失败返回 CLR_INVALID(0xFFFFFFFF)
                    let r = (c.0 & 0xff) as i64;
                    let g = ((c.0 >> 8) & 0xff) as i64;
                    let b = ((c.0 >> 16) & 0xff) as i64;
                    values[n] = (r * 299 + g * 587 + b * 114) / 1000;
                    n += 1;
                }
            }
            let _ = ReleaseDC(None, hdc);
            median_luma(&mut values, n)
        }
    }

    /// 采样一次：更新 dark_bg，判定翻转返回 true（调用方据此重绘）。
    /// 迟滞带内维持现状；采样全失败按暗处理——与既有兜底一致。
    fn sample_background(&mut self, hwnd: HWND) -> bool {
        let lum = self.sample_luma(hwnd);
        let dark = match lum {
            Some(v) => decide_dark(v, self.dark_bg),
            None => true,
        };
        if dark == self.dark_bg {
            return false; // 带内维持 / 全失败且已是暗：不重绘、不记日志（避免 4 Hz 刷屏）
        }
        self.dark_bg = dark;
        match lum {
            Some(v) => Log::log(&format!("bg flip: lum={v} dark={dark}")),
            None => Log::log("bg flip: sample failed dark=true"),
        }
        true
    }

    /// ID_BG 定时器：仅判定翻转时重绘（时间刷新由 ID_TIMER 负责）。
    fn on_bg_tick(&mut self) {
        if !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        if self.sample_background(hwnd) {
            self.render_now(hwnd);
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
        // 先取齐三个按钮的命中结果：命中后的处理要 &mut self，之后不能再借 renderer
        let hit_pin = pt_in_rect(&renderer.btn_pin, p);
        let hit_block = pt_in_rect(&renderer.btn_block, p);
        let hit_close = pt_in_rect(&renderer.btn_close, p);
        if hit_pin {
            self.set_pinned(true);
            return;
        }
        if hit_block {
            self.block_foreground();
            return;
        }
        if hit_close {
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

    /// 拉黑当前前台应用：取前台进程完整路径 → 追加进黑名单 → 立即隐藏（与点击 X 同一路径）。
    /// 点击本窗口不夺前台（WS_EX_NOACTIVATE），此刻 GetForegroundWindow 仍指向底下的全屏应用。
    /// 取不到进程名或写入失败时不写文件、不隐藏——悬浮窗仍在即是「未生效」的信号。
    fn block_foreground(&mut self) {
        let Some(exe) = detect::foreground_exe() else {
            Log::log("blacklist add failed");
            return;
        };
        if blacklist::add_entry(&exe) {
            Log::log(&format!("blacklist added: {exe}"));
            self.hide();
        } else {
            Log::log("blacklist add failed");
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
            // 换位后背景可能已变：立即采一次，不等下一次 ID_BG（固定色模式不读屏）
            if self.color_mode == settings::ColorMode::Auto && self.sample_background(hwnd) {
                self.render_now(hwnd);
            }
            // 拖动结束：持久化当前大致位置（跨重启恢复，见 position.rs）
            position::save(hwnd);
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

fn now_hms() -> String {
    let st = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond)
}

/// 有效点亮度 → 低中位数：升序 sorted[(n-1)/2]（偏暗倾向，与"采样失败按暗处理"一致）。
/// n 为有效点数，values[..n] 有效
fn median_luma(values: &mut [i64], n: usize) -> Option<i64> {
    if n == 0 {
        return None;
    }
    values[..n].sort();
    Some(values[(n - 1) / 2])
}

/// 迟滞判定：< DARK_LUMA 暗；> LIGHT_LUMA 亮；带内维持 prev
fn decide_dark(lum: i64, prev: bool) -> bool {
    if lum < DARK_LUMA { true } else if lum > LIGHT_LUMA { false } else { prev }
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
                } else if wparam.0 == ID_BG {
                    floater.on_bg_tick();
                } else if let Some(hwnd) = floater.hwnd {
                    floater.render_now(hwnd);
                    // 按距下一次显示变化的毫秒数重排定时器（wMilliseconds/wSecond 实时读取），
                    // 秒跳变与真实秒对齐；分模式顺势降至 1 次/分
                    SetTimer(Some(hwnd), ID_TIMER, floater.timer_ms_to_next(), None);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_luma_takes_lower_median() {
        // 实测字幕锚点：5 暗点 + 4 白字点，低中位数 30（均值 130 会误判亮）
        let mut v = [30, 30, 30, 30, 30, 255, 255, 255, 255];
        assert_eq!(median_luma(&mut v, 9), Some(30));
    }

    #[test]
    fn median_luma_even_count_takes_lower_middle() {
        let mut v = [232, 20];
        assert_eq!(median_luma(&mut v, 2), Some(20));
        let mut v = [40, 10, 30, 20];
        assert_eq!(median_luma(&mut v, 4), Some(20));
    }

    #[test]
    fn median_luma_edge_counts() {
        let mut v = [0i64; 9];
        assert_eq!(median_luma(&mut v, 0), None); // 全失败
        let mut v = [7];
        assert_eq!(median_luma(&mut v, 1), Some(7));
    }

    #[test]
    fn decide_dark_hysteresis_boundaries() {
        // 95 < DARK_LUMA(96) 恒暗；161 > LIGHT_LUMA(160) 恒亮——与 prev 无关
        assert!(decide_dark(95, false));
        assert!(decide_dark(95, true));
        assert!(!decide_dark(161, false));
        assert!(!decide_dark(161, true));
        // 96 / 160 落在带内：维持 prev
        assert!(decide_dark(96, true));
        assert!(!decide_dark(96, false));
        assert!(decide_dark(160, true));
        assert!(!decide_dark(160, false));
    }
}
