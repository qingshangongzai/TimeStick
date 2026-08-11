//! 内嵌图标加载：读取 build.rs 嵌入的组图标资源（ID 1，见 build.rs），
//! 供托盘（32px）与窗口类（32px/16px）复用；加载失败返回 None，由调用方回退。

use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR};

/// 组图标资源 ID（winres 的 set_icon 固定分配为 1）。
const APP_ICON_ID: u16 = 1;

/// 图标加载尺寸：hIcon / 托盘（32px，含高 DPI 缩放余量）。
pub const LARGE: i32 = 32;
/// 图标加载尺寸：hIconSm（16px，标题栏小图标）。
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
