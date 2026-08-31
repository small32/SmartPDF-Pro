//! 主界面（egui 纯 Rust 方案）。
//!
//! 布局（对齐 SumatraPDF 经典布局）：
//! - 顶部：菜单栏（文件/视图/前往/缩放，中文）
//! - 左侧：页面缩略图导航（点击跳页）
//! - 中央：整篇文档纵向连续滚动（滚动条拖动浏览全部页面）
//! - 底部：状态栏（页码 / 缩放比例）

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

// ---- 标签条尺寸 ----
/// 与 macOS 标题栏等高，标签才能与左上的红黄绿三色按钮齐平。
const TAB_H: f32 = 28.0;
/// 标题栏三色按钮占据的宽度，标签栏需从此处之后开始。
const TITLEBAR_BUTTONS_W: f32 = 72.0;
/// 标签内文字左右内边距。
const TAB_PAD_X: f32 = 10.0;
/// 标题与 × 之间的间距。
const TAB_TEXT_GAP: f32 = 6.0;
/// 标签内关闭按钮的边长。
const TAB_CLOSE_W: f32 = 15.0;
const TAB_ROUNDING: u8 = 10;
/// 标签之间的水平间距。
const TAB_SPACING: f32 = 4.0;
/// 空间不足时标题可被压缩到的最小宽度。
const TAB_TEXT_MIN_W: f32 = 40.0;
/// 单个标签除标题外的固定宽度（左右内边距 + 标题与 × 的间距 + × 按钮）。
const TAB_FIXED_W: f32 = TAB_PAD_X * 2.0 + TAB_TEXT_GAP + TAB_CLOSE_W;

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

/// 标签上发生的动作。
enum TabAction {
    None,
    Activate,
    Close,
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
            // 同一行内中英文混排时，不同字体的字形默认按上/下边界对齐，
            // 会出现扩展名".pdf"比中文文件名高一截/低一截；按中线对齐可解决。
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    // 强制单行：若允许换行，文件名与扩展名会被折成上下两行（一高一低）
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    if max_width.is_finite() {
        job.wrap.overflow_character = Some('…');
    }
    job
}

/// 为各标签分配标题宽度：放得下就完整显示；放不下只压缩真正超宽的标签。
///
/// 做法是反复把剩余预算平分给尚未定下的标签，宽度够用的先按完整宽度固定
/// 并从预算中扣除，剩下的继续平分，直到全部定下（water-filling）。
fn fit_title_widths(ui: &egui::Ui, titles: &[String], avail: f32) -> Vec<f32> {
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let mut widths: Vec<f32> = titles
        .iter()
        .map(|t| {
            ui.painter()
                .layout_job(title_job(&font_id, egui::Color32::PLACEHOLDER, t, f32::INFINITY))
                .size()
                .x
        })
        .collect();

    let budget = avail
        - widths.len() as f32 * TAB_FIXED_W
        - widths.len().saturating_sub(1) as f32 * TAB_SPACING;
    if widths.iter().sum::<f32>() <= budget {
        return widths; // 横向放得下：完整文件名
    }

    let mut budget = budget;
    let mut pending: Vec<usize> = (0..widths.len()).collect();
    while !pending.is_empty() {
        let share = budget / pending.len() as f32;
        let fits: Vec<usize> = pending
            .iter()
            .copied()
            .filter(|&i| widths[i] <= share)
            .collect();
        if fits.is_empty() {
            // 剩下的都放不下：统一压到当前份额（不低于下限）
            for i in &pending {
                widths[*i] = share.max(TAB_TEXT_MIN_W);
            }
            break;
        }
        for i in &fits {
            budget -= widths[*i];
        }
        pending.retain(|i| !fits.contains(i));
    }
    widths
}

