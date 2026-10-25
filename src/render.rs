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

//! D2D/DWrite 渲染管线：将时间文字与按钮绘制到 32bpp ARGB 位图（每像素 Alpha），
//! 再经 UpdateLayeredWindow 整体提交 —— 实现“半透明圆角背景 + 不透明文字”。
//!
//! 布局为单行：[时间] [图钉] [拉黑] [关闭]；鼠标未悬停时只画时间（水平居中），
//! 悬停时展开背景与按钮。渲染目标固定 DPI=96，布局坐标即物理像素（与鼠标热区一致）。

use std::mem::size_of;
use std::ptr::null_mut;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, ID2D1Brush, ID2D1DCRenderTarget,
    ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_METRICS, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    RGBQUAD,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};
use windows_numerics::Vector2;

// 布局常量（物理像素，渲染目标固定 96 DPI）
const PAD_X: f32 = 6.0;
const PAD_V: f32 = 4.0;
const BTN_SIZE: f32 = 14.0;
const BTN_GAP: f32 = 3.0;
const CORNER_RADIUS: f32 = 8.0;
// 背景透明度：普通 25%，固定模式悬停提示 40%
const BG_ALPHA: f32 = 0.25;
const BG_ALPHA_HOVER: f32 = 0.40;

/// 显示大小五档字号（物理像素）：极小 / 小 / 中 / 大 / 特大；小档 18.0 为原始设计值兼布局基准
pub const FONT_LEVELS: [f32; 5] = [14.0, 18.0, 22.0, 28.0, 36.0];
/// 默认档位索引（小 = 18.0，与原始设计值一致：老用户升级后视觉零变化）
pub const SIZE_LEVEL_DEFAULT: u8 = 1;
/// 布局缩放基准字号：其余档位按与它的比值缩放全部布局常量
const FONT_BASE: f32 = FONT_LEVELS[SIZE_LEVEL_DEFAULT as usize];

// 七段数码管几何（物理像素）：段厚、数字单元、冒号单元、单元间距。
// 数值取向：与 18px 默认字体的视觉占比相当（整体面积小、字重细）
const SEG_T: f32 = 2.0;
const DIGIT_W: f32 = 10.0;
const DIGIT_H: f32 = 18.0;
const COLON_W: f32 = 5.0;
const DIGIT_GAP: f32 = 1.5;
const COLON_DOT_R: f32 = 1.8;
const COLON_DOT_GAP: f32 = 5.0; // 两圆点之间的边缘间距（绘制处以 2 分置于中线两侧）
// 段厚随字号缩放：基准档及以下（s ≤ 1）保持等比，更大档改用 s 的 SEG_T_DAMP 次幂（开平方）
// 放缓增长 —— 补偿大字号下"等比笔画显得笨重"的视觉效应。
// 次幂曲线在 s < 1 时反而高于等比，会让极小档变粗（1.5 → 1.76），故基准档以下不套用阻尼。
const SEG_T_DAMP: f32 = 0.5;
const SEG_T_MIN: f32 = 1.5; // 再细则边缘覆盖不足、小档发虚
const SEG_T_MAX: f32 = 4.0; // 新增更大档位时须复核此上限

/// 七段数码管几何（按显示档位缩放后）：段厚、数字单元、冒号单元、单元间距。
/// 绘制路径与文本宽度计算共用同一份，否则两者会错位。
struct SegGeom {
    t: f32,
    digit_w: f32,
    digit_h: f32,
    colon_w: f32,
    gap: f32,
    dot_r: f32,
    dot_gap: f32,
}

impl SegGeom {
    /// 按布局缩放系数 s 计算。段厚走阻尼曲线（不取整，交由抗锯齿），
    /// 数字/冒号单元取整，间距保持浮点（取整无收益且累积误差）。
    fn scaled(s: f32) -> Self {
        // 段厚不再量化到 0.5px 阶梯：阶梯跨档增量不均（+0.5 / 0 / +1.0 / +0.5），
        // 相对字重随之忽高忽低（0.107 / 0.111 / 0.091 / 0.103 / 0.097），过渡不和谐。
        // 阻尼曲线下段厚恒为连续值，抗锯齿足以表现，视觉过渡平滑。
        let t = if s <= 1.0 { SEG_T * s } else { SEG_T * s.powf(SEG_T_DAMP) };
        let t = t.clamp(SEG_T_MIN, SEG_T_MAX);
        SegGeom {
            t,
            digit_w: (DIGIT_W * s).round(),
            digit_h: (DIGIT_H * s).round(),
            colon_w: (COLON_W * s).round(),
            gap: DIGIT_GAP * s,
            dot_r: COLON_DOT_R * s,
            dot_gap: COLON_DOT_GAP * s,
        }
    }
}

