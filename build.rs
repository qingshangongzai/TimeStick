// 构建期把 assets/icon.ico 嵌入 exe（组图标资源，winres 分配 ID 1）：
// 资源管理器显示文件图标；运行时经 src/icon.rs 的 LoadImageW 按尺寸加载为托盘/窗口图标。
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winres::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("winres: 嵌入图标失败");
    }
}
