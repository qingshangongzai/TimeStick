//! D2D/DWrite 渲染管线：将时间文字与按钮绘制到 32bpp ARGB 位图（每像素 Alpha），
//! 再经 UpdateLayeredWindow 整体提交 —— 实现“半透明圆角背景 + 不透明文字”。
//!
//! 布局为单行：[时间] [图钉] [关闭]。渲染目标固定 DPI=96，
//! 布局坐标即物理像素（与鼠标热区一致）。

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
    /// 图钉按钮热区（客户区坐标）
    pub btn_pin: RECT,
    /// 关闭按钮热区（客户区坐标）
    pub btn_close: RECT,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let factory = unsafe { D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };

        // 时间格式固定为 HH:MM，文本尺寸恒定，窗口大小一次算好
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
        let probe: Vec<u16> = "00:00".encode_utf16().collect();
        let layout = unsafe { dwrite.CreateTextLayout(&probe, &text_format, 10000.0, 10000.0)? };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)?; }
        let text_w = metrics.width.ceil();
        let text_h = metrics.height.ceil();

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
            btn_pin,
            btn_close,
        })
    }

    /// 绘制当前帧。
    /// - 半透明圆角背景（深色背景黑底/浅色背景白底，固定模式悬停时背景加深作为提示）
    /// - 时间文字（深色背景白字/浅色背景黑字）
    /// - 非固定：空心图钉 + 关闭 X；固定且悬停：实心图钉（“即将解除”反馈）
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
            // dark_bg=false（浅色背景）时反色为半透明白底
            let bg = match (dark_bg, pinned && hovering) {
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
            // 时间文字（垂直居中，浅色背景用黑色、深色背景用白色）
            let text_brush = if dark_bg { &self.brush_light } else { &self.brush_dark };
            let tc: Vec<u16> = time.encode_utf16().collect();
            let text_y = ((h as f32 - self.text_h) / 2.0).max(0.0);
            let layout = D2D_RECT_F { left: PAD_X, top: text_y, right: 100000.0, bottom: 100000.0 };
            self.rt.DrawText(
                &tc,
                &self.text_format,
                &layout,
                text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            // 按钮图形
            if pinned {
                if hovering {
                    self.draw_pin_filled(dark_bg);
                }
            } else {
                self.draw_pin_hollow(dark_bg);
                self.draw_close(dark_bg);
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

    fn btn_center(&self, r: &RECT) -> (f32, f32) {
        (((r.left + r.right) as f32) / 2.0, ((r.top + r.bottom) as f32) / 2.0)
    }

    /// 调试：把 DIB 内容写成 BMP（32bpp 自顶向下 BGRA）。
    fn dump_bmp(&self, path: &str, w: i32, h: i32) {
        use std::io::Write;
        let row = w as usize * 4;
        let mut data = Vec::with_capacity(row * h as usize);
        unsafe {
            for y in 0..h as usize {
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
