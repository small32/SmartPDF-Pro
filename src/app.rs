//! 主界面（egui 纯 Rust 方案）。
//!
//! 布局（macOS 原生风格，单窗口单文档）：
//! - 顶部标题栏：左上留空（红黄绿三色按钮区），中央显示当前文件名，
//!   右上放「侧栏开关」「＋ 打开」两个图标按钮
//! - 左侧：页面缩略图导航（可收起，F9 / 标题栏按钮 / 菜单切换）
//! - 中央：整篇文档纵向连续滚动（滚动条拖动浏览全部页面）
//! - 底部：状态栏（页码 / 缩放比例 / 状态消息）

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontData, FontDefinitions, FontId, Key, Layout, Modifiers,
    ScrollArea, Vec2,
};
use eframe::egui::load::SizedTexture;

use crate::document::scale_px_per_pt;
use crate::tab::DocTab;

/// 页面浏览模式。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Single,
    Continuous,
}

/// 缩略图渲染缩放（低分辨率小图，与正文缩放分开缓存）。
const THUMB_ZOOM: f32 = 0.3;
/// 缩略图显示宽度（逻辑点）。
const THUMB_WIDTH: f32 = 108.0;
const PAGE_GAP: f32 = 18.0;

/// 常用缩放档位（与菜单「缩放」中的百分比一致）。
const ZOOM_STEPS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

// ---- 标题栏尺寸 ----
/// 标题栏内容行高：与 macOS 标题栏等高，和左上红黄绿三色按钮齐平。
const TITLEBAR_H: f32 = 28.0;
/// 标题栏三色按钮占据的宽度；文件名居中时两侧各预留这么宽，避免遮挡。
const TITLEBAR_BUTTONS_W: f32 = 72.0;
/// 标题栏右侧图标按钮的边长。
const TITLEBAR_BTN: f32 = 26.0;
/// 右侧按钮与窗口右缘的间距。
const TITLEBAR_BTN_MARGIN: f32 = 10.0;
/// 右侧两个按钮之间的间距。
const TITLEBAR_BTN_GAP: f32 = 6.0;

/// 去掉标题末尾的扩展名（如 "demo.pdf" → "demo"；"v1.2 报告" 保留）。
/// 只在最后一段像扩展名（长度 ≤ 5 且前段非空）时去除，避免误伤文件名中的点。
fn strip_extension(title: &str) -> String {
    match title.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 5 => {
            stem.to_string()
        }
        _ => title.to_string(),
    }
}

/// 适应宽度的结果**向下**吸附到最近的标准档位（差距 ≤12% 时生效），
/// 让默认窗口尺寸下直接显示整洁的 100% / 125% 等百分比而非 109% 这类碎数。
/// 只向下吸附：页面永远不会比精确适配更宽，避免左右边缘被裁切。
fn snap_fit_zoom(fit: f32) -> f32 {
    ZOOM_STEPS
        .iter()
        .rev()
        .copied()
        .find(|&s| s <= fit && (fit - s) / s <= 0.12)
        .unwrap_or(fit)
}

/// 手动缩放（⌘+/⌘-）的结果就近吸附到标准档位（±6% 以内），
/// 连击几次后仍落在干净百分比上。
fn snap_zoom_nearest(z: f32) -> f32 {
    ZOOM_STEPS
        .iter()
        .copied()
        .min_by(|a, b| (z - a).abs().partial_cmp(&(z - b).abs()).unwrap())
        .filter(|&s| (z - s).abs() / s <= 0.06)
        .unwrap_or(z)
}

/// 等比调节颜色明度（f < 1 变暗，> 1 变亮），用于 chrome 面板底色。
fn shade(c: egui::Color32, f: f32) -> egui::Color32 {
    let ch = |v: u8| (v as f32 * f).clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(ch(c.r()), ch(c.g()), ch(c.b()), c.a())
}

/// 标题栏 / 侧栏 / 状态栏的「chrome」底色：比正文区深（浅色主题则浅）一档，
/// 让导航区与内容区有安静的层次区分。
fn panel_fill(ui: &egui::Ui) -> egui::Color32 {
    let v = ui.visuals();
    shade(v.panel_fill, if v.dark_mode { 0.88 } else { 0.965 })
}

