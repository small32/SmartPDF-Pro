#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod document;
mod icon;
mod sysmenu;
mod tab;

use std::path::PathBuf;
use std::sync::Arc;
use winit::event_loop::EventLoop;

fn main() {
    env_logger::init();

    // Finder 的打开请求可能在第一个 egui 帧之前到达，命令通道必须先就绪。
    sysmenu::init_channel();

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

    // 自己创建 event loop，才能在 AppKit 开始分发启动事件之前，为 winit 已安装的
    // NSApplicationDelegate 补上 application:openURLs:。不能替换整个 delegate，
    // 否则会破坏 winit 的生命周期与窗口事件处理。
    let event_loop = EventLoop::<eframe::UserEvent>::with_user_event()
        .build()
        .expect("创建事件循环失败");
    sysmenu::install_file_open_handlers();

    let mut native_app = eframe::create_native(
        "SmartPDF Pro",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::SmartPdfApp::new(cc, files)))),
        &event_loop,
    );
    event_loop.run_app(&mut native_app).expect("启动失败");
}
