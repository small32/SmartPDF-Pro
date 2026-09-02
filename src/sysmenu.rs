//! macOS 系统菜单栏（屏幕顶部）。
//!
//! egui 只能画窗口内菜单；系统菜单栏需要原生 NSMenu。这里用 objc2 构建
//! 原生菜单栏，菜单动作通过全局 channel 转发给 egui App（见 [`crate::app`]）。

use std::cell::RefCell;
use std::ffi::CString;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use objc2::define_class;
use objc2::extern_conformance;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{MainThreadOnly, MainThreadMarker};
use objc2_app_kit::{
    NSEventModifierFlags, NSAnimationContext, NSApplication, NSImage, NSMenuItem, NSMenu, NSScreen,
    NSWindowButton, NSWindowDidEndLiveResizeNotification, NSWindowDidResizeNotification,
    NSWindowStyleMask, NSWindowTitleVisibility, NSWindowWillStartLiveResizeNotification,
};
use objc2_foundation::{NSData, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString};

use eframe::egui::Context;

/// 系统菜单动作（经 channel 转给 egui App 处理）。
#[derive(Debug, Clone)]
pub enum SysCmd {
    Open,
    /// 由 Finder「打开方式」/双击经 application:openFiles: 转来的文件路径。
    OpenFiles(Vec<String>),
    Close,
    Prev,
    Next,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ZoomPercent(isize),
    FitWidth,
    Single,
    Continuous,
    /// 从当前页开始放映（⌘Return）。
    Presentation,
    /// 从头开始放映（⇧⌘Return）。
    PresentationFromBeginning,
    /// 打开「设置」窗口（字体选择等）。
    Settings,
}

static TX: OnceLock<Mutex<Sender<SysCmd>>> = OnceLock::new();
static RX: OnceLock<Mutex<Receiver<SysCmd>>> = OnceLock::new();

/// egui 上下文：供原生 resize 回调在「实时缩放」期间直接请求重绘（见 [`register_resize_watcher`]）。
static CTX: OnceLock<Context> = OnceLock::new();

/// 是否处于实时缩放中（由开始/结束实时缩放通知维护）。
static LIVE_RESIZE: AtomicBool = AtomicBool::new(false);

/// 保存 egui 上下文，使 `NSWindowDidResizeNotification` 回调能强制重绘。
pub fn set_context(ctx: Context) {
    let _ = CTX.set(ctx);
}

/// 窗口是否正在实时缩放（供 `ui()` 在缩放期间强制连续重绘）。
pub fn live_resizing() -> bool {
    LIVE_RESIZE.load(Ordering::Relaxed)
}

/// 初始化命令通道（须在安装菜单前调用一次）。
pub fn init_channel() {
    let (tx, rx) = channel();
    let _ = TX.set(Mutex::new(tx));
    let _ = RX.set(Mutex::new(rx));
}

/// 系统菜单触发后发送命令。
fn send(cmd: SysCmd) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(cmd);
    }
}

/// egui 每帧取走待处理的系统命令。
pub fn take() -> Vec<SysCmd> {
    let mut out = Vec::new();
    if let Some(rx) = RX.get() {
        while let Ok(c) = rx.lock().unwrap().try_recv() {
            out.push(c);
        }
    }
    out
}

