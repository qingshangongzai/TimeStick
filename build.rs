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

// 构建期把 assets/icon.ico 与版本信息嵌入 exe：
// - 图标（组图标资源，winres 分配 ID 1）：资源管理器显示文件图标；运行时经 src/icon.rs 的 LoadImageW 按尺寸加载为托盘/窗口图标
// - 版本信息：产品名/文件说明/公司/版权，属性页与任务管理器显示；版本号由 winres 默认取自 Cargo.toml（唯一真源）
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winres::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set("ProductName", "时间贴")
            .set("FileDescription", "时间贴")
            .set("CompanyName", "青山公仔")
            .set("LegalCopyright", "©️ 2026 青山公仔")
            .compile()
            .expect("winres: 嵌入资源失败");
    }
}
