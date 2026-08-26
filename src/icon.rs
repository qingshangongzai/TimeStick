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

//! 内嵌图标加载：读取 build.rs 嵌入的组图标资源（ID 1，见 build.rs），
//! 供托盘（32px）与窗口类（32px/16px）复用；加载失败返回 None，由调用方回退。

use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR};

/// 组图标资源 ID（winres 的 set_icon 固定分配为 1）。
const APP_ICON_ID: u16 = 1;

/// 图标加载基准尺寸（96 DPI 逻辑像素）：窗口大图标/托盘 32；
/// 关于对话框按实际 DPI 换算物理尺寸传入（如 150% 传 48，命中 .ico 精确帧）；
/// 主窗口类/托盘图标保持基准尺寸即可（系统按需自行缩放）。
pub const LARGE: i32 = 32;
/// 图标加载基准尺寸（96 DPI 逻辑像素）：标题栏小图标 16；
/// 关于对话框按实际 DPI 换算物理尺寸传入（如 150% 传 24，命中 .ico 精确帧）；
/// 主窗口类 hIconSm 保持基准尺寸即可（系统按需自行缩放）。
pub const SMALL: i32 = 16;

/// 加载指定尺寸的内嵌图标；调用方负责 DestroyIcon。
pub fn load(size: i32) -> Option<HICON> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        // MAKEINTRESOURCEW(1)：资源 ID 直接编码进指针值
        let name = PCWSTR::from_raw(APP_ICON_ID as *const u16);
        let h = LoadImageW(Some(hinst), name, IMAGE_ICON, size, size, LR_DEFAULTCOLOR).ok()?;
        Some(HICON(h.0))
    }
}