/// 一体化标签：标题与 × 关闭按钮画在同一个控件里，共用底色与中心线。
/// `max_text_w` 是该标题可用的最大宽度（由 [`fit_title_widths`] 分配）。
fn tab_item(ui: &mut egui::Ui, title: &str, is_active: bool, max_text_w: f32) -> TabAction {
    // 先把需要的颜色拷出来（Color32 是 Copy），避免后续 &mut ui 时借用冲突
    let (text_color, strong_color) = {
        let v = ui.visuals();
        (v.text_color(), v.strong_text_color())
    };
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let text_color = if is_active { strong_color } else { text_color };

    let galley = ui
        .painter()
        .layout_job(title_job(&font_id, text_color, title, max_text_w));

    let size = egui::vec2(TAB_FIXED_W + galley.size().x, TAB_H);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());

    // 仿 Chrome 标签：柔和半透明底色（仅在 hover/激活时浮现），圆角更柔，线条更轻
    let dark = ui.visuals().dark_mode;
    let hover_fill = if dark {
        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 12)
    };
    let active_fill = if dark {
        Color32::from_rgba_unmultiplied(255, 255, 255, 40)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 24)
    };
    let fill = if is_active {
        Some(active_fill)
    } else if resp.hovered() {
        Some(hover_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(TAB_ROUNDING), fill);
    }

    // 标题与 × 共用同一条垂直中心线（按字形包围盒居中，避开行距造成的偏移）
    let center_y = rect.center().y;
    ui.painter().galley(
        egui::pos2(
            rect.left() + TAB_PAD_X,
            center_y - galley.mesh_bounds.center().y,
        ),
        galley,
        text_color,
    );

    // 关闭按钮：仿 Chrome 的小圆点，hover 时显示淡灰圆底，平时只显示一个低调的 ×
    let close_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - TAB_PAD_X - TAB_CLOSE_W / 2.0, center_y),
        egui::vec2(TAB_CLOSE_W, TAB_CLOSE_W),
    );
    let close_resp = ui.allocate_rect(close_rect, egui::Sense::click());
    if close_resp.hovered() {
        let close_hover = if dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 46)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 28)
        };
        ui.painter()
            .circle_filled(close_rect.center(), TAB_CLOSE_W / 2.0, close_hover);
    }
    let x_color = if close_resp.hovered() {
        strong_color
    } else {
        text_color.gamma_multiply(0.7)
    };
    ui.painter().text(
        close_rect.center(),
        Align2::CENTER_CENTER,
        "×",
        font_id,
        x_color,
    );

    if close_resp.clicked() || resp.double_clicked() {
        TabAction::Close
    } else if resp.clicked() {
        TabAction::Activate
    } else {
        TabAction::None
    }
}

