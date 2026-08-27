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

//! 开机自启动：写/删 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的
//! TimeStick 值，以该值存在为启用判定（默认关闭，由用户经托盘菜单主动开启）。
//! 任务管理器「禁用」不删 Run 值、只写 StartupApproved 禁用标志——本模块不感知
//! 该标志，仅在开启时顺带清除残留，避免 Run 值存在却仍被系统抑制。

use windows::core::{w, PCWSTR, Result, HRESULT};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("TimeStick");
/// 任务管理器「禁用」启动项的落点：不删 Run 值，只在此键下写二进制禁用标志
/// （被禁项首字节 03）。该键下无 TimeStick 条目即默认启用。
const STARTUP_APPROVED_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");

/// 当前是否已开机自启动：Run 键下存在 TimeStick 值即视为启用（不校验值内容，
/// 也不感知任务管理器「禁用」留下的 StartupApproved 标志）；任何失败 → false。
pub fn is_enabled() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_QUERY_VALUE, &mut key) != ERROR_SUCCESS {
            return false;
        }
        let found = RegQueryValueExW(key, VALUE_NAME, None, None, None, None) == ERROR_SUCCESS;
        let _ = RegCloseKey(key);
        found
    }
}

/// 开启/关闭开机自启动。开启时写入带引号的 exe 完整路径（UTF-16 含结尾 null，
/// 引号保证含空格的路径被正确解析），并清除任务管理器「禁用」标志残留；
/// 关闭时删除该值（值已不存在视为成功，如 Run 值曾被外部删除）。
pub fn set_enabled(on: bool) -> Result<()> {
    // 先取 exe 路径再开注册表键：路径获取失败时无需动键
    let data = if on {
        let exe = std::env::current_exe()?;
        let mut data: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().collect();
        data.push(0);
        Some(data)
    } else {
        None
    };
    unsafe {
        let mut key = HKEY::default();
        let err = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if err != ERROR_SUCCESS {
            return Err(HRESULT::from_win32(err.0).into());
        }
        let result = match &data {
            Some(d) => {
                // RegSetValueExW 接收字节切片：把 UTF-16 编码（含结尾 null）按字节传入
                let bytes = std::slice::from_raw_parts(d.as_ptr() as *const u8, d.len() * 2);
                RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes))
            }
            None => RegDeleteValueW(key, VALUE_NAME),
        };
        let _ = RegCloseKey(key);
        if result == ERROR_SUCCESS || (result == ERROR_FILE_NOT_FOUND && !on) {
            // 开启成功后清除「禁用」标志残留：不清理则 Run 值存在也可能仍被系统抑制
            if on {
                clear_startup_approved_flag();
            }
            Ok(())
        } else {
            Err(HRESULT::from_win32(result.0).into())
        }
    }
}

/// 清除任务管理器「禁用」标志（StartupApproved 键下无 TimeStick 条目即默认启用）；
/// 键/值不存在或删除失败均静默——Run 值已写入，常见场景本就无残留，属尽力修复。
fn clear_startup_approved_flag() {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, STARTUP_APPROVED_KEY, None, KEY_SET_VALUE, &mut key) != ERROR_SUCCESS {
            return;
        }
        let _ = RegDeleteValueW(key, VALUE_NAME);
        let _ = RegCloseKey(key);
    }
}