/// 每像素 Alpha 渲染器：D2D 绘制 → DIB 内存 DC → UpdateLayeredWindow。
pub struct Renderer {
    rt: ID2D1DCRenderTarget,
    /// 浅色背景用的画刷（白字/白底按钮）
    brush_light: ID2D1Brush,
    /// 深色背景用的画刷（黑字/黑底按钮）
    brush_dark: ID2D1Brush,
    brush_bg: ID2D1Brush,
    brush_bg_hover: ID2D1Brush,
    brush_bg_light: ID2D1Brush,
    brush_bg_light_hover: ID2D1Brush,
    text_format: IDWriteTextFormat,
    hdc: HDC,
    dib: HBITMAP,
    old_obj: HGDIOBJ,
    /// DIB 像素缓冲（调试转储用）
    bits: *mut u8,
    /// 调试：TIMESTICK_DUMP 指向的 BMP 输出路径
    dump_path: Option<String>,
    /// 窗口客户区尺寸（物理像素）
    pub size: (i32, i32),
    /// 单行文本高度（垂直居中用）
    text_h: f32,
    /// 单行文本宽度（未悬停水平居中用）
    text_w: f32,
    /// 左侧内边距（悬停时时间回到该处）
    pad_x: f32,
    /// 背景圆角（不超过窗口半高）
    corner: f32,
    /// 按钮矢量几何缩放系数（= btn / BTN_SIZE）
    btn_scale: f32,
    /// 七段数码管几何（按档位缩放后）
    seg: SegGeom,
    /// 七段数码管风格（true 时时间以七段管绘制，尺寸按几何计算）
    seven_seg: bool,
    /// 图钉按钮热区（客户区坐标）
    pub btn_pin: RECT,
    /// 拉黑按钮热区（客户区坐标）
    pub btn_block: RECT,
    /// 关闭按钮热区（客户区坐标）
    pub btn_close: RECT,
}

impl Renderer {
    pub fn new(show_seconds: bool, seven_seg: bool, font_size: f32) -> Result<Self> {
        // 布局统一按与基准档（小档 18.0，FONT_BASE）字号的比值缩放：内边距、间距、圆角、按钮与七段管几何
        let s = font_size / FONT_BASE;
        let seg = SegGeom::scaled(s);
        let factory = unsafe { D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };

        // 时间格式按显示需求取 probe（HH:MM 或 HH:MM:SS），文本尺寸一次算定，窗口大小随之确定
        let text_format = unsafe {
            dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font_size,
                w!(""),
            )?
        };
        // 七段管尺寸按几何常量计算（跳过 DWrite 度量）；默认字体走 DWrite 探针
        let probe = if show_seconds { "00:00:00" } else { "00:00" };
        let (text_w, text_h) = if seven_seg {
            (seven_seg_text_width(&seg, probe), seg.digit_h)
        } else {
            let probe_u16: Vec<u16> = probe.encode_utf16().collect();
            let layout = unsafe { dwrite.CreateTextLayout(&probe_u16, &text_format, 10000.0, 10000.0)? };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            unsafe { layout.GetMetrics(&mut metrics)?; }
            (metrics.width.ceil(), metrics.height.ceil())
        };