pub struct SmartPdfApp {
    tabs: Vec<DocTab>,
    active: Option<usize>,
    view_mode: ViewMode,
    status: String,
    /// 连续模式下需要滚动到的页（翻页/缩略图跳页后置位，下一帧应用）。
    scroll_target: Option<usize>,
    /// 是否处于演示模式（类 PPT 全屏单页）。
    presenting: bool,
    /// Dock 图标是否已在首帧设置（窗口创建后才能覆盖 winit 写入的默认图标）。
    dock_icon_done: bool,
    /// 统一标题栏是否已在首帧设置（内容延伸到标题栏，标签栏与三色按钮同行）。
    titlebar_done: bool,
    /// 标签栏相对窗口顶部的上边距：使标签垂直中心对齐三色按钮中心（运行时测得）。
    titlebar_tab_top: f32,
    /// 本帧双击是否落在标签/按钮上（用于抑制"双击标题栏放大"，避免误关标签）。
    titlebar_double_consumed: bool,
    /// 手动检测标题栏双击：上次单击的时间与位置。
    titlebar_dbl_time: f64,
    titlebar_dbl_pos: egui::Pos2,
    /// 上一帧视口尺寸：用于检测窗口缩放/拖拽，主动触发重绘避免标签栏滞后。
    last_viewport: egui::Vec2,
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
            tabs: Vec::new(),
            active: None,
            view_mode: ViewMode::Continuous,
            status: String::new(),
            scroll_target: None,
            presenting: false,
            dock_icon_done: false,
            titlebar_done: false,
            titlebar_tab_top: 0.0,
            titlebar_double_consumed: false,
            titlebar_dbl_time: -1.0,
            titlebar_dbl_pos: egui::Pos2::ZERO,
            last_viewport: egui::Vec2::ZERO,
        };
        for f in files {
            app.open_path(&f);
        }
        app
    }

    fn active_idx(&self) -> Option<usize> {
        self.active.filter(|i| *i < self.tabs.len())
    }

    // ---- 打开 / 关闭 ----

    fn open_path(&mut self, path: &Path) {
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = Some(i);
            self.status = format!("已在标签页中打开：{}", path.display());
            return;
        }
        match DocTab::open(path) {
            Ok(tab) => {
                let title = tab.title.clone();
                self.tabs.push(tab);
                self.active = Some(self.tabs.len() - 1);
                self.status = format!("已打开「{}」", title);
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

    fn close_active(&mut self) {
        let Some(i) = self.active_idx() else {
            return;
        };
        self.close_tab(i);
    }

    /// 关闭指定索引的标签页（参照 SumatraPDF 的 WindowTab）。
    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let removed = self.tabs.remove(idx).title.clone();
        self.status = format!("已关闭「{}」", removed);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(idx.min(self.tabs.len() - 1))
        };
        self.scroll_target = self.active.and_then(|i| Some(self.tabs.get(i)?.page));
    }

    // ---- 顶部标签栏 ----

    fn tabs_panel(&mut self, ui: &mut egui::Ui) {
        // 强制标签栏行高恒定为 TAB_H：有文档时行高由 tab_item 撑满为 28，
        // 但无文档时面板里仅剩「＋」按钮，行高会退化为按钮自然高度（更矮），
        // 导致整条栏相对三色按钮中心上偏错位。统一行高可让两者始终垂直对齐。
        ui.style_mut().spacing.interact_size.y = TAB_H;
        // 先快照标签信息，避免闭包内对 self 的借用冲突；标签只显示文件名不显示扩展名
        let items: Vec<(usize, String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (i, strip_extension(&t.title), self.active == Some(i)))
            .collect();
        let titles: Vec<String> = items.iter().map(|(_, t, _)| t.clone()).collect();

        let mut activate: Option<usize> = None;
        let mut close: Option<usize> = None;

        // 垂直居中的横排标签栏：每个标签自带关闭按钮
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            // 空出左上角红黄绿三色按钮的位置（标签栏延伸到标题栏后需避开）
            ui.add_space(TITLEBAR_BUTTONS_W);
            ui.spacing_mut().item_spacing.x = TAB_SPACING;
            // 仿 Chrome 的新标签圆形按钮：固定正方形画成圆形，平时透明、hover 浮现淡灰圆底
            let plus_font = egui::TextStyle::Button.resolve(ui.style());
            let plus_text = ui.painter().layout_job(title_job(
                &plus_font,
                ui.visuals().text_color(),
                "＋",
                f32::INFINITY,
            ));
            let (plus_rect, plus_resp) =
                ui.allocate_exact_size(egui::vec2(TAB_H, TAB_H), egui::Sense::click());
            if plus_resp.hovered() {
                let dark = ui.visuals().dark_mode;
                let plus_hover = if dark {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 46)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 28)
                };
                ui.painter()
                    .circle_filled(plus_rect.center(), TAB_H / 2.0, plus_hover);
            }
            // 用字形包围盒居中（与标签页同法），避免全角「＋」度量偏高导致上偏
            ui.painter().galley(
                egui::pos2(
                    plus_rect.center().x - plus_text.mesh_bounds.center().x,
                    plus_rect.center().y - plus_text.mesh_bounds.center().y,
                ),
                plus_text,
                ui.visuals().text_color(),
            );
            if plus_resp.on_hover_text("打开文档（⌘O）").clicked() {
                self.titlebar_double_consumed = true;
                self.pick_and_open();
            }
        // 有文档才显示分隔符与标签
        if items.is_empty() {
            return;
        }
        ui.separator();
            // ＋ 与分隔符之后剩下的宽度才是标签可用宽度
            let widths = fit_title_widths(ui, &titles, ui.available_width());
            for ((idx, title, is_active), max_text_w) in items.into_iter().zip(widths) {
                match tab_item(ui, &title, is_active, max_text_w) {
                    TabAction::Close => {
                        // 双击标签关闭：标记 consumed 以免触发"双击标题栏放大"
                        self.titlebar_double_consumed = true;
                        close = Some(idx);
                    }
                    TabAction::Activate => activate = Some(idx),
                    TabAction::None => {}
                }
            }
        });

        if let Some(idx) = activate {
            if self.active != Some(idx) {
                self.active = Some(idx);
                if let Some(p) = self.tabs.get(idx) {
                    self.scroll_target = Some(p.page);
                }
            }
        }
        if let Some(idx) = close {
            self.close_tab(idx);
        }
    }

    // ---- 页面导航 / 缩放 ----

    fn goto(&mut self, page: usize) {
        let Some(i) = self.active_idx() else {
            return;
        };
        let clamped = page.min(self.tabs[i].doc.page_count.saturating_sub(1));
        if self.tabs[i].page != clamped {
            self.tabs[i].goto(clamped);
            self.scroll_target = Some(clamped);
        }
    }

    fn navigate(&mut self, delta: isize) {
        let Some(i) = self.active_idx() else {
            return;
        };
        let target = (self.tabs[i].page as isize + delta).max(0) as usize;
        self.goto(target);
    }

    fn zoom(&mut self, factor: f32) {
        let Some(i) = self.active_idx() else {
            return;
        };
        self.tabs[i].zoom = (self.tabs[i].zoom * factor).clamp(0.05, 8.0);
        self.tabs[i].fit_width = false;
    }

    fn zoom_percent(&mut self, pct: f32) {
        let Some(i) = self.active_idx() else {
            return;
        };
        self.tabs[i].zoom = (pct / 100.0).clamp(0.05, 8.0);
        self.tabs[i].fit_width = false;
    }

    fn toggle_fit_width(&mut self) {
        let Some(i) = self.active_idx() else {
            return;
        };
        self.tabs[i].fit_width = !self.tabs[i].fit_width;
    }

    // ---- 渲染收集 ----

    fn poll(&mut self, ctx: &egui::Context) {
        let mut any = false;
        for tab in &mut self.tabs {
            any |= tab.poll_render(ctx);
        }
        if any {
            ctx.request_repaint();
        }
    }


    // ---- 左侧缩略图面板 ----

    fn thumb_panel(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active_idx() else {
            // 未打开文档时保持左侧面板为空
            return;
        };
        let page_count = self.tabs[i].doc.page_count;

        // 平均缩略图高度（作为 show_rows 行高，只渲染可见缩略图）
        let mut sum_h = 0.0f32;
        for p in 0..page_count {
            let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(p);
            sum_h += THUMB_WIDTH * h_pt / w_pt.max(0.01);
        }
        let row_h = sum_h / page_count.max(1) as f32 + 6.0;

        ScrollArea::vertical()
            .id_salt("thumbs")
            .show_rows(ui, row_h, page_count, |ui, range| {
                for p in range {
                    let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(p);
                    let h = THUMB_WIDTH * h_pt / w_pt.max(0.01);
                    // 只对可见缩略图发起渲染请求
                    self.tabs[i].request_render(p, THUMB_ZOOM);
                    let panel_w = ui.available_width();
                    let resp = ui
                        .allocate_ui_with_layout(
                            Vec2::new(panel_w, h),
                            Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                if let Some(tid) = self.tabs[i].display_texture_at(p, THUMB_ZOOM) {
                                    ui.image(SizedTexture::new(tid, Vec2::new(THUMB_WIDTH, h)));
                                } else {
                                    ui.allocate_space(Vec2::new(THUMB_WIDTH, h));
                                }
                            },
                        )
                        .response;
                    if resp.clicked() {
                        self.tabs[i].goto(p);
                        self.scroll_target = Some(p);
                    }
                    ui.add_space(6.0);
                }
            });
    }

    // ---- 中央文档面板 ----

    fn doc_panel(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active_idx() else {
            // 未打开文档时保持中央区域为空
            return;
        };
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let avail_w = ui.available_width();

        // 适应宽度：只在窗口宽度真实变化（>4px）时重算缩放。
        // 若每帧用 available_width 重算，垂直滚动条出现/消失会让宽度微变 → zoom 振荡、
        // 页面反复以不同缩放渲染（闪烁）。用 viewport 宽度做判据一次性更新。
        {
            let tab = &mut self.tabs[i];
            if tab.fit_width {
                let vw = ui
                    .ctx()
                    .input(|i| i.viewport().inner_rect.map(|r| r.width()).unwrap_or(0.0));
                if (vw - tab.fit_width_viewport).abs() > 4.0 {
                    let (pw, _) = tab.page_size_pt();
                    tab.zoom = (avail_w / (pw * scale_px_per_pt(1.0))).clamp(0.05, 8.0);
                    tab.fit_width_viewport = vw;
                }
            }
        }

        let zoom = self.tabs[i].zoom;
        let page_count = self.tabs[i].doc.page_count;
        let px_per_pt = scale_px_per_pt(zoom);
        let gap = PAGE_GAP;

        match self.view_mode {
            ViewMode::Single => {
                let page = self.tabs[i].page;
                self.tabs[i].request_render(page, zoom);
                ui.centered_and_justified(|ui| {
                    if let Some(tid) = self.tabs[i].display_texture_for(page) {
                        let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(page);
                        let w = w_pt * px_per_pt / ppp;
                        let h = h_pt * px_per_pt / ppp;
                        ui.image(SizedTexture::new(tid, Vec2::new(w, h)));
                    }
                });
            }
            ViewMode::Continuous => {
                // 平均页高（作为 show_rows 的行高，近似可见区）
                let mut sum_h = 0.0f32;
                for p in 0..page_count {
                    sum_h += self.tabs[i].doc.page_size_pt(p).1;
                }
                let avg_h = sum_h / page_count.max(1) as f32 * px_per_pt / ppp;
                let row_h = avg_h + gap;

                let mut scroll = ScrollArea::vertical().id_salt("doc").auto_shrink([false, false]);
                if let Some(p) = self.scroll_target.take() {
                    scroll = scroll.vertical_scroll_offset(p as f32 * row_h);
                }

                scroll.show_rows(ui, row_h, page_count, |ui, range| {
                    for p in range {
                        let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(p);
                        let w = w_pt * px_per_pt / ppp;
                        let h = h_pt * px_per_pt / ppp;
                        // 请求渲染（未缓存/未在途才发送）
                        self.tabs[i].request_render(p, zoom);
                        ui.allocate_ui_with_layout(
                            Vec2::new(avail_w, h),
                            Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                if let Some(tid) = self.tabs[i].display_texture_for(p) {
                                    ui.image(SizedTexture::new(tid, Vec2::new(w, h)));
                                } else {
                                    ui.allocate_space(Vec2::new(w, h));
                                }
                            },
                        );
                        ui.add_space(gap);
                    }
                });
            }
        }
    }

    // ---- 底部状态栏 ----

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active_idx() else {
            return;
        };
        let page = self.tabs[i].page;
        let count = self.tabs[i].doc.page_count;
        let zoom_pct = (self.tabs[i].zoom * 100.0).round() as i32;
        ui.horizontal(|ui| {
            ui.label(format!("页 {}/{}", page + 1, count));
            ui.separator();
            ui.label(format!("缩放 {zoom_pct}%"));
            if !self.status.is_empty() {
                ui.separator();
                ui.label(&self.status);
            }
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
        // 进入演示：⌘P
        if ctx.input_mut(|i| i.consume_key(cmd, Key::P)) {
            self.start_presentation(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::O)) {
            self.pick_and_open();
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::W)) {
            self.close_active();
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
                crate::sysmenu::SysCmd::Close => self.close_active(),
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
            }
        }
    }

    // ---- 演示模式（类 PPT） ----

    fn start_presentation(&mut self, ctx: &egui::Context) {
        if self.active_idx().is_none() {
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
        let Some(i) = self.active_idx() else {
            // 无文档时退出演示
            self.exit_presentation(ui.ctx());
            return;
        };
        let page = self.tabs[i].page;
        let page_count = self.tabs[i].doc.page_count;
        let (pw, ph) = self.tabs[i].doc.page_size_pt(page);
        if pw <= 0.0 || ph <= 0.0 {
            return;
        }

        // 适配屏幕（保持宽高比）
        let avail = ui.available_size();
        let scale = (avail.x / pw).min(avail.y / ph);
        let disp_w = pw * scale;
        let disp_h = ph * scale;

        // 高清渲染缩放：渲染像素 ≈ 显示像素
        let ppp = ui.ctx().pixels_per_point();
        let pres_zoom = ((disp_w * ppp) / pw / scale_px_per_pt(1.0)).clamp(0.1, 4.0);
        // 预渲染当前页与相邻页（翻页时高清图已就绪，避免先模糊）
        let lo = page.saturating_sub(1);
        let hi = (page + 1).min(page_count.saturating_sub(1));
        for np in lo..=hi {
            self.tabs[i].request_render(np, pres_zoom);
        }

        // 黑底
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);

        // 页面居中绘制：仅显示高清精确命中，不用低清缩略图拉伸（否则会模糊）
        if let Some(tid) = self.tabs[i].texture_exact(page, pres_zoom) {
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

        // 页码指示（右下角）
        let text = format!("{page} / {}", page_count + 1);
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
        // 窗口已创建：把内容视图延伸到标题栏，标签栏与三色按钮同处一行
        if !self.titlebar_done {
            crate::sysmenu::setup_unified_titlebar();
            // 读取红按钮中心，使标签栏垂直对齐三色按钮（不同窗口样式高度不同）
            let cy = crate::sysmenu::titlebar_button_center_y();
            self.titlebar_tab_top = (cy - TAB_H / 2.0).max(0.0);
            self.titlebar_done = true;
        }
        self.titlebar_double_consumed = false;
        self.poll(&ctx);
        self.poll_sys_commands(&ctx);
        self.handle_shortcuts(&ctx);

        if self.presenting {
            // 演示模式：全屏单页（类 PPT）
            egui::CentralPanel::default().show(ui, |ui| self.presentation_panel(ui));
            return;
        }

        // 窗口内不再放菜单栏：菜单在 macOS 系统菜单栏（见 sysmenu.rs）
        // 标签栏充当标题栏：左/右留 8px，顶部留白使标签垂直对齐三色按钮中心
        let titlebar_tab_top = self.titlebar_tab_top;
        let panel_resp = egui::Panel::top("tabs")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: titlebar_tab_top as i8,
                bottom: 0,
            }))
            .show(ui, |ui| self.tabs_panel(ui));
        // 双击标题栏空白处放大（egui 无 any_double_click，这里手动检测 500ms 内两次单击）
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
                    // 点在标签/按钮上：重置计时，避免与标题栏双击串扰
                    self.titlebar_dbl_time = -1.0;
                }
            }
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("thumbnails")
            .resizable(true)
            .default_size(170.0)
            .min_size(110.0)
            .show(ui, |ui| self.thumb_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.doc_panel(ui));
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