// 菜单动作的目标对象：每个方法把对应命令发往 channel。
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SmartPDFProMenuTarget"]
    pub struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openDocument:))]
        fn open_document(&self, _sender: &AnyObject) {
            send(SysCmd::Open);
        }

        #[unsafe(method(closeDocument:))]
        fn close_document(&self, _sender: &AnyObject) {
            send(SysCmd::Close);
        }

        #[unsafe(method(prevPage:))]
        fn prev_page(&self, _sender: &AnyObject) {
            send(SysCmd::Prev);
        }

        #[unsafe(method(nextPage:))]
        fn next_page(&self, _sender: &AnyObject) {
            send(SysCmd::Next);
        }

        #[unsafe(method(firstPage:))]
        fn first_page(&self, _sender: &AnyObject) {
            send(SysCmd::First);
        }

        #[unsafe(method(lastPage:))]
        fn last_page(&self, _sender: &AnyObject) {
            send(SysCmd::Last);
        }

        #[unsafe(method(zoomIn:))]
        fn zoom_in(&self, _sender: &AnyObject) {
            send(SysCmd::ZoomIn);
        }

        #[unsafe(method(zoomOut:))]
        fn zoom_out(&self, _sender: &AnyObject) {
            send(SysCmd::ZoomOut);
        }

        #[unsafe(method(zoomReset:))]
        fn zoom_reset(&self, _sender: &AnyObject) {
            send(SysCmd::ZoomReset);
        }

        #[unsafe(method(zoomToPercent:))]
        fn zoom_to_percent(&self, sender: &AnyObject) {
            let item: &NSMenuItem =
                unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            send(SysCmd::ZoomPercent(item.tag()));
        }

        #[unsafe(method(toggleFitWidth:))]
        fn toggle_fit_width(&self, _sender: &AnyObject) {
            send(SysCmd::FitWidth);
        }

        #[unsafe(method(singlePageView:))]
        fn single_page_view(&self, _sender: &AnyObject) {
            send(SysCmd::Single);
        }

        #[unsafe(method(continuousView:))]
        fn continuous_view(&self, _sender: &AnyObject) {
            send(SysCmd::Continuous);
        }

        #[unsafe(method(startPresentation:))]
        fn start_presentation(&self, _sender: &AnyObject) {
            send(SysCmd::Presentation);
        }

        #[unsafe(method(startPresentationFromBeginning:))]
        fn start_presentation_from_beginning(&self, _sender: &AnyObject) {
            send(SysCmd::PresentationFromBeginning);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: &AnyObject) {
            send(SysCmd::Settings);
        }
    }
);

extern_conformance!(
    unsafe impl NSObjectProtocol for MenuTarget {}
);

// odoc 处理器：winit 强占 NSApplication delegate（断言必须是它自己的 ApplicationDelegate），
// 因此不能 setDelegate 覆盖。改为向 NSAppleEventManager 注册「打开文档」事件处理器，
// Finder 双击 / 「打开方式」的 odoc Apple Event 会落到这里，我们从中提取文件路径。
// 提取全程用 msg_send! 原始调用，避免引入 objc2-core-services 类型。
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SmartPDFOdocHandler"]
    pub struct OdocHandler;

    impl OdocHandler {
        #[unsafe(method(handleEvent:withReplyEvent:))]
        fn handle_event(&self, event: &AnyObject, _reply: &AnyObject) {
            log::info!("odoc handler 被调用");
            let paths = extract_odoc_paths(event);
            log::info!("odoc 提取到 {} 个路径", paths.len());
            if !paths.is_empty() {
                log::info!("收到系统打开请求：{} 个文件", paths.len());
                send(SysCmd::OpenFiles(paths));
            }
        }
    }
);

extern_conformance!(
    unsafe impl NSObjectProtocol for OdocHandler {}
);

