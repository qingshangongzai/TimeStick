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
//! 布局为单行：[时间] [图钉] [关闭]；鼠标未悬停时只画时间（水平居中），
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
const FONT_SIZE: f32 = 18.0;
const CORNER_RADIUS: f32 = 8.0;
// 背景透明度：普通 25%，固定模式悬停提示 40%
const BG_ALPHA: f32 = 0.25;
const BG_ALPHA_HOVER: f32 = 0.40;
// 七段数码管几何（物理像素）：段厚、数字单元、冒号单元、单元间距。
// 数值取向：与 18px 默认字体的视觉占比相当（整体面积小、字重细）
const SEG_T: f32 = 2.0;
const DIGIT_W: f32 = 10.0;
const DIGIT_H: f32 = 18.0;
const COLON_W: f32 = 5.0;
const DIGIT_GAP: f32 = 1.5;
const COLON_DOT_R: f32 = 1.8;
const COLON_DOT_GAP: f32 = 5.0; // 两圆点之间的边缘间距（绘制处以 2 分置于中线两侧）

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
    /// 七段数码管风格（true 时时间以七段管绘制，尺寸按几何计算）
    seven_seg: bool,
    /// 图钉按钮热区（客户区坐标）
    pub btn_pin: RECT,
    /// 关闭按钮热区（客户区坐标）
    pub btn_close: RECT,
}

