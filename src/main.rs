#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod document;
mod icon;
mod sysmenu;
mod tab;

use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    env_logger::init();

    // 必须在 eframe::run_native（[NSApp run]）之前注册 odoc（打开文档）处理器：
    // LaunchServices 在 app 完成启动后立即投递「打开方式/双击」事件，
    // 若等 egui 首帧再注册，事件先到会丢失，双击 PDF 报「无法打开该格式」。
    sysmenu::install_odoc_early();

    // 命令行传入的文档
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            // 窗口标题留空：macOS 系统菜单栏已显示 App 名，
            // 标题栏再显示一次会造成重复。
            .with_title("")
            // 不显式设置时 eframe 会用自己的默认图标覆盖 macOS Dock 图标
            .with_icon(Arc::new(icon::egui_icon())),
            // 注意：不要在这里用 with_fullsize_content_view / with_movable_by_background ——
            // eframe 0.36 的 winit 集成不会转发到原生窗口。统一标题栏由
            // sysmenu::setup_unified_titlebar() 在首帧用 objc2 直接设置 NSWindow 样式。
        ..Default::default()
    };

    eframe::run_native(
        "SmartPDF Pro",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::SmartPdfApp::new(cc, files)))),
    )
    .expect("启动失败");
}