/// 从 odoc Apple Event 中提取文件路径。
///
/// 事件结构：directObject 参数是文件列表（NSAppleEventDescriptor 的 list），
/// 每项是文件 URL 描述符；逐个取 fileURLValue → path 得到路径字符串。
fn extract_odoc_paths(event: &AnyObject) -> Vec<String> {
    const KEY_DIRECT_OBJECT: u32 = 0x2D2D2D2D; // '----'
    let mut out = Vec::new();
    unsafe {
        // 取 directObject 参数（文件列表描述符）
        let direct: *mut AnyObject =
            msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        if direct.is_null() {
            return out;
        }
        let count: usize = msg_send![direct, numberOfItems];
        for i in 1..=count {
            let item: *mut AnyObject = msg_send![direct, descriptorAtIndex: i];
            if item.is_null() {
                continue;
            }
            // 文件 URL 描述符 → NSURL → path
            let url: *mut AnyObject = msg_send![item, fileURLValue];
            if url.is_null() {
                continue;
            }
            let path: *mut AnyObject = msg_send![url, path];
            if path.is_null() {
                continue;
            }
            let s: &NSString = &*(path as *const NSString);
            let text = s.to_string();
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

/// 向 NSAppleEventManager 注册 odoc 事件处理器。
fn install_odoc_handler(mtm: MainThreadMarker) {
    const CORE_CLASS: u32 = 0x636F7265; // 'core' kCoreEventClass
    const OPEN_DOCS: u32 = 0x6F646F63; // 'odoc' kAEOpenDocuments
    unsafe {
        // 保持处理器对象存活（NSAppleEventManager 对 handler 是弱引用）。
        let handler = OdocHandler::new();
        let _ = ODOC_HANDLER.with(|h| h.borrow_mut().replace(handler.clone()));
        let sel = Sel::register(&CString::new("handleEvent:withReplyEvent:").unwrap());
        let cls = objc2::runtime::AnyClass::get(&CString::new("NSAppleEventManager").unwrap())
            .expect("NSAppleEventManager");
        let aem: *mut AnyObject = msg_send![cls, sharedAppleEventManager];
        let _: () = msg_send![
            aem,
            setEventHandler: &*handler,
            andSelector: sel,
            forEventClass: CORE_CLASS,
            andEventID: OPEN_DOCS
        ];
        log::info!("odoc 处理器注册成功（{} 保持存活）", ODOC_HANDLER.with(|h| h.borrow().is_some()));
        let _ = mtm;
    }
}

// 实时缩放监听：macOS 拖拽缩放时窗口运行在 modal tracking run loop，自定义
// NSWindow 样式（FullSizeContentView + MovableByWindowBackground）会干扰 winit 的
// resize→repaint 链路，导致缩放期间不重绘、内容被 Core Animation 拉伸，松手才跳变。
// 这里用 Cocoa 的 NSWindowDidResizeNotification 在每一步缩放时直接 request_repaint，
// 该通知在 tracking 模式下也会持续派发，从而强制 egui 同步重绘、不再拉伸。
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SmartPDFResizeWatcher"]
    pub struct ResizeWatcher;

    impl ResizeWatcher {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _note: &AnyObject) {
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
        }

        #[unsafe(method(windowWillStartLiveResize:))]
        fn window_will_start_live_resize(&self, _note: &AnyObject) {
            LIVE_RESIZE.store(true, Ordering::Relaxed);
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
        }

        #[unsafe(method(windowDidEndLiveResize:))]
        fn window_did_end_live_resize(&self, _note: &AnyObject) {
            LIVE_RESIZE.store(false, Ordering::Relaxed);
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
        }
    }
);

extern_conformance!(
    unsafe impl NSObjectProtocol for ResizeWatcher {}
);

thread_local! {
    /// 保持菜单目标对象存活（菜单 item 对 target 是弱引用）。
    static TARGET: RefCell<Option<Retained<MenuTarget>>> = RefCell::new(None);
    /// 保持 odoc 处理器存活（NSAppleEventManager 对 handler 是弱引用）。
    static ODOC_HANDLER: RefCell<Option<Retained<OdocHandler>>> = RefCell::new(None);
    /// 保持 Dock 图标存活（AppKit 对 applicationIconImage 是弱引用，不保活会回退黑白）。
    static DOCK_ICON: RefCell<Option<Retained<NSImage>>> = RefCell::new(None);
    /// 保持 resize 监听对象存活（NSNotificationCenter 不持有 observer）。
    static WATCHER: RefCell<Option<Retained<ResizeWatcher>>> = RefCell::new(None);
    /// 双击缩放前的窗口 frame：再次双击还原（等价原生 zoom 的记忆行为）。
    static ZOOM_RESTORE: RefCell<Option<(f64, f64, f64, f64)>> = const { RefCell::new(None) };
}

impl MenuTarget {
    fn new() -> Retained<Self> {
        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let this = mtm.alloc::<Self>().set_ivars(());
            msg_send![super(this), init]
        }
    }
}

impl OdocHandler {
    fn new() -> Retained<Self> {
        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let this = mtm.alloc::<Self>().set_ivars(());
            msg_send![super(this), init]
        }
    }
}

/// 设置 Dock 栏图标：使用与窗口图标、`.app` 图标同源的内嵌 PNG。
///
/// 须在窗口创建后调用（见 `app.rs` 的首帧调用）：eframe/winit 建窗时会写入
/// 自己的图标，早于建窗设置会被覆盖。
pub fn set_dock_icon() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let data = NSData::with_bytes(crate::icon::ICON_PNG);
    let Some(img) = NSImage::initWithData(mtm.alloc::<NSImage>(), &data) else {
        log::warn!("Dock 图标解码失败");
        return;
    };
    // 保活强引用，防止 AppKit 弱引用导致图标失效回退为黑白
    let _ = DOCK_ICON.with(|d| d.borrow_mut().replace(img.clone()));
    let app = NSApplication::sharedApplication(mtm);
    unsafe {
        app.setApplicationIconImage(Some(&img));
    }
    log::info!("已设置 Dock 图标");
}

/// 把内容视图延伸到标题栏区域，让 egui 顶部面板（标签栏）能与左上的红黄绿
/// 三色按钮同处一行（macOS "unified titlebar" 效果，类似 Safari 标签栏）。
///
/// 注意：eframe 0.36 的 winit 集成**不会**把 `ViewportBuilder::with_fullsize_content_view`
/// 转发给原生窗口，直接设 `with_fullsize_content_view(true)` 无效（且会让标签栏被遮挡）。
/// 因此这里用 objc2 直接设置 NSWindow 样式。
///
/// 须在窗口创建后调用（见 `app.rs` 的首帧调用）。
pub fn setup_unified_titlebar() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    // resize 监听对象：整个生命周期保活（NSNotificationCenter 不持有 observer）
    let watcher = WATCHER.with(|w| {
        let mut g = w.borrow_mut();
        if g.is_none() {
            let obj = unsafe {
                let m = mtm.alloc::<ResizeWatcher>().set_ivars(());
                msg_send![super(m), init]
            };
            *g = Some(obj);
        }
        g.as_ref().unwrap().clone()
    });
    let center = NSNotificationCenter::defaultCenter();
    for window in windows.iter() {
        // 标题栏透明：egui 内容能透到标题栏之下
        window.setTitlebarAppearsTransparent(true);
        // 隐藏原生标题文字（标签栏承担标题角色）
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        // 内容延伸到整窗（含标题栏区域）
        let mask = window.styleMask();
        window.setStyleMask(mask | NSWindowStyleMask::FullSizeContentView);
        // 注意：不再用 setMovableByWindowBackground(true) —— 那会让整个窗口背景都能拖动，
        // 导致拖动「设置」窗口、滚动条时整个窗体跟着移动。窗口拖动改由 egui 在标签栏
        // 区域检测后发 ViewportCommand::StartDrag（见 app.rs::tabs_panel）。
        // 实时缩放：开始/结束维护 LIVE_RESIZE 标志（ui() 据此连续重绘），
        // 每一步缩放也都请求一次重绘，避免内容被 Core Animation 拉伸后跳变
        unsafe {
            center.addObserver_selector_name_object(
                watcher.as_ref(),
                objc2::sel!(windowDidResize:),
                Some(&NSWindowDidResizeNotification),
                Some(window.as_ref()),
            );
            center.addObserver_selector_name_object(
                watcher.as_ref(),
                objc2::sel!(windowWillStartLiveResize:),
                Some(&NSWindowWillStartLiveResizeNotification),
                Some(window.as_ref()),
            );
            center.addObserver_selector_name_object(
                watcher.as_ref(),
                objc2::sel!(windowDidEndLiveResize:),
                Some(&NSWindowDidEndLiveResizeNotification),
                Some(window.as_ref()),
            );
        }
    }
    log::info!("已启用统一标题栏");
}

/// 红绿黄三色按钮（红圆）中心相对窗口顶部的 y 坐标（点）。
///
/// 不同窗口样式下标题栏高度不同，硬编码会把 tab 栏与按钮错开；这里运行时
/// 直接读取原生 close 按钮的 frame，让 egui 标签栏据此垂直对齐。
pub fn titlebar_button_center_y() -> f32 {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let Some(win) = NSApplication::sharedApplication(mtm).keyWindow() else {
        return 14.0;
    };
    let Some(btn) = win.standardWindowButton(NSWindowButton::CloseButton) else {
        return 14.0;
    };
    let f = btn.frame();
    let cy = (f.origin.y + f.size.height / 2.0) as f32;
    log::info!("标题栏按钮中心 y = {cy:.1}（标签栏上边距将设为 {:.1}）", cy - 14.0);
    cy
}