impl Renderer {
    pub fn new(show_seconds: bool, seven_seg: bool) -> Result<Self> {
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
                FONT_SIZE,
                w!(""),
            )?
        };
        // 七段管尺寸按几何常量计算（跳过 DWrite 度量）；默认字体走 DWrite 探针
        let probe = if show_seconds { "00:00:00" } else { "00:00" };
        let (text_w, text_h) = if seven_seg {
            (seven_seg_text_width(probe), DIGIT_H)
        } else {
            let probe_u16: Vec<u16> = probe.encode_utf16().collect();
            let layout = unsafe { dwrite.CreateTextLayout(&probe_u16, &text_format, 10000.0, 10000.0)? };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            unsafe { layout.GetMetrics(&mut metrics)?; }
            (metrics.width.ceil(), metrics.height.ceil())
        };

        // 单行布局：[时间] [图钉] [关闭]
        let btn = BTN_SIZE;
        let w = (PAD_X + text_w + BTN_GAP + btn + BTN_GAP + btn + PAD_X).ceil() as i32;
        let h = (text_h.max(btn) + PAD_V * 2.0).ceil() as i32;
        let btn_y = ((h as f32 - btn) / 2.0).ceil() as i32;
        let x_pin = (PAD_X + text_w + BTN_GAP).ceil() as i32;
        let x_close = x_pin + BTN_SIZE as i32 + BTN_GAP as i32;
        let btn_pin = RECT { left: x_pin, top: btn_y, right: x_pin + BTN_SIZE as i32, bottom: btn_y + BTN_SIZE as i32 };
        let btn_close = RECT { left: x_close, top: btn_y, right: x_close + BTN_SIZE as i32, bottom: btn_y + BTN_SIZE as i32 };

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
            seven_seg,
            btn_pin,
            btn_close,
        })
    }

    /// 绘制当前帧。
    /// - 未悬停（hovering=false）：只画时间文字（水平居中），无背景无按钮 —— 纯文字悬浮
    /// - 悬停（hovering=true）：半透明圆角背景 + 时间（回到左侧）+ 按钮
    /// - 背景配色随背景亮度反色（深色背景黑底/浅色背景白底），固定模式悬停时加深作为提示
    /// - 时间文字：深色背景白字/浅色背景黑字；按钮：非固定显示空心图钉 + 关闭 X，固定显示实心图钉（“即将解除”反馈）
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
                    radiusX: CORNER_RADIUS,
                    radiusY: CORNER_RADIUS,
                };
                self.rt.FillRoundedRectangle(&rounded, bg);
            }
            // 时间文字（垂直居中；未悬停水平居中，悬停回到左侧给按钮让位）
            // 七段管风格：段色同样随背景反色（深底白段/浅底黑段）
            let text_brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
            let text_y = ((h as f32 - self.text_h) / 2.0).max(0.0);
            let text_x = if hovering { PAD_X } else { ((w as f32 - self.text_w) / 2.0).max(PAD_X) };
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

    /// 空心图钉（未固定状态）：圆环 + 中心点 + 针尖
    fn draw_pin_hollow(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_pin);
        unsafe {
            self.rt.DrawEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3.5, radiusY: 3.5 },
                brush,
                1.2,
                None,
            );
            self.rt.FillEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 1.3, radiusY: 1.3 },
                brush,
            );
            self.rt.DrawLine(
                Vector2 { X: cx, Y: cy + 3.5 },
                Vector2 { X: cx, Y: cy + 6.0 },
                brush,
                1.2,
                None,
            );
        }
    }

    /// 实心图钉（已固定状态，悬停提示）：填充圆 + 针尖
    fn draw_pin_filled(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_pin);
        unsafe {
            self.rt.FillEllipse(
                &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3.5, radiusY: 3.5 },
                brush,
            );
            self.rt.DrawLine(
                Vector2 { X: cx, Y: cy + 3.5 },
                Vector2 { X: cx, Y: cy + 6.0 },
                brush,
                1.2,
                None,
            );
        }
    }

    /// 关闭 X 图形：两条交叉线
    fn draw_close(&self, dark_bg: bool) {
        let brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
        let (cx, cy) = self.btn_center(&self.btn_close);
        unsafe {
            self.rt.DrawLine(
                Vector2 { X: cx - 3.5, Y: cy - 3.5 },
                Vector2 { X: cx + 3.5, Y: cy + 3.5 },
                brush,
                1.2,
                None,
            );
            self.rt.DrawLine(
                Vector2 { X: cx - 3.5, Y: cy + 3.5 },
                Vector2 { X: cx + 3.5, Y: cy - 3.5 },
                brush,
                1.2,
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
                cx += COLON_W;
            } else {
                self.draw_seven_seg_digit(c as u8, cx, y, brush);
                cx += DIGIT_W;
            }
            // 与 seven_seg_text_width 的推进规则一致（尾随间距仅多算 1.5px，无影响）
            cx += DIGIT_GAP;
        }
    }

    /// 画单个数字单元（仅点亮段，其余全透明）。
    /// 段命名：a 顶部横 / b 右上 / c 右下 / d 底部横 / e 左下 / f 左上 / g 中间横。
    fn draw_seven_seg_digit(&self, digit: u8, x: f32, y: f32, brush: &ID2D1Brush) {
        let mask = seg_map(digit);
        let vlen = (DIGIT_H - SEG_T) / 2.0; // 垂直段半高
        let mid_y = vlen; // 中横段顶边（中横段垂直居中 ⇒ 恰为半高；调整段位布局时须独立计算）
        let low_y = (DIGIT_H + SEG_T) / 2.0; // 下垂直段顶边
        let r = SEG_T / 2.0; // 段圆角半径
        let seg = |l: f32, t: f32, rw: f32, rh: f32| D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: x + l, top: y + t, right: x + l + rw, bottom: y + t + rh },
            radiusX: r,
            radiusY: r,
        };
        unsafe {
            if mask & SEG_A != 0 { self.rt.FillRoundedRectangle(&seg(0.0, 0.0, DIGIT_W, SEG_T), brush); }
            if mask & SEG_B != 0 { self.rt.FillRoundedRectangle(&seg(DIGIT_W - SEG_T, 0.0, SEG_T, vlen), brush); }
            if mask & SEG_C != 0 { self.rt.FillRoundedRectangle(&seg(DIGIT_W - SEG_T, low_y, SEG_T, vlen), brush); }
            if mask & SEG_D != 0 { self.rt.FillRoundedRectangle(&seg(0.0, DIGIT_H - SEG_T, DIGIT_W, SEG_T), brush); }
            if mask & SEG_E != 0 { self.rt.FillRoundedRectangle(&seg(0.0, low_y, SEG_T, vlen), brush); }
            if mask & SEG_F != 0 { self.rt.FillRoundedRectangle(&seg(0.0, 0.0, SEG_T, vlen), brush); }
            if mask & SEG_G != 0 { self.rt.FillRoundedRectangle(&seg(0.0, mid_y, DIGIT_W, SEG_T), brush); }
        }
    }

    /// 画冒号：两个圆点，垂直居中分布。
    fn draw_seven_seg_colon(&self, x: f32, y: f32, brush: &ID2D1Brush) {
        let cx = x + COLON_W / 2.0;
        let cy = y + DIGIT_H / 2.0;
        unsafe {
            let top = D2D1_ELLIPSE {
                point: Vector2 { X: cx, Y: cy - COLON_DOT_R - COLON_DOT_GAP / 2.0 },
                radiusX: COLON_DOT_R,
                radiusY: COLON_DOT_R,
            };
            let bottom = D2D1_ELLIPSE {
                point: Vector2 { X: cx, Y: cy + COLON_DOT_R + COLON_DOT_GAP / 2.0 },
                radiusX: COLON_DOT_R,
                radiusY: COLON_DOT_R,
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
/// 推进规则与 draw_seven_seg 一致。
fn seven_seg_text_width(text: &str) -> f32 {
    let mut w = 0.0;
    for c in text.chars() {
        w += if c == ':' { COLON_W } else { DIGIT_W };
        w += DIGIT_GAP;
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
        // "00:00" = 4 数字 + 1 冒号 + 5 间距
        assert_eq!(seven_seg_text_width("00:00"), 4.0 * DIGIT_W + COLON_W + 5.0 * DIGIT_GAP);
        // "00:00:00" = 6 数字 + 2 冒号 + 8 间距
        assert_eq!(seven_seg_text_width("00:00:00"), 6.0 * DIGIT_W + 2.0 * COLON_W + 8.0 * DIGIT_GAP);
        // 单字符：单元宽 + 尾随间距
        assert_eq!(seven_seg_text_width("5"), DIGIT_W + DIGIT_GAP);
    }
}
