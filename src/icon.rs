//! 应用图标：窗口图标、Dock 图标与 .app 包图标的**唯一来源**。
//!
//! macOS 上 eframe/winit 会用默认的 egui 图标覆盖 Dock 图标，所以窗口图标必须
//! 显式指定；`.app` 的 icns 也必须由同一张 PNG 生成（见 `build_app.sh`），
//! 否则 Dock 里显示的图标会和 Finder 里的 App 图标不一致。

/// 图标源图（1024×1024 PNG），由 `examples/gen_icon` 生成后提交入库。
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-1024.png");

/// 解码为 egui 窗口图标数据（RGBA，非预乘）。
pub fn egui_icon() -> egui::IconData {
    let img = image::load_from_memory(ICON_PNG)
        .expect("内嵌图标资源损坏")
        .into_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}