/// 双击标题栏缩放（zoom）：等价 macOS 原生双击缩放，但用 **Core Animation 异步驱动**
/// 而非阻塞式 `zoom:` 动画。
///
/// 原生 `zoom:` 的动画在主线程以阻塞 modal 循环逐帧 setFrame，卡住 eframe 事件循环，
/// 动画期间 egui 不重绘，旧内容被 Core Animation 整体拉伸（文字、标签高度都变形）。
/// 这里改用 `NSAnimationContext` + `animator`：异步动画逐帧改变窗口尺寸，每步派发
/// `windowDidResize`（已注册的 ResizeWatcher 会 request_repaint），egui 在动画每一步
/// 都按新尺寸重排——文字字号、标签高度恒定，仅标签宽度逐帧自适应，平滑无拉伸。
pub fn zoom_key_window() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let Some(win) = NSApplication::sharedApplication(mtm).keyWindow() else {
        return;
    };
    // 目标 frame：已放大→还原保存值；否则→屏幕可见区（并记录当前 frame 供还原）
    let cur = win.frame();
    let cur_tuple = (cur.origin.x, cur.origin.y, cur.size.width, cur.size.height);
    let target = ZOOM_RESTORE.with(|z| z.borrow_mut().take());
    let (tx, ty, tw, th) = match target {
        Some(saved) => saved,
        None => {
            let Some(screen) = win.screen().or_else(|| NSScreen::mainScreen(mtm)) else {
                return;
            };
            let f = screen.visibleFrame();
            ZOOM_RESTORE.with(|z| *z.borrow_mut() = Some(cur_tuple));
            (f.origin.x, f.origin.y, f.size.width, f.size.height)
        }
    };

    // 异步动画：animator 代理让 setFrame 走 Core Animation（不阻塞事件循环）
    let block = block2::RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(0.25);
        let animator: *mut AnyObject = unsafe { msg_send![&win, animator] };
        let frame = NSRect::new(NSPoint::new(tx, ty), NSSize::new(tw, th));
        let _: () = unsafe { msg_send![animator, setFrame: frame, display: true] };
    });
    NSAnimationContext::runAnimationGroup(&block);
    log::info!(
        "zoom 动画：({:.0},{:.0} {:.0}x{:.0}) → ({tx:.0},{ty:.0} {tw:.0}x{th:.0})",
        cur.origin.x, cur.origin.y, cur.size.width, cur.size.height
    );
}