        // 单行布局：[时间] [图钉] [拉黑] [关闭]
        let pad_x = PAD_X * s;
        let pad_v = PAD_V * s;
        let btn_gap = BTN_GAP * s;
        // 按钮只放大不缩小：等比缩到 0.78 倍时仅 10.9px，低于可点击目标下限
        let btn = (BTN_SIZE * s).max(BTN_SIZE);
        let w = (pad_x + text_w + btn_gap + btn + btn_gap + btn + btn_gap + btn + pad_x).ceil() as i32;
        let h = (text_h.max(btn) + pad_v * 2.0).ceil() as i32;
        // 圆角不超过窗口半高，避免小档时圆角吃掉整个高度
        let corner = (CORNER_RADIUS * s).min(h as f32 / 2.0);
        let btn_y = ((h as f32 - btn) / 2.0).ceil() as i32;
        let x_pin = (pad_x + text_w + btn_gap).ceil() as i32;
        let x_block = x_pin + btn as i32 + btn_gap as i32;
        let x_close = x_block + btn as i32 + btn_gap as i32;
        let btn_pin = RECT { left: x_pin, top: btn_y, right: x_pin + btn as i32, bottom: btn_y + btn as i32 };
        let btn_block = RECT { left: x_block, top: btn_y, right: x_block + btn as i32, bottom: btn_y + btn as i32 };
        let btn_close = RECT { left: x_close, top: btn_y, right: x_close + btn as i32, bottom: btn_y + btn as i32 };

        // 32bpp 自顶向下 DIB：UpdateLayeredWindow 据此获取每像素 Alpha
        let mut bits: *mut std::ffi::c_void = null_mut();
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 负高度 = 自顶向下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let dib = unsafe { CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };
        let hdc = unsafe { CreateCompatibleDC(None) };
        let old_obj = unsafe { SelectObject(hdc, HGDIOBJ(dib.0)) };