/// chrome 与正文区之间的发丝分割线（1px，深浅主题各自取灰）。
fn hairline(ui: &egui::Ui) -> egui::Stroke {
    let v = ui.visuals();
    let color = if v.dark_mode {
        Color32::from_gray(48)
    } else {
        Color32::from_gray(214)
    };
    egui::Stroke::new(1.0, color)
}

/// 标题的排版任务：`max_width` 为 `f32::INFINITY` 时完整显示不截断。
fn title_job(
    font_id: &egui::FontId,
    color: egui::Color32,
    title: &str,
    max_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(
        title.to_owned(),
        egui::TextFormat {
            font_id: font_id.clone(),
            color,
            // 同一行内中英文混排时按中线对齐，避免扩展名比中文文件名高一截。
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    if max_width.is_finite() {
        job.wrap.overflow_character = Some('…');
    }
    job
}

/// 标题栏「＋ 打开文档」按钮。图标用画笔自绘（两条正交线），
/// 不依赖字体符号在不同分辨率/字体下的渲染差异。
fn plus_button(ui: &mut egui::Ui, center: egui::Pos2) -> bool {
    let rect = egui::Rect::from_center_size(center, egui::vec2(TITLEBAR_BTN, TITLEBAR_BTN));
    let resp = ui.allocate_rect(rect, egui::Sense::click());
    let (dark, text) = {
        let v = ui.visuals();
        (v.dark_mode, v.text_color())
    };
    if resp.hovered() {
        let fill = if dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 42)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 26)
        };
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(7), fill);
    }
    let stroke = egui::Stroke::new(1.7, text);
    let r = 4.6;
    ui.painter().line_segment(
        [egui::pos2(center.x - r, center.y), egui::pos2(center.x + r, center.y)],
        stroke,
    );
    ui.painter().line_segment(
        [egui::pos2(center.x, center.y - r), egui::pos2(center.x, center.y + r)],
        stroke,
    );
    resp.on_hover_text("打开文档（⌘O）").clicked()
}

/// 标题栏「缩略图侧栏」开关按钮：圆角矩形 + 左侧竖线的经典侧栏图标。
/// `active`（侧栏可见）时底色常显、图标着色，开关状态一目了然。
fn thumbs_button(ui: &mut egui::Ui, center: egui::Pos2, active: bool) -> bool {
    let rect = egui::Rect::from_center_size(center, egui::vec2(TITLEBAR_BTN, TITLEBAR_BTN));
    let resp = ui.allocate_rect(rect, egui::Sense::click());
    let (dark, strong, weak) = {
        let v = ui.visuals();
        (v.dark_mode, v.strong_text_color(), v.weak_text_color())
    };
    if active || resp.hovered() {
        let alpha = if active { 44 } else { 30 };
        let fill = if dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, alpha)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, alpha - 8)
        };
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(7), fill);
    }
    let stroke = egui::Stroke::new(1.4, if active { strong } else { weak });
    let icon = egui::Rect::from_center_size(center, egui::vec2(14.0, 14.0));
    ui.painter().rect_stroke(
        icon,
        egui::CornerRadius::same(3),
        stroke,
        egui::StrokeKind::Inside,
    );
    let split = icon.left() + 4.5;
    ui.painter().line_segment(
        [
            egui::pos2(split, icon.top() + 2.5),
            egui::pos2(split, icon.bottom() - 2.5),
        ],
        stroke,
    );
    resp.on_hover_text("缩略图侧栏（F9）").clicked()
}

pub struct SmartPdfApp {
    /// 当前文档（单窗口单文档；打开新文档替换旧的，旧渲染线程随通道关闭退出）。
    tab: Option<DocTab>,
    view_mode: ViewMode,
    status: String,
    /// 连续模式下需要滚动到的页（翻页/缩略图跳页后置位，下一帧应用）。
    scroll_target: Option<usize>,
    /// 是否处于演示模式（类 PPT 全屏单页）。
    presenting: bool,
    /// Dock 图标是否已在首帧设置（窗口创建后才能覆盖 winit 写入的默认图标）。
    dock_icon_done: bool,
    /// 统一标题栏是否已在首帧设置（内容延伸到标题栏，与三色按钮同行）。
    titlebar_done: bool,
    /// 标题栏内容行相对窗口顶部的上边距：垂直中心对齐三色按钮中心（运行时测得）。
    titlebar_top: f32,
    /// 本帧点击是否落在标题栏按钮上（用于抑制「双击标题栏放大」误触发）。
    titlebar_double_consumed: bool,
    /// 手动检测标题栏双击：上次单击的时间与位置。
    titlebar_dbl_time: f64,
    titlebar_dbl_pos: egui::Pos2,
    /// 上一帧视口尺寸：用于检测窗口缩放/拖拽，主动触发重绘避免界面滞后。
    last_viewport: egui::Vec2,
    /// 缩略图侧栏是否展开（F9 / 标题栏按钮 / 菜单「缩略图面板」切换）。
    show_thumbs: bool,
}

impl SmartPdfApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        setup_cjk_fonts(&cc.egui_ctx);
        // 原生系统菜单栏（动作经 channel 转发给本 App）
        crate::sysmenu::init_channel();
        // 注册 egui 上下文，供原生 resize 回调在实时缩放时强制重绘
        crate::sysmenu::set_context(cc.egui_ctx.clone());
        crate::sysmenu::install();
        let mut app = Self {
            tab: None,
            view_mode: ViewMode::Continuous,
            status: String::new(),
            scroll_target: None,
            presenting: false,
            dock_icon_done: false,
            titlebar_done: false,
            titlebar_top: 0.0,
            titlebar_double_consumed: false,
            titlebar_dbl_time: -1.0,
            titlebar_dbl_pos: egui::Pos2::ZERO,
            last_viewport: egui::Vec2::ZERO,
            show_thumbs: true,
        };
        for f in files {
            app.open_path(&f);
        }
        app
    }

    // ---- 打开 / 关闭 ----

    /// 打开文档（单文档模型：替换当前文档）。
    fn open_path(&mut self, path: &Path) {
        if self.tab.as_ref().is_some_and(|t| t.path == path) {
            self.status = format!("文档已打开：{}", path.display());
            return;
        }
        match DocTab::open(path) {
            Ok(tab) => {
                let title = tab.title.clone();
                // 旧 tab 置换析构 → 请求通道关闭 → 渲染线程退出，无泄漏
                self.tab = Some(tab);
                self.scroll_target = Some(0);
                self.status = format!("已打开「{title}」");
            }
            Err(e) => self.status = format!("无法打开 {}：{}", path.display(), e),
        }
    }

    fn pick_and_open(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("打开文档")
            .add_filter("支持文档", crate::document::SUPPORTED_EXTENSIONS)
            .add_filter("PDF", &["pdf"])
            .add_filter("所有文件", &["*"])
            .pick_file();
        if let Some(path) = file {
            self.open_path(&path);
        }
    }

    /// 关闭当前文档（应用保持运行）。
    fn close_doc(&mut self) {
        if let Some(t) = self.tab.take() {
            self.status = format!("已关闭「{}」", t.title);
            self.scroll_target = None;
        }
    }

    /// 设置缩略图侧栏展开/收起。收起会改变正文可用宽度，但视口宽度不变、
    /// 不会触发 fit_width 的重算判据，这里清零记录宽度强制重新适配。
    fn set_thumbs(&mut self, show: bool) {
        if self.show_thumbs == show {
            return;
        }
        self.show_thumbs = show;
        if let Some(t) = self.tab.as_mut() {
            t.fit_width_viewport = 0.0;
        }
    }

    fn toggle_thumbs(&mut self) {
        let show = !self.show_thumbs;
        self.set_thumbs(show);
    }

    // ---- 顶部标题栏 ----

    /// 标题栏：左上避开红黄绿三色按钮；文件名窗口居中（超长以 … 截断）；
    /// 右上放「侧栏开关」「＋ 打开」两个图标按钮，保持左上区域干净。
    fn titlebar_panel(&mut self, ui: &mut egui::Ui) {
        let (rect, _resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), TITLEBAR_H),
            egui::Sense::click(),
        );
        let center_y = rect.center().y;

        // 中央文件名（先快照，避免与后续 &mut self 的借用冲突）
        let (title, has_doc) = match self.tab.as_ref() {
            Some(t) => (strip_extension(&t.title), true),
            None => ("SmartPDF Pro".to_string(), false),
        };
        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let text_color = if has_doc {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let max_w = (rect.width() - TITLEBAR_BUTTONS_W * 2.0).max(60.0);
        let galley = ui
            .painter()
            .layout_job(title_job(&font_id, text_color, &title, max_w));
        ui.painter().galley(
            egui::pos2(
                rect.center().x - galley.mesh_bounds.center().x,
                center_y - galley.mesh_bounds.center().y,
            ),
            galley,
            text_color,
        );

        // 右侧按钮组：从右往左为 ＋ 打开、侧栏开关
        let plus_center = egui::pos2(rect.right() - TITLEBAR_BTN_MARGIN - TITLEBAR_BTN / 2.0, center_y);
        let side_center = egui::pos2(
            plus_center.x - TITLEBAR_BTN - TITLEBAR_BTN_GAP,
            center_y,
        );
        let plus_clicked = plus_button(ui, plus_center);
        let thumbs_clicked = thumbs_button(ui, side_center, self.show_thumbs);
        if plus_clicked || thumbs_clicked {
            self.titlebar_double_consumed = true;
        }
        if thumbs_clicked {
            self.toggle_thumbs();
        }
        if plus_clicked {
            self.pick_and_open();
        }
    }

    // ---- 页面导航 / 缩放 ----

    fn goto(&mut self, page: usize) {
        let Some(tab) = self.tab.as_mut() else {
            return;
        };
        let clamped = page.min(tab.doc.page_count.saturating_sub(1));
        if tab.page != clamped {
            tab.goto(clamped);
            self.scroll_target = Some(clamped);
        }
    }

    fn navigate(&mut self, delta: isize) {
        let target = match self.tab.as_ref() {
            Some(t) => (t.page as isize + delta).max(0) as usize,
            None => return,
        };
        self.goto(target);
    }

    fn zoom(&mut self, factor: f32) {
        let Some(tab) = self.tab.as_mut() else {
            return;
        };
        tab.zoom = snap_zoom_nearest((tab.zoom * factor).clamp(0.05, 8.0));
        tab.fit_width = false;
    }

    fn zoom_percent(&mut self, pct: f32) {
        let Some(tab) = self.tab.as_mut() else {
            return;
        };
        tab.zoom = (pct / 100.0).clamp(0.05, 8.0);
        tab.fit_width = false;
    }

    fn toggle_fit_width(&mut self) {
        let Some(tab) = self.tab.as_mut() else {
            return;
        };
        tab.fit_width = !tab.fit_width;
    }

    // ---- 渲染收集 ----

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(tab) = self.tab.as_mut() {
            if tab.poll_render(ctx) {
                ctx.request_repaint();
            }
        }
    }

    // ---- 左侧缩略图面板 ----

    fn thumb_panel(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tab.as_mut() else {
            return;
        };
        let page_count = tab.doc.page_count;

        // 平均缩略图高度（作为 show_rows 行高，只渲染可见缩略图）
        let mut sum_h = 0.0f32;
        for p in 0..page_count {
            let (w_pt, h_pt) = tab.doc.page_size_pt(p);
            sum_h += THUMB_WIDTH * h_pt / w_pt.max(0.01);
        }
        let row_h = sum_h / page_count.max(1) as f32 + 6.0;

        let mut jump_to: Option<usize> = None;
        ScrollArea::vertical()
            .id_salt("thumbs")
            .show_rows(ui, row_h, page_count, |ui, range| {
                for p in range {
                    let (w_pt, h_pt) = tab.doc.page_size_pt(p);
                    let h = THUMB_WIDTH * h_pt / w_pt.max(0.01);
                    // 只对可见缩略图发起渲染请求
                    tab.request_render(p, THUMB_ZOOM);
                    let panel_w = ui.available_width();
                    let resp = ui
                        .allocate_ui_with_layout(
                            Vec2::new(panel_w, h),
                            Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                if let Some(tid) = tab.display_texture_at(p, THUMB_ZOOM) {
                                    ui.image(SizedTexture::new(tid, Vec2::new(THUMB_WIDTH, h)));
                                } else {
                                    ui.allocate_space(Vec2::new(THUMB_WIDTH, h));
                                }
                            },
                        )
                        .response;
                    if resp.clicked() {
                        jump_to = Some(p);
                    }
                    ui.add_space(6.0);
                }
            });
        // 点击缩略图跳页（闭包外应用，避免与 tab 的借用冲突）
        if let Some(p) = jump_to {
            if let Some(t) = self.tab.as_mut() {
                t.goto(p);
            }
            self.scroll_target = Some(p);
        }
    }

    // ---- 中央文档面板 ----

    fn doc_panel(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tab.as_mut() else {
            // 未打开文档：给出打开指引（配合拖放打开）
            ui.centered_and_justified(|ui| {
                ui.weak(egui::RichText::new("把文件拖进窗口，或按 ⌘O 打开文档").small());
            });
            return;
        };
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let avail_w = ui.available_width();

        // 适应宽度：只在窗口宽度真实变化（>4px）时重算缩放。
        // 若每帧用 available_width 重算，垂直滚动条出现/消失会让宽度微变 → zoom 振荡、
        // 页面反复以不同缩放渲染（闪烁）。用 viewport 宽度做判据一次性更新。
        if tab.fit_width {
            let vw = ctx
                .input(|i| i.viewport().inner_rect.map(|r| r.width()).unwrap_or(0.0));
            if (vw - tab.fit_width_viewport).abs() > 4.0 {
                let (pw, _) = tab.page_size_pt();
                // 先按可视宽精确计算，再向下吸附到最近的标准百分比（100%/125%…）
                let fit = (avail_w / (pw * scale_px_per_pt(1.0))).clamp(0.05, 8.0);
                tab.zoom = snap_fit_zoom(fit);
                tab.fit_width_viewport = vw;
            }
        }

        let zoom = tab.zoom;
        let page_count = tab.doc.page_count;
        let px_per_pt = scale_px_per_pt(zoom);
        let gap = PAGE_GAP;

        match self.view_mode {
            ViewMode::Single => {
                let page = tab.page;
                tab.request_render(page, zoom);
                ui.centered_and_justified(|ui| {
                    if let Some(tid) = tab.display_texture_for(page) {
                        let (w_pt, h_pt) = tab.doc.page_size_pt(page);
                        let w = w_pt * px_per_pt / ppp;
                        let h = h_pt * px_per_pt / ppp;
                        ui.image(SizedTexture::new(tid, Vec2::new(w, h)));
                    }
                });
            }
            ViewMode::Continuous => {
                // 平均页高（作为 show_rows 的行高，近似可见区）
                let mut sum_h = 0.0;
                for p in 0..page_count {
                    sum_h += tab.doc.page_size_pt(p).1;
                }
                let avg_h = sum_h / page_count.max(1) as f32 * px_per_pt / ppp;
                let row_h = avg_h + gap;

                let mut scroll = ScrollArea::vertical().id_salt("doc").auto_shrink([false, false]);
                let mut jumped = false;
                if let Some(p) = self.scroll_target.take() {
                    scroll = scroll.vertical_scroll_offset(p as f32 * row_h);
                    jumped = true;
                }

                // 记录可见行范围的首行，滚动浏览时同步当前页（状态栏跟随）
                let mut first_visible: Option<usize> = None;
                scroll.show_rows(ui, row_h, page_count, |ui, range| {
                    first_visible = Some(range.start);
                    for p in range {
                        let (w_pt, h_pt) = tab.doc.page_size_pt(p);
                        let w = w_pt * px_per_pt / ppp;
                        let h = h_pt * px_per_pt / ppp;
                        // 请求渲染（未缓存/未在途才发送）
                        tab.request_render(p, zoom);
                        ui.allocate_ui_with_layout(
                            Vec2::new(avail_w, h),
                            Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                if let Some(tid) = tab.display_texture_for(p) {
                                    ui.image(SizedTexture::new(tid, Vec2::new(w, h)));
                                } else {
                                    ui.allocate_space(Vec2::new(w, h));
                                }
                            },
                        );
                        ui.add_space(gap);
                    }
                });
                // 滚动浏览时同步当前页；跳页那一帧除外，避免覆盖刚设置的页码
                if !jumped {
                    if let Some(p) = first_visible {
                        let p = p.min(page_count.saturating_sub(1));
                        if p != tab.page {
                            tab.page = p;
                        }
                    }
                }
            }
        }
    }

    // ---- 底部状态栏 ----

    /// 状态栏：左侧固定两个信息槽（页码 / 缩放），右侧放状态消息或打开指引。
    /// 恒定行高、等距槽位，避免消息出现/消失时高度跳动或排版不齐。
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let dim = ui.visuals().weak_text_color();
        let info = |t: String| egui::RichText::new(t).small().color(dim);
        // 先快照要显示的内容，避免闭包借用冲突
        let (left, right) = match self.tab.as_ref() {
            Some(t) => {
                let zoom_pct = (t.zoom * 100.0).round() as i32;
                (
                    format!("页 {} / {}", t.page + 1, t.doc.page_count),
                    format!("缩放 {zoom_pct}%"),
                )
            }
            None => ("未打开文档".to_string(), String::new()),
        };
        let hint = if self.tab.is_none() {
            "⌘O 打开文档，或把文件拖进窗口"
        } else {
            self.status.as_str()
        };
        ui.horizontal(|ui| {
            ui.label(info(left));
            ui.add_space(14.0);
            ui.label(info(right));
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if !hint.is_empty() {
                    ui.label(info(hint.to_string()));
                }
            });
        });
    }

    // ---- 快捷键 ----

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // ---- 演示模式快捷键 ----
        if self.presenting {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.exit_presentation(ctx);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowRight))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageDown))
            {
                self.navigate(1);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowLeft))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageUp))
            {
                self.navigate(-1);
            }
            return;
        }

        let cmd = Modifiers::COMMAND;
        // 进入演示：⌘P / F5
        if ctx.input_mut(|i| i.consume_key(cmd, Key::P))
            || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5))
        {
            self.start_presentation(ctx);
        }
        // 收起/展开缩略图侧栏：F9（菜单「视图 → 缩略图面板」同快捷键）
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F9)) {
            self.toggle_thumbs();
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::O)) {
            self.pick_and_open();
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::W)) {
            self.close_doc();
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::Equals)) || ctx.input_mut(|i| i.consume_key(cmd, Key::Plus)) {
            self.zoom(1.25);
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::Minus)) {
            self.zoom(1.0 / 1.25);
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::Num0)) {
            self.zoom_percent(100.0);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageDown)) {
            self.navigate(1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageUp)) {
            self.navigate(-1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
            self.navigate(1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
            self.navigate(-1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Home)) {
            self.goto(0);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::End)) {
            self.goto(usize::MAX);
        }
    }

    // ---- 系统菜单命令处理 ----

    fn poll_sys_commands(&mut self, ctx: &egui::Context) {
        for cmd in crate::sysmenu::take() {
            match cmd {
                crate::sysmenu::SysCmd::Open => self.pick_and_open(),
                crate::sysmenu::SysCmd::Close => self.close_doc(),
                crate::sysmenu::SysCmd::Prev => self.navigate(-1),
                crate::sysmenu::SysCmd::Next => self.navigate(1),
                crate::sysmenu::SysCmd::First => self.goto(0),
                crate::sysmenu::SysCmd::Last => self.goto(usize::MAX),
                crate::sysmenu::SysCmd::ZoomIn => self.zoom(1.25),
                crate::sysmenu::SysCmd::ZoomOut => self.zoom(1.0 / 1.25),
                crate::sysmenu::SysCmd::ZoomReset => self.zoom_percent(100.0),
                crate::sysmenu::SysCmd::ZoomPercent(p) => self.zoom_percent(p as f32),
                crate::sysmenu::SysCmd::FitWidth => self.toggle_fit_width(),
                crate::sysmenu::SysCmd::Single => self.view_mode = ViewMode::Single,
                crate::sysmenu::SysCmd::Continuous => self.view_mode = ViewMode::Continuous,
                crate::sysmenu::SysCmd::Presentation => self.start_presentation(ctx),
                crate::sysmenu::SysCmd::ToggleThumbs => self.toggle_thumbs(),
            }
        }
    }

    // ---- 演示模式（类 PPT） ----

    fn start_presentation(&mut self, ctx: &egui::Context) {
        if self.tab.is_none() {
            return;
        }
        self.presenting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
    }

    fn exit_presentation(&mut self, ctx: &egui::Context) {
        self.presenting = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }

    /// 演示面板：黑底 + 当前页等比适配全屏 + 页码指示。
    fn presentation_panel(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tab.as_mut() else {
            // 无文档时退出演示
            self.exit_presentation(ui.ctx());
            return;
        };
        let page = tab.page;
        let page_count = tab.doc.page_count;
        let (pw, ph) = tab.doc.page_size_pt(page);
        if pw <= 0.0 || ph <= 0.0 {
            return;
        }

        // 适配屏幕（保持宽高比）
        let avail = ui.available_size();
        let scale = (avail.x / pw).min(avail.y / ph);
        let disp_w = pw * scale;
        let disp_h = ph * scale;

        // 高清渲染缩放：渲染像素 ≈ 显示像素（屏幕倍率由 request_render 统一乘入，
        // 这里只算逻辑比例，不再重复乘 ppp）
        let pres_zoom = (disp_w / pw / scale_px_per_pt(1.0)).clamp(0.1, 4.0);
        // 预渲染当前页与相邻页（翻页时高清图已就绪，避免先模糊）
        let lo = page.saturating_sub(1);
        let hi = (page + 1).min(page_count.saturating_sub(1));
        for np in lo..=hi {
            tab.request_render(np, pres_zoom);
        }

        // 黑底
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);

        // 页面居中绘制：仅显示高清精确命中，不用低清缩略图拉伸（否则会模糊）
        if let Some(tid) = tab.texture_exact(page, pres_zoom) {
            let top_left = egui::pos2(
                rect.center().x - disp_w / 2.0,
                rect.center().y - disp_h / 2.0,
            );
            ui.painter().image(
                tid,
                egui::Rect::from_min_size(top_left, egui::vec2(disp_w, disp_h)),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            // 高清图尚未就绪：显示占位提示（保持黑底，渲染完成即清晰）
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "…",
                FontId::proportional(28.0),
                Color32::from_gray(90),
            );
        }

        // 页码指示（右下角，1 基显示）
        let text = format!("{} / {}", page + 1, page_count);
        ui.painter().text(
            egui::pos2(rect.right() - 20.0, rect.bottom() - 20.0),
            Align2::RIGHT_BOTTOM,
            text,
            FontId::proportional(16.0),
            Color32::from_gray(160),
        );
    }
}

impl eframe::App for SmartPdfApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // 实时缩放期间强制连续重绘：拖动缩放时窗口处于 modal tracking run loop，
        // eframe 的按需重绘会被推迟，ui() 不持续运行就无法按新尺寸渲染，
        // 内容被 Core Animation 拉伸、松手才跳回。LIVE_RESIZE 置位期间每帧重绘，
        // 让 ui() 持续运行并读取最新视口尺寸、按新尺寸渲染，从而平滑跟随。
        if crate::sysmenu::live_resizing() {
            ctx.request_repaint();
        }
        // 视口尺寸变化时也主动重绘一次（兜底）
        if let Some(sz) = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size())) {
            if (self.last_viewport - sz).length() > 0.5 {
                self.last_viewport = sz;
                ctx.request_repaint();
            }
        }
        // 窗口此时已创建，覆盖 winit 建窗时写入的图标
        if !self.dock_icon_done {
            crate::sysmenu::set_dock_icon();
            self.dock_icon_done = true;
        }
        // 窗口已创建：把内容视图延伸到标题栏，内容行与三色按钮同处一行
        if !self.titlebar_done {
            crate::sysmenu::setup_unified_titlebar();
            // 读取红按钮中心，使标题栏内容垂直对齐三色按钮（不同窗口样式高度不同）
            let cy = crate::sysmenu::titlebar_button_center_y();
            self.titlebar_top = (cy - TITLEBAR_H / 2.0).max(0.0);
            self.titlebar_done = true;
        }
        self.titlebar_double_consumed = false;

        // 屏幕倍率同步：多显示器分辨率/倍率不同，换屏时按新倍率渲染
        //（缓存 key 含倍率，旧倍率纹理自然过期淘汰，无需手动清缓存）
        let ppp = ctx.pixels_per_point();
        if let Some(t) = self.tab.as_mut() {
            t.set_pixel_ratio(ppp);
        }

        self.poll(&ctx);
        self.poll_sys_commands(&ctx);
        self.handle_shortcuts(&ctx);

        // 拖放打开：winit 的 DroppedFile 事件只出现在当帧的 raw.dropped_files 里，
        // 支持一次拖入多个文件（单文档模型下依次替换，最后者生效）。
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            self.open_path(&path);
        }

        if self.presenting {
            // 演示模式：全屏单页（类 PPT）
            egui::CentralPanel::default().show(ui, |ui| self.presentation_panel(ui));
            return;
        }

        // ---- 窗口骨架 ----
        // 标题栏 / 侧栏 / 状态栏同用一层「chrome 底色」，与中央正文区拉开层次；
        // 分割线统一在所有面板之后绘制（后画的面板填充不会覆盖它们）。
        let fill = panel_fill(ui);
        let line = hairline(ui);
        let top = self.titlebar_top;
        let panel_resp = egui::Panel::top("titlebar")
            .frame(egui::Frame::NONE.fill(fill).inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: top as i8,
                bottom: 0,
            }))
            .show(ui, |ui| self.titlebar_panel(ui));
        // 双击标题栏空白处放大（egui 无 any_double_click，手动检测 500ms 内两次单击）
        let now = ctx.input(|i| i.time);
        let clicked = ctx.input(|i| i.pointer.any_click());
        let click_pos = ctx.input(|i| i.pointer.interact_pos());
        if clicked {
            if let Some(p) = click_pos {
                if panel_resp.response.rect.contains(p) && !self.titlebar_double_consumed {
                    if now - self.titlebar_dbl_time < 0.5 && self.titlebar_dbl_pos.distance(p) < 6.0 {
                        crate::sysmenu::zoom_key_window();
                        self.titlebar_dbl_time = -1.0;
                    } else {
                        self.titlebar_dbl_time = now;
                        self.titlebar_dbl_pos = p;
                    }
                } else {
                    // 点在按钮上：重置计时，避免与标题栏双击串扰
                    self.titlebar_dbl_time = -1.0;
                }
            }
        }
        // 底部状态栏：同 chrome 底色，恒定高度
        let status_rect = egui::Panel::bottom("status")
            .frame(
                egui::Frame::NONE.fill(fill).inner_margin(egui::Margin {
                    left: 10,
                    right: 10,
                    top: 5,
                    bottom: 5,
                }),
            )
            .show(ui, |ui| self.status_bar(ui))
            .response
            .rect;

        // 左侧缩略图侧栏：展开时可拖宽；收起时完全隐藏（标题栏按钮 / F9 展开）
        let thumbs_rect = if self.show_thumbs {
            Some(
                egui::Panel::left("thumbnails")
                    .resizable(true)
                    .default_size(170.0)
                    .min_size(110.0)
                    .frame(
                        egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(6, 6)),
                    )
                    .show(ui, |ui| self.thumb_panel(ui))
                    .response
                    .rect,
            )
        } else {
            None
        };
        egui::CentralPanel::default().show(ui, |ui| self.doc_panel(ui));

        // 发丝分割线（在所有面板绘制之后画，避免被后画面板的填充覆盖）：
        // 标题栏下缘横线、侧栏右缘竖线（与标题栏横线端点对齐相接）、状态栏上缘横线。
        let tb = panel_resp.response.rect;
        ui.painter().hline(tb.x_range(), tb.bottom(), line);
        if let Some(tr) = thumbs_rect {
            ui.painter().vline(tr.right(), tr.y_range(), line);
        }
        ui.painter()
            .hline(status_rect.x_range(), status_rect.top(), line);
    }
}

/// 加载系统 CJK 字体作为 fallback，保证中文界面正常显示。
fn setup_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let candidates = [
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".into(), Arc::new(FontData::from_owned(bytes)));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            log::info!("已加载系统 CJK 字体：{path}");
            break;
        }
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 适应宽度吸附：接近标准档位时向下取整；差距过大保持精确适配。
    #[test]
    fn fit_zoom_snaps_down_to_steps() {
        assert_eq!(snap_fit_zoom(1.09), 1.0);
        assert_eq!(snap_fit_zoom(1.30), 1.25);
        assert_eq!(snap_fit_zoom(1.62), 1.5);
        assert_eq!(snap_fit_zoom(2.04), 2.0);
        // 0.96 比下方档位（0.75）大太多、又够不到 1.0：保持精确值
        assert_eq!(snap_fit_zoom(0.96), 0.96);
        assert_eq!(snap_fit_zoom(1.9), 1.9);
    }

    /// 手动缩放吸附：±6% 以内就近落档位，否则保持。
    #[test]
    fn nearest_snap_for_manual_zoom() {
        assert_eq!(snap_zoom_nearest(1.28), 1.25);
        assert_eq!(snap_zoom_nearest(0.74), 0.75);
        assert_eq!(snap_zoom_nearest(2.05), 2.0);
        assert_eq!(snap_zoom_nearest(1.13), 1.13);
        assert_eq!(snap_zoom_nearest(0.05), 0.05);
    }

    /// 标题去扩展名（标题栏居中显示文件名用）。
    #[test]
    fn strips_extension_for_title() {
        assert_eq!(strip_extension("demo.pdf"), "demo");
        assert_eq!(strip_extension("v1.2 报告"), "v1.2 报告");
        assert_eq!(strip_extension("无扩展名"), "无扩展名");
    }
}