/// 安装系统菜单栏（在 egui 应用启动后调用一次，主线程）。
pub fn install() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let target = MenuTarget::new();
    let _ = TARGET.with(|t| t.borrow_mut().replace(target.clone()));
    let app = NSApplication::sharedApplication(mtm);
    // 文件打开（Finder 双击 / 「打开方式」）：winit 占用 NSApplication delegate
    // （断言必须是它自己的 ApplicationDelegate），不能 setDelegate。改为给
    // NSAppleEventManager 注册 odoc（打开文档）处理器，从 Apple Event 中取文件路径。
    install_odoc_handler(mtm);
    let menu_bar = NSMenu::new(mtm);

    // 应用菜单（关于 / 设置 / 退出）
    let app_menu = NSMenu::new(mtm);
    app_menu.addItem(&menu_item(
        mtm,
        None,
        "关于 SmartPDF Pro",
        "orderFrontStandardAboutPanel:",
        "",
    ));
    app_menu.addItem(&menu_item(mtm, Some(&target), "字体设置…", "openSettings:", ","));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&menu_item(mtm, None, "退出 SmartPDF Pro", "terminate:", "q"));
    menu_bar.addItem(&top_menu(mtm, "SmartPDF Pro", &app_menu));

    // 文件
    let file_menu = NSMenu::new(mtm);
    file_menu.addItem(&menu_item(mtm, Some(&target), "打开…", "openDocument:", "o"));
    file_menu.addItem(&menu_item(mtm, Some(&target), "关闭", "closeDocument:", "w"));
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    file_menu.addItem(&menu_item(mtm, None, "退出", "terminate:", "q"));
    menu_bar.addItem(&top_menu(mtm, "文件", &file_menu));

    // 视图
    let view_menu = NSMenu::new(mtm);
    view_menu.addItem(&menu_item(mtm, Some(&target), "单页", "singlePageView:", ""));
    view_menu.addItem(&menu_item(mtm, Some(&target), "连续显示页面", "continuousView:", ""));
    view_menu.addItem(&NSMenuItem::separatorItem(mtm));
    view_menu.addItem(&menu_item(mtm, Some(&target), "统一页宽", "toggleFitWidth:", ""));
    view_menu.addItem(&NSMenuItem::separatorItem(mtm));
    // 演示：⌘↩ 从当前页；⇧⌘↩ 从头开始（Return 键的 keyEquivalent 是 "\r"）
    view_menu.addItem(&menu_item_mods(
        mtm,
        Some(&target),
        "从当前页开始放映  ⌘↩",
        "startPresentation:",
        "\r",
        NSEventModifierFlags::Command,
    ));
    view_menu.addItem(&menu_item_mods(
        mtm,
        Some(&target),
        "从头开始放映  ⇧⌘↩",
        "startPresentationFromBeginning:",
        "\r",
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
    ));
    menu_bar.addItem(&top_menu(mtm, "视图", &view_menu));

    // 前往
    let goto_menu = NSMenu::new(mtm);
    goto_menu.addItem(&menu_item(mtm, Some(&target), "下一页", "nextPage:", ""));
    goto_menu.addItem(&menu_item(mtm, Some(&target), "上一页", "prevPage:", ""));
    goto_menu.addItem(&NSMenuItem::separatorItem(mtm));
    goto_menu.addItem(&menu_item(mtm, Some(&target), "首页", "firstPage:", ""));
    goto_menu.addItem(&menu_item(mtm, Some(&target), "末页", "lastPage:", ""));
    menu_bar.addItem(&top_menu(mtm, "前往", &goto_menu));

    // 缩放
    let zoom_menu = NSMenu::new(mtm);
    zoom_menu.addItem(&menu_item(mtm, Some(&target), "适应宽度", "toggleFitWidth:", ""));
    zoom_menu.addItem(&menu_item(mtm, Some(&target), "实际大小", "zoomReset:", "0"));
    zoom_menu.addItem(&NSMenuItem::separatorItem(mtm));
    zoom_menu.addItem(&menu_item(mtm, Some(&target), "放大", "zoomIn:", "+"));
    zoom_menu.addItem(&menu_item(mtm, Some(&target), "缩小", "zoomOut:", "-"));
    zoom_menu.addItem(&NSMenuItem::separatorItem(mtm));
    for pct in [200isize, 150, 125, 100, 75, 50] {
        zoom_menu.addItem(&menu_item_tag(
            mtm,
            Some(&target),
            &format!("{pct}%"),
            "zoomToPercent:",
            "",
            pct,
        ));
    }
    menu_bar.addItem(&top_menu(mtm, "缩放", &zoom_menu));

    app.setMainMenu(Some(&menu_bar));
}

fn menu_item(
    mtm: MainThreadMarker,
    target: Option<&MenuTarget>,
    title: &str,
    selector: &str,
    key: &str,
) -> Retained<NSMenuItem> {
    menu_item_tag(mtm, target, title, selector, key, 0)
}

/// 创建带自定义修饰键的菜单项（如 ⇧⌘ 组合快捷键）。
fn menu_item_mods(
    mtm: MainThreadMarker,
    target: Option<&MenuTarget>,
    title: &str,
    selector: &str,
    key: &str,
    mods: NSEventModifierFlags,
) -> Retained<NSMenuItem> {
    let item = menu_item(mtm, target, title, selector, key);
    item.setKeyEquivalentModifierMask(mods);
    item
}

fn menu_item_tag(
    mtm: MainThreadMarker,
    target: Option<&MenuTarget>,
    title: &str,
    selector: &str,
    key: &str,
    tag: isize,
) -> Retained<NSMenuItem> {
    let sel = CString::new(selector).unwrap();
    unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str(title),
            Some(Sel::register(&sel)),
            &NSString::from_str(key),
        );
        item.setTag(tag);
        if let Some(t) = target {
            let _: () = msg_send![&item, setTarget: t];
        }
        item
    }
}

fn top_menu(mtm: MainThreadMarker, title: &str, submenu: &NSMenu) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setSubmenu(Some(submenu));
    item
}