        // DC 渲染目标：预乘 Alpha 与 UpdateLayeredWindow 的 AC_SRC_ALPHA 相匹配。
        // DPI 固定 96：布局坐标即物理像素，与鼠标热区一致（不随系统缩放换算）
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = unsafe { factory.CreateDCRenderTarget(&props)? };
        let white = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)? };
        let brush_light: ID2D1Brush = white.cast()?;
        let black = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)? };
        let brush_dark: ID2D1Brush = black.cast()?;
        let bg = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: BG_ALPHA }, None)? };
        let brush_bg: ID2D1Brush = bg.cast()?;
        let bg_hover = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: BG_ALPHA_HOVER }, None)? };
        let brush_bg_hover: ID2D1Brush = bg_hover.cast()?;
        let bg_light = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: BG_ALPHA }, None)? };
        let brush_bg_light: ID2D1Brush = bg_light.cast()?;
        let bg_light_hover = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: BG_ALPHA_HOVER }, None)? };
        let brush_bg_light_hover: ID2D1Brush = bg_light_hover.cast()?;

        Ok(Renderer {
            rt,
            brush_light,
            brush_dark,
            brush_bg,
            brush_bg_hover,
            brush_bg_light,
            brush_bg_light_hover,
            text_format,
            hdc,
            dib,
            old_obj,
            bits: bits as *mut u8,
            dump_path: std::env::var("TIMESTICK_DUMP").ok().filter(|p| !p.is_empty()),
            size: (w, h),
            text_h,
            text_w,
            pad_x,
            corner,
            btn_scale: btn / BTN_SIZE,
            seg,
            seven_seg,
            btn_pin,
            btn_block,
            btn_close,
        })
    }

    /// 绘制当前帧。
    /// - 未悬停（hovering=false）：只画时间文字（水平居中），无背景无按钮 —— 纯文字悬浮
    /// - 悬停（hovering=true）：半透明圆角背景 + 时间（回到左侧）+ 按钮
    /// - 背景配色随背景亮度反色（深色背景黑底/浅色背景白底），固定模式悬停时加深作为提示
    /// - 时间文字：深色背景白字/浅色背景黑字；按钮：非固定显示空心图钉 + 拉黑 + 关闭 X，固定显示实心图钉（“即将解除”反馈）
    pub fn render(&self, time: &str, pinned: bool, hovering: bool, dark_bg: bool) {
        unsafe {
            let (w, h) = self.size;
            let rect = RECT { left: 0, top: 0, right: w, bottom: h };
            if let Err(e) = self.rt.BindDC(self.hdc, &rect) {
                crate::log::Log::log(&format!("BindDC failed: {e}"));
                return;
            }
            self.rt.BeginDraw();
            // 全透明清底
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            // 圆角背景（每像素 Alpha：圆角外区域保持透明）。
            // dark_bg=false（浅色背景）时反色为半透明白底；固定模式悬停时加深作为提示
            if hovering {
                let bg = match (dark_bg, pinned) {
                    (true, true) => &self.brush_bg_hover,
                    (true, false) => &self.brush_bg,
                    (false, true) => &self.brush_bg_light_hover,
                    (false, false) => &self.brush_bg_light,
                };
                let rounded = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 },
                    radiusX: self.corner,
                    radiusY: self.corner,
                };
                self.rt.FillRoundedRectangle(&rounded, bg);
            }
            // 时间文字（垂直居中；未悬停水平居中，悬停回到左侧给按钮让位）
            // 七段管风格：段色同样随背景反色（深底白段/浅底黑段）
            let text_brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
            let text_y = ((h as f32 - self.text_h) / 2.0).max(0.0);
            let text_x = if hovering { self.pad_x } else { ((w as f32 - self.text_w) / 2.0).max(self.pad_x) };
            if self.seven_seg {
                self.draw_seven_seg(time, text_x, text_y, text_brush);
            } else {
                let tc: Vec<u16> = time.encode_utf16().collect();
                let layout = D2D_RECT_F { left: text_x, top: text_y, right: 100000.0, bottom: 100000.0 };
                self.rt.DrawText(
                    &tc,
                    &self.text_format,
                    &layout,
                    text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            // 按钮图形（仅悬停时显示）
            if hovering {
                if pinned {
                    self.draw_pin_filled(dark_bg);
                } else {
                    self.draw_pin_hollow(dark_bg);
                    self.draw_block(dark_bg);
                    self.draw_close(dark_bg);
                }
            }
            if let Err(e) = self.rt.EndDraw(None, None) {
                crate::log::Log::log(&format!("EndDraw failed: {e}"));
            }
        }
    }

    /// 将当前 DIB 内容经 UpdateLayeredWindow 提交到屏幕（同时设定窗口位置）。
    pub fn present(&self, hwnd: windows::Win32::Foundation::HWND, pos: POINT) {
        let (w, h) = self.size;
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            if let Err(e) = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pos as *const POINT),
                Some(&size as *const SIZE),
                Some(self.hdc),
                Some(&src as *const POINT),
                COLORREF(0),
                Some(&blend as *const BLENDFUNCTION),
                ULW_ALPHA,
            ) {
                crate::log::Log::log(&format!("UpdateLayeredWindow failed: {e}"));
            } else if let Some(path) = &self.dump_path {
                self.dump_bmp(path, w, h);
            }
        }
    }

    /// 空心图钉（未固定状态）：圆环 + 中心点 + 针尖。
    /// 矢量几何坐标随按钮一起缩放（k = btn / BTN_SIZE）。
    fn draw_pin_hollow(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_pin);
        let k = self.btn_scale;
        unsafe {
            self.rt.DrawEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3.5 * k, radiusY: 3.5 * k },
                brush,
                1.2 * k,
                None,
            );
            self.rt.FillEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 1.3 * k, radiusY: 1.3 * k },
                brush,
            );
            self.rt.DrawLine(
                Vector2 { X: cx, Y: cy + 3.5 * k },
                Vector2 { X: cx, Y: cy + 6.0 * k },
                brush,
                1.2 * k,
                None,
            );
        }
    }

    /// 实心图钉（已固定状态，悬停提示）：填充圆 + 针尖
    fn draw_pin_filled(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_pin);
        let k = self.btn_scale;
        unsafe {
            self.rt.FillEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3.5 * k, radiusY: 3.5 * k },
                brush,
            );
            self.rt.DrawLine(
                Vector2 { X: cx, Y: cy + 3.5 * k },
                Vector2 { X: cx, Y: cy + 6.0 * k },
                brush,
                1.2 * k,
                None,
            );
        }
    }

    /// 拉黑（禁止符）图形：圆环 + 斜线 —— 点击把当前全屏应用加入黑名单
    fn draw_block(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_block);
        let k = self.btn_scale;
        // 斜线端点到圆心的分量取环半径 3.5k 的 1/√2，恰好落在环上
        let d = 2.5 * k;
        unsafe {
            self.rt.DrawEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3.5 * k, radiusY: 3.5 * k },
                brush,
                1.2 * k,
                None,
            );
            self.rt.DrawLine(
                Vector2 { X: cx - d, Y: cy + d },
                Vector2 { X: cx + d, Y: cy - d },
                brush,
                1.2 * k,
                None,
            );
        }
    }

    /// 关闭 X 图形：两条交叉线
    fn draw_close(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_close);
        let k = self.btn_scale;
        unsafe {
            self.rt.DrawLine(
                Vector2 { X: cx - 3.5 * k, Y: cy - 3.5 * k },
                Vector2 { X: cx + 3.5 * k, Y: cy + 3.5 * k },
                brush,
                1.2 * k,
                None,
            );
            self.rt.DrawLine(
                Vector2 { X: cx - 3.5 * k, Y: cy + 3.5 * k },
                Vector2 { X: cx + 3.5 * k, Y: cy - 3.5 * k },
                brush,
                1.2 * k,
                None,
            );
        }
    }

    /// 七段管风格：画整串时间（数字 + 冒号），x/y 为文字左上角（与 DrawText 同基准）。
    fn draw_seven_seg(&self, time: &str, x: f32, y: f32, brush: &ID2D1Brush) {
        let mut cx = x;
        for c in time.chars() {
            if c == ':' {
                self.draw_seven_seg_colon(cx, y, brush);
                cx += self.seg.colon_w;
            } else {
                self.draw_seven_seg_digit(c as u8, cx, y, brush);
                cx += self.seg.digit_w;
            }
            // 与 seven_seg_text_width 的推进规则一致（尾随间距仅多算 1 个 gap，无影响）
            cx += self.seg.gap;
        }
    }

    /// 画单个数字单元（仅点亮段，其余全透明）。
    /// 段命名：a 顶部横 / b 右上 / c 右下 / d 底部横 / e 左下 / f 左上 / g 中间横。
    fn draw_seven_seg_digit(&self, digit: u8, x: f32, y: f32, brush: &ID2D1Brush) {
        let mask = seg_map(digit);
        let g = &self.seg;
        let vlen = (g.digit_h - g.t) / 2.0; // 垂直段半高
        let mid_y = vlen; // 中横段顶边（中横段垂直居中 ⇒ 恰为半高；调整段位布局时须独立计算）
        let low_y = (g.digit_h + g.t) / 2.0; // 下垂直段顶边
        let r = g.t / 2.0; // 段圆角半径
        let seg = |l: f32, t: f32, rw: f32, rh: f32| D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: x + l, top: y + t, right: x + l + rw, bottom: y + t + rh },
            radiusX: r,
            radiusY: r,
        };
        unsafe {
            if mask & SEG_A != 0 { self.rt.FillRoundedRectangle(&seg(0.0, 0.0, g.digit_w, g.t), brush); }
            if mask & SEG_B != 0 { self.rt.FillRoundedRectangle(&seg(g.digit_w - g.t, 0.0, g.t, vlen), brush); }
            if mask & SEG_C != 0 { self.rt.FillRoundedRectangle(&seg(g.digit_w - g.t, low_y, g.t, vlen), brush); }
            if mask & SEG_D != 0 { self.rt.FillRoundedRectangle(&seg(0.0, g.digit_h - g.t, g.digit_w, g.t), brush); }
            if mask & SEG_E != 0 { self.rt.FillRoundedRectangle(&seg(0.0, low_y, g.t, vlen), brush); }
            if mask & SEG_F != 0 { self.rt.FillRoundedRectangle(&seg(0.0, 0.0, g.t, vlen), brush); }
            if mask & SEG_G != 0 { self.rt.FillRoundedRectangle(&seg(0.0, mid_y, g.digit_w, g.t), brush); }
        }
    }

    /// 画冒号：两个圆点，垂直居中分布。
    fn draw_seven_seg_colon(&self, x: f32, y: f32, brush: &ID2D1Brush) {
        let g = &self.seg;
        let cx = x + g.colon_w / 2.0;
        let cy = y + g.digit_h / 2.0;
        unsafe {
            let top = D2D1_ELLIPSE {
                point: Vector2 { X: cx, Y: cy - g.dot_r - g.dot_gap / 2.0 },
                radiusX: g.dot_r,
                radiusY: g.dot_r,
            };
            let bottom = D2D1_ELLIPSE {
                point: Vector2 { X: cx, Y: cy + g.dot_r + g.dot_gap / 2.0 },
                radiusX: g.dot_r,
                radiusY: g.dot_r,
            };
            self.rt.FillEllipse(&top, brush);
            self.rt.FillEllipse(&bottom, brush);
        }
    }

    fn btn_center(&self, r: &RECT) -> (f32, f32) {
        (((r.left + r.right) as f32) / 2.0, ((r.top + r.bottom) as f32) / 2.0)
    }

    /// 调试：把 DIB 内容写成 BMP（32bpp 自顶向下 BGRA）。
    fn dump_bmp(&self, path: &str, w: i32, h: i32) {
        use std::io::Write;
        let row = w as usize * 4;
        let mut data = Vec::with_capacity(row * h as usize);
        unsafe {
            // BMP 文件头 biHeight 为正值，标准语义自底向上；DIB 为自顶向下，须逆序拷贝行
            for y in (0..h as usize).rev() {
                data.extend_from_slice(std::slice::from_raw_parts(self.bits.add(y * row), row));
            }
        }
        let file_size = 54 + data.len();
        let mut f = match std::fs::File::create(path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let _ = f.write_all(b"BM");
        let _ = f.write_all(&(file_size as u32).to_le_bytes());
        let _ = f.write_all(&0u32.to_le_bytes());
        let _ = f.write_all(&54u32.to_le_bytes());
        let _ = f.write_all(&40u32.to_le_bytes());
        let _ = f.write_all(&(w as u32).to_le_bytes());
        let _ = f.write_all(&(h as u32).to_le_bytes());
        let _ = f.write_all(&1u16.to_le_bytes());
        let _ = f.write_all(&32u16.to_le_bytes());
        let _ = f.write_all(&0u32.to_le_bytes());
        let _ = f.write_all(&(data.len() as u32).to_le_bytes());
        let _ = f.write_all(&0i32.to_le_bytes());
        let _ = f.write_all(&0i32.to_le_bytes());
        let _ = f.write_all(&0u32.to_le_bytes());
        let _ = f.write_all(&0u32.to_le_bytes());
        let _ = f.write_all(&data);
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.hdc, self.old_obj);
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
            let _ = DeleteDC(self.hdc);
        }
    }
}

// 段码位掩码：bit0=a 顶部横 … bit6=g 中间横（段命名见 draw_seven_seg_digit）
const SEG_A: u8 = 1 << 0;
const SEG_B: u8 = 1 << 1;
const SEG_C: u8 = 1 << 2;
const SEG_D: u8 = 1 << 3;
const SEG_E: u8 = 1 << 4;
const SEG_F: u8 = 1 << 5;
const SEG_G: u8 = 1 << 6;

/// 数字 0-9 的点亮段位掩码；其他字符返回全灭（防御，不 panic）。
fn seg_map(digit: u8) -> u8 {
    match digit {
        b'0' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_E | SEG_F,
        b'1' => SEG_B | SEG_C,
        b'2' => SEG_A | SEG_B | SEG_G | SEG_E | SEG_D,
        b'3' => SEG_A | SEG_B | SEG_G | SEG_C | SEG_D,
        b'4' => SEG_F | SEG_G | SEG_B | SEG_C,
        b'5' => SEG_A | SEG_F | SEG_G | SEG_C | SEG_D,
        b'6' => SEG_A | SEG_F | SEG_G | SEG_E | SEG_C | SEG_D,
        b'7' => SEG_A | SEG_B | SEG_C,
        b'8' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_E | SEG_F | SEG_G,
        b'9' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_F | SEG_G,
        _ => 0,
    }
}

/// 七段管文本宽度：数字单元 × 数字数 + 冒号单元 × 冒号数 + 间距 × 字符数。
/// 推进规则与 draw_seven_seg 一致（几何须取自同一份 SegGeom，否则文本宽度与绘制错位）。
fn seven_seg_text_width(g: &SegGeom, text: &str) -> f32 {
    let mut w = 0.0;
    for c in text.chars() {
        w += if c == ':' { g.colon_w } else { g.digit_w };
        w += g.gap;
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seg_map_covers_digits() {
        assert_eq!(seg_map(b'0'), SEG_A | SEG_B | SEG_C | SEG_D | SEG_E | SEG_F);
        assert_eq!(seg_map(b'1'), SEG_B | SEG_C);
        assert_eq!(seg_map(b'5'), SEG_A | SEG_F | SEG_G | SEG_C | SEG_D);
        assert_eq!(seg_map(b'8'), SEG_A | SEG_B | SEG_C | SEG_D | SEG_E | SEG_F | SEG_G);
        assert_eq!(seg_map(b'9'), SEG_A | SEG_B | SEG_C | SEG_D | SEG_F | SEG_G);
    }

    #[test]
    fn seg_map_unknown_is_all_off() {
        assert_eq!(seg_map(b'x'), 0);
    }

    #[test]
    fn seven_seg_text_width_matches_geometry() {
        let g = SegGeom::scaled(1.0);
        // "00:00" = 4 数字 + 1 冒号 + 5 间距
        assert_eq!(seven_seg_text_width(&g, "00:00"), 4.0 * g.digit_w + g.colon_w + 5.0 * g.gap);
        // "00:00:00" = 6 数字 + 2 冒号 + 8 间距
        assert_eq!(seven_seg_text_width(&g, "00:00:00"), 6.0 * g.digit_w + 2.0 * g.colon_w + 8.0 * g.gap);
        // 单字符：单元宽 + 尾随间距
        assert_eq!(seven_seg_text_width(&g, "5"), g.digit_w + g.gap);
    }

    #[test]
    fn default_level_is_the_original_design_size() {
        // 默认档必须是原始设计值 18.0：它与 FONT_BASE 相等，老用户升级后视觉零变化
        assert_eq!(FONT_LEVELS[SIZE_LEVEL_DEFAULT as usize], 18.0);
        assert_eq!(FONT_BASE, 18.0);
    }

    #[test]
    fn seg_geom_scales_integer_and_monotonic() {
        // 小档 s=1.0（布局基准）：与原始常量逐值一致
        let base = SegGeom::scaled(1.0);
        assert_eq!((base.t, base.digit_w, base.digit_h, base.colon_w), (2.0, 10.0, 18.0, 5.0));
        assert_eq!((base.gap, base.dot_r, base.dot_gap), (1.5, 1.8, 5.0));
        // 极小档 s≈0.778：基准档以下保持等比（不套阻尼，否则小档反而变粗）
        let xs = SegGeom::scaled(14.0 / 18.0);
        assert!((xs.t - SEG_T * 14.0 / 18.0).abs() < 1e-5, "xs t = {}", xs.t);
        assert_eq!(xs.digit_h, 14.0);
        // 中档 s≈1.222：新增档取值落在小档与大档之间
        let mid = SegGeom::scaled(22.0 / 18.0);
        assert!(mid.digit_h > base.digit_h && mid.digit_h < 28.0);
        // 特大档 s=2.0：数字/冒号单元整体翻倍
        let xl = SegGeom::scaled(2.0);
        assert_eq!((xl.digit_w, xl.digit_h, xl.colon_w), (20.0, 36.0, 10.0));
        // 段厚单调递增，但相对字重（t / digit_h）单调不增 —— 阻尼曲线的两个特征
        let levels: Vec<SegGeom> = FONT_LEVELS.iter().map(|f| SegGeom::scaled(f / FONT_BASE)).collect();
        for pair in levels.windows(2) {
            assert!(pair[1].t > pair[0].t, "段厚须随档位递增");
            let w = |g: &SegGeom| g.t / g.digit_h;
            assert!(w(&pair[1]) <= w(&pair[0]) + 1e-6, "相对字重须随档位单调不增");
        }
    }

    /// 各档相对字重（段厚 / 字高）须落在合理区间内：
    /// 过粗显笨重、过细发虚。改造前极小档与大档为 0.143（下限与奇偶校正各加粗约 29%）；
    /// 具体档位间的平滑过渡由 seg_geom_scales_integer_and_monotonic 的单调性断言把关
    #[test]
    fn seg_thickness_weight_is_consistent_across_levels() {
        for f in FONT_LEVELS {
            let g = SegGeom::scaled(f / FONT_BASE);
            let weight = g.t / g.digit_h;
            assert!((0.07..=0.12).contains(&weight), "font size {f} weight {weight}");
        }
    }
}
