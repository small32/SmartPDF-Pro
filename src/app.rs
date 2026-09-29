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
use eframe::egui::containers::scroll_area::ScrollSource;
// CoreText：枚举系统字体供「设置」选择；ab_glyph 校验所选字体可渲染
use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle};
use objc2_core_text::{
    CTFont, CTFontManagerCopyAvailableFontFamilyNames, CTFontManagerCopyAvailableFontURLs,
    kCTFontURLAttribute,
};

use crate::document::scale_px_per_pt;
use crate::tab::DocTab;

/// 页面浏览模式。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Single,
    Continuous,
}

const PAGE_GAP: f32 = 18.0;
/// 中央文档页面的默认最大显示宽度，与 160px 缩略图保持 5 倍比例。
const PAGE_MAX_WIDTH: f32 = 800.0;

fn cumulative_page_offsets(heights: impl IntoIterator<Item = f32>) -> Vec<f32> {
    let mut offsets = vec![0.0];
    for height in heights {
        offsets.push(offsets.last().copied().unwrap_or_default() + height.max(0.0));
    }
    offsets
}

fn page_at_offset(offsets: &[f32], y: f32) -> Option<usize> {
    let page_count = offsets.len().checked_sub(1)?;
    if page_count == 0 {
        return None;
    }
    Some(
        offsets
            .partition_point(|offset| *offset <= y.max(0.0))
            .saturating_sub(1)
            .min(page_count - 1),
    )
}

/// 计算需要布置和渲染的页面范围，并在视口上下各预取一页。
fn visible_page_range(offsets: &[f32], viewport: egui::Rect) -> std::ops::Range<usize> {
    let page_count = offsets.len().saturating_sub(1);
    let Some(first_visible) = page_at_offset(offsets, viewport.top()) else {
        return 0..0;
    };
    let last_visible = page_at_offset(offsets, viewport.bottom()).unwrap_or(first_visible);
    first_visible.saturating_sub(1)..(last_visible + 2).min(page_count)
}

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
    /// 状态栏消息的设置时刻（5 秒后自动清除，避免旧消息常驻）。
    status_at: std::time::Instant,
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
    /// 缩略图栏上次计算适应宽度时的面板宽度（用于避免每帧振荡重算）。
    thumb_fit_width: f32,
    /// 连续模式主文档区的实时滚动偏移（文档内容坐标系，像素），用于与缩略图栏双向同步。
    doc_scroll_y: f32,
    /// 连续模式主文档内容总高度（像素），供缩略图栏换算跟随比例 k = 缩略图总高 / 此值。
    doc_total_height: f32,
    /// 一次性强制主文档区滚动到指定偏移（由悬停缩略图栏滚轮触发）；下一帧由 doc_panel
    /// 消费并清空。与 scroll_target 跳页同通道，避免每帧覆盖用户在主区的自由滚动。
    doc_scroll_force: Option<f32>,
    /// 「设置」窗口是否打开。
    show_settings: bool,
    /// 用户选择的界面字体文件路径（None = 系统默认）。
    ui_font: Option<String>,
    /// 字体设置窗口中「待确认」的选择（跨帧保留）：
    /// Some(Some(path)) = 选了某字体；Some(None) = 选了系统默认；None = 尚未点选。
    settings_pending: Option<Option<String>>,
    /// 系统字体枚举缓存（首次打开「字体设置」时惰性填充）：
    /// CoreText 全量枚举开销大，不能在窗口打开期间每帧重算。
    font_list: Option<Vec<(String, String)>>,
}

impl SmartPdfApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        // 命令通道与 Finder openURLs handler 已在 main() 中、进入 AppKit
        // 事件循环前初始化，避免冷启动的打开文档事件先到而被丢弃。
        let saved_font = load_font_config();
        setup_cjk_fonts(&cc.egui_ctx, saved_font.as_deref());
        // 注册 egui 上下文，供原生 resize 回调在实时缩放时强制重绘
        crate::sysmenu::set_context(cc.egui_ctx.clone());
        crate::sysmenu::install();
        let mut app = Self {
            tabs: Vec::new(),
            active: None,
            view_mode: ViewMode::Continuous,
            status: String::new(),
            status_at: std::time::Instant::now(),
            scroll_target: None,
            presenting: false,
            dock_icon_done: false,
            titlebar_done: false,
            titlebar_tab_top: 0.0,
            titlebar_double_consumed: false,
            titlebar_dbl_time: -1.0,
            titlebar_dbl_pos: egui::Pos2::ZERO,
            last_viewport: egui::Vec2::ZERO,
            thumb_fit_width: 0.0,
            doc_scroll_y: 0.0,
            doc_total_height: 0.0,
            doc_scroll_force: None,
            show_settings: false,
            ui_font: saved_font,
            settings_pending: None,
            font_list: None,
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

    /// 设置状态栏消息并记录时间（到时自动清除）。
    fn set_status(&mut self, msg: String) {
        self.status = msg;
        self.status_at = std::time::Instant::now();
    }

    fn open_path(&mut self, path: &Path) {
        // 规范化路径：符号链接/相对路径指向同一文件时不重复开标签
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path = canonical.as_path();
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = Some(i);
            self.set_status(format!("已在标签页中打开：{}", path.display()));
            return;
        }
        match DocTab::open(path) {
            Ok(tab) => {
                let title = tab.title.clone();
                self.tabs.push(tab);
                self.active = Some(self.tabs.len() - 1);
                // 新文档从顶部开始：清空主区滚动位置，缩略图栏随之归零。
                self.doc_scroll_y = 0.0;
                self.doc_total_height = 0.0;
                self.doc_scroll_force = None;
                self.set_status(format!("已打开「{}」", title));
            }
            Err(e) => self.set_status(format!("无法打开 {}：{}", path.display(), e)),
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
        self.set_status(format!("已关闭「{}」", removed));
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(idx.min(self.tabs.len() - 1))
        };
        self.scroll_target = self.active.and_then(|i| Some(self.tabs.get(i)?.page));
    }

    // ---- 顶部标签栏 ----

    fn tabs_panel(&mut self, ui: &mut egui::Ui) {
        // 标题栏区域（整条标签栏）支持拖动窗口：空白处按住拖动 → 触发原生窗口拖动。
        // 由于子组件（标签/＋按钮）绘制在交互层之上会优先响应，因此只有空白区域
        // 的拖动才会落到这里，设置窗口、滚动条、标签、按钮的拖动互不干扰。
        // 原实现用 NSWindow.setMovableByWindowBackground(true) 让整窗可拖动，
        // 导致拖动设置窗口/滚动条时整个窗体跟着移动，已移除（见 sysmenu.rs）。
        let drag_resp = ui.interact(
            ui.max_rect(),
            ui.id().with("titlebar_drag"),
            egui::Sense::drag(),
        );
        if drag_resp.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

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
            // 标签过多放不下时：优先保证活动标签可见，从它向两侧按宽度交替扩展，
            // 放不下的标签隐藏（不渲染），避免压缩后总宽仍超出窗口被裁掉。
            let n = items.len();
            let active_pos = items
                .iter()
                .position(|(idx, _, _)| Some(*idx) == self.active)
                .unwrap_or(n.saturating_sub(1));
            let mut visible = vec![false; n];
            visible[active_pos] = true;
            let mut used = TAB_FIXED_W + widths[active_pos];
            let mut lo = active_pos;
            let mut hi = active_pos + 1;
            while lo > 0 || hi < n {
                let next_left = if lo > 0 { lo - 1 } else { usize::MAX };
                let next_right = if hi < n { hi } else { usize::MAX };
                // 两侧候选中先放较宽的一个（更可能放不下，尽早决策）
                let take = if next_left != usize::MAX
                    && (next_right == usize::MAX || widths[next_left] >= widths[next_right])
                {
                    next_left
                } else {
                    next_right
                };
                if take == usize::MAX {
                    break;
                }
                let w = TAB_FIXED_W + widths[take];
                if used + w + TAB_SPACING > ui.available_width() {
                    break;
                }
                used += w + TAB_SPACING;
                visible[take] = true;
                if take < active_pos {
                    lo = take;
                } else {
                    hi = take + 1;
                }
            }
            let hidden = n - visible.iter().filter(|v| **v).count();
            for (((idx, title, is_active), max_text_w), show) in
                items.into_iter().zip(widths).zip(visible)
            {
                if !show {
                    continue;
                }
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
            if hidden > 0 {
                ui.label(format!("+{hidden}"));
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

    /// 渲染线程仍有请求在途时，安排下一帧重绘。
    ///
    /// 必须在本帧面板渲染之后调用：本帧新发出的请求（例如点选缩略图后跳页的目标页）
    /// 也要纳入判断。否则点击后 eframe 立刻回到空闲，渲染线程虽然出了图，却没有下一帧
    /// 把它画出来——只能等下一次输入事件（鼠标移动）才刷新，表现为跳转「不立即」。
    fn keep_repainting_while_rendering(&self, ctx: &egui::Context) {
        if self.tabs.iter().any(|tab| tab.has_pending()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }


    // ---- 左侧缩略图面板 ----

    fn thumb_panel(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active_idx() else {
            // 未打开文档时保持左侧面板为空
            return;
        };
        let page_count = self.tabs[i].doc.page_count;
        let ppp = ui.ctx().pixels_per_point();
        let max_texture_side = ui.ctx().input(|input| input.max_texture_side);

        // 缩略图宽度封顶：最大 160px（即使栏再宽也不继续放大，居中显示）。
        let cap = |w: f32| w.clamp(20.0, 160.0);

        let live_w = cap(ui.available_width());
        if (live_w - self.thumb_fit_width).abs() > 4.0 {
            self.thumb_fit_width = live_w;
        }
        let zoom_base = self.thumb_fit_width.max(20.0);
        let offsets = cumulative_page_offsets((0..page_count).map(|p| {
            let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(p);
            live_w * h_pt / w_pt.max(0.01) + 6.0
        }));
        let total_height = offsets.last().copied().unwrap_or_default();
        // 跟随比例 k：缩略图总高 / 主文档总高。两栏内容均为「各页累计高度」，比例近似恒定，
        // 故主区滚动 offset 乘以 k 即缩略图栏应处的位置。
        let k = if self.doc_total_height > 0.0 {
            total_height / self.doc_total_height
        } else {
            0.0
        };
        // 纯跟随：缩略图栏是主文档滚动位置的镜像。doc_scroll_y 每帧由 doc_panel 回灌为主区真实偏移，
        // 这里按 k = 缩略图总高 / 主文档总高 比例同步缩略图栏滚动位置。缩略图栏不回写 doc_scroll_y，
        // 因此不存在反馈环路，主文档区始终可自由滚动到任意页。点击缩略图跳页走 scroll_target（主区一次性跳页）。
        let want = self.doc_scroll_y * k;

        // 滚轮驱动主文档：指针悬停缩略图栏时滚轮不应被本栏吞掉，而是换算为主文档滚动。
        // egui 滚轮语义：delta 正 = 内容向上滚（offset 减小），负 = 向下。按 k 的倒数换算到
        // 主文档坐标系。本栏自身禁用 mouse_wheel（SCROLL_BAR），避免同一份 delta 被两个
        // ScrollArea 消费。主文档上限由 ScrollArea 内部自行夹取，这里只需保证非负。
        // 写入 doc_scroll_y 后，doc_panel 本帧回灌的是 viewport.min.y（旧值），会覆盖——
        // 因此用 doc_scroll_force 一次性强制主区到新位置（与跳页同通道），下帧回灌接续。
        let wheel_y = ui.ctx().input(|i| i.smooth_scroll_delta().y);
        if wheel_y != 0.0 && ui.rect_contains_pointer(ui.max_rect()) && k > 0.0 {
            let new_doc = (self.doc_scroll_y - wheel_y / k).max(0.0);
            if (new_doc - self.doc_scroll_y).abs() > f32::EPSILON {
                self.doc_scroll_y = new_doc;
                self.doc_scroll_force = Some(new_doc);
            }
        }

        let scroll = ScrollArea::vertical()
            .id_salt("thumbs")
            .scroll_source(ScrollSource::SCROLL_BAR)
            .vertical_scroll_offset(want);
        scroll.show_viewport(ui, |ui, viewport| {
            ui.set_height(total_height);
            let content_top = ui.max_rect().top();
            for p in visible_page_range(&offsets, viewport) {
                let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(p);
                let h = live_w * h_pt / w_pt.max(0.01);
                // 自适应缩放：渲染缩放使页面宽度铺满 zoom_base（egui 点），含 ppp 修正
                let zoom =
                    (zoom_base * ppp / (w_pt * scale_px_per_pt(1.0))).clamp(0.05, 8.0);
                // 只对可见缩略图发起渲染请求（低优先级：缩略图不与正文抢渲染队列）
                self.tabs[i].request_render(p, zoom, max_texture_side, false);
                // 按真实累计高度布置，避免不同尺寸页面导致虚拟滚动偏移。
                let row_w = ui.available_width().max(20.0);
                let row_rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left(), content_top + offsets[p]),
                    Vec2::new(row_w, h),
                );
                let resp = ui
                    .scope_builder(
                        egui::UiBuilder::new()
                            .id_salt(("thumb", p))
                            .max_rect(row_rect)
                            // 必须显式声明 click 感知：`UiBuilder` 默认 sense 是
                            // `Sense::hover()`（即空感知），此时 scope_builder 返回的
                            // `.response.clicked()` 恒为 false——点击永远不会被注册。
                            .sense(egui::Sense::click())
                            .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
                        |ui| {
                            if let Some(tid) = self.tabs[i].display_texture_at(p, zoom) {
                                ui.image(SizedTexture::new(tid, Vec2::new(live_w, h)));
                            } else {
                                ui.allocate_space(Vec2::new(live_w, h));
                            }
                        },
                    )
                    .response;
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    // 点选缩略图：goto 直接改写当前页，scroll_target 让 doc_panel 在本帧
                    // 一次性把主区强制到该页偏移（不等任何动画/惯性），再补一次重绘，
                    // 保证跳转与目标页出图都不必等下一次输入事件。
                    self.tabs[i].goto(p);
                    self.scroll_target = Some(p);
                    ui.ctx().request_repaint();
                }
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
        let max_texture_side = ctx.input(|input| input.max_texture_side);
        let avail_w = ui.available_width();

        // 适应宽度：以「文档区域宽度」avail_w 为基准（CentralPanel 已排除左侧缩略图栏），
        // 页面默认宽度封顶为 800px；窗口不足时自动缩小，高度按页面比例计算。
        // 只在宽度真实变化（>4px）时重算缩放。若每帧重算，垂直滚动条出现/消失会让宽度微变
        // → zoom 振荡、页面反复以不同缩放渲染（闪烁）。用 avail_w 做判据一次性更新，
        // 同时也能响应缩略图栏宽度变化。
        //
        // 缩放换算：显示宽度 = pw * scale_px_per_pt(zoom) / ppp（egui 点），
        // 要填满 avail_w 需 zoom = avail_w * ppp / (pw * scale_px_per_pt(1.0))。
        {
            let tab = &mut self.tabs[i];
            if tab.fit_width {
                if (avail_w - tab.fit_width_viewport).abs() > 4.0 {
                    let (pw, _) = tab.page_size_pt();
                    let page_width = avail_w.min(PAGE_MAX_WIDTH);
                    tab.zoom =
                        (page_width * ppp / (pw * scale_px_per_pt(1.0))).clamp(0.05, 8.0);
                    tab.fit_width_viewport = avail_w;
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
                // 单页模式没有连续滚动的偏移量，跳页由 goto() 直接生效；清掉可能残留的
                // scroll_target，避免之后切回连续模式时凭空跳到旧目标页。
                self.scroll_target = None;
                // 单页模式当前页即视线中心，优先渲染。
                self.tabs[i].request_render(page, zoom, max_texture_side, true);
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
                let offsets = cumulative_page_offsets((0..page_count).map(|p| {
                    self.tabs[i].doc.page_size_pt(p).1 * px_per_pt / ppp + gap
                }));
                let total_height = offsets.last().copied().unwrap_or_default();
                self.doc_total_height = total_height;

                let mut scroll = ScrollArea::vertical().id_salt("doc").auto_shrink([false, false]);
                // 跳页（缩略图点击/翻页/标签切换）：仅在此一次性强制主区到目标页，其余帧主区完全自由滚动。
                if let Some(p) = self.scroll_target.take() {
                    if let Some(offset) = offsets.get(p) {
                        self.doc_scroll_y = *offset;
                        scroll = scroll.vertical_scroll_offset(self.doc_scroll_y);
                    }
                }
                // 悬停缩略图栏滚轮：一次性强制主区到换算后的新位置，消费即清空。
                if let Some(f) = self.doc_scroll_force.take() {
                    self.doc_scroll_y = f;
                    scroll = scroll.vertical_scroll_offset(f);
                }

                scroll.show_viewport(ui, |ui, viewport| {
                    ui.set_height(total_height);
                    let content_top = ui.max_rect().top();
                    // 回灌：用户拖动主滚动条时，把主区实际偏移写回共享位置，驱动缩略图栏跟随。
                    self.doc_scroll_y = viewport.min.y;
                    let center_page = page_at_offset(&offsets, viewport.center().y);
                    if let Some(current) = center_page {
                        self.tabs[i].page = current;
                    }

                    for p in visible_page_range(&offsets, viewport) {
                        let (w_pt, h_pt) = self.tabs[i].doc.page_size_pt(p);
                        let w = w_pt * px_per_pt / ppp;
                        let h = h_pt * px_per_pt / ppp;
                        // 请求渲染（未缓存/未在途才发送）：视线中心页优先，
                        // 其余可见页（含上下预取）普通优先级，保证跳页目标最快出图。
                        let is_center = center_page == Some(p);
                        self.tabs[i].request_render(p, zoom, max_texture_side, is_center);
                        let row_rect = egui::Rect::from_min_size(
                            egui::pos2(ui.max_rect().left(), content_top + offsets[p]),
                            Vec2::new(avail_w, h),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .id_salt(("page", p))
                                .max_rect(row_rect)
                                .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
                            |ui| {
                                if let Some(tid) = self.tabs[i].display_texture_for(p) {
                                    ui.image(SizedTexture::new(tid, Vec2::new(w, h)));
                                } else {
                                    ui.allocate_space(Vec2::new(w, h));
                                }
                            },
                        );
                    }
                });
            }
        }
    }

    // ---- 底部状态栏 ----

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // 状态消息 5 秒后自动清除，避免旧消息常驻状态栏
        if !self.status.is_empty()
            && self.status_at.elapsed() > std::time::Duration::from_secs(5)
        {
            self.status.clear();
        }
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
            // 退出：Esc 或 ⌘句点 (.)
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
                || ctx.input_mut(|i| {
                    i.consume_key(Modifiers::COMMAND, Key::Period)
                })
            {
                self.exit_presentation(ctx);
            }
            // 下一页/下一个动画：N / PageDown / 向右 / 向下 / 空格
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::N))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageDown))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowRight))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space))
            {
                self.navigate(1);
            }
            // 上一页/返回上一个动画：P / PageUp / 向左 / 向上
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::P))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageUp))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowLeft))
                || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp))
            {
                self.navigate(-1);
            }
            return;
        }

        let cmd = Modifiers::COMMAND;
        // 进入演示：⇧⌘Return 从头开始；⌘Return 从当前页。
        // 必须先判带 SHIFT 的：consume_key 的修饰键匹配会忽略多余的 Shift，
        // 若先判 ⌘Return，⇧⌘Return 会被它抢先消费。
        if ctx.input_mut(|i| {
            i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter)
        }) {
            self.start_presentation_from_beginning(ctx);
        } else if ctx.input_mut(|i| i.consume_key(cmd, Key::Enter)) {
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
                crate::sysmenu::SysCmd::OpenFiles(paths) => {
                    for p in paths {
                        self.open_path(std::path::Path::new(&p));
                    }
                }
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
                crate::sysmenu::SysCmd::PresentationFromBeginning => {
                    self.start_presentation_from_beginning(ctx)
                }
                crate::sysmenu::SysCmd::Settings => self.show_settings = true,
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

    /// 从头开始放映：跳转到首页再进入演示模式。
    fn start_presentation_from_beginning(&mut self, ctx: &egui::Context) {
        if self.active_idx().is_none() {
            return;
        }
        self.goto(0);
        self.start_presentation(ctx);
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
        let max_texture_side = ui.ctx().input(|input| input.max_texture_side);
        let pres_zoom = ((disp_w * ppp) / pw / scale_px_per_pt(1.0)).clamp(0.1, 4.0);
        // 预渲染当前页与相邻页（翻页时高清图已就绪，避免先模糊）
        let lo = page.saturating_sub(1);
        let hi = (page + 1).min(page_count.saturating_sub(1));
        for np in lo..=hi {
            // 演示模式当前页优先，相邻页普通优先级。
            let is_current = np == page;
            self.tabs[i].request_render(np, pres_zoom, max_texture_side, is_current);
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

        // 页码指示（右下角）：page 为 0 基，显示需 +1
        let text = format!("{} / {page_count}", page + 1);
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
            self.keep_repainting_while_rendering(&ctx);
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
            .max_size(200.0)
            .show(ui, |ui| self.thumb_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.doc_panel(ui));

        // 设置窗口（菜单栏「设置… ⌘,」打开）
        if self.show_settings {
            self.settings_window(&ctx);
        }
        self.keep_repainting_while_rendering(&ctx);
    }
}

/// 加载界面字体：优先使用用户选择的字体（`font_path`），否则用系统 CJK 字体兜底。
///
/// 选择的字体经 [`font_is_renderable`] 校验可渲染后才会应用，避免所选字体（如系统保留
/// 字体）无法渲染导致整个界面文字消失。始终保留默认 CJK 字体作为 fallback。
fn setup_cjk_fonts(ctx: &egui::Context, font_path: Option<&str>) {
    let mut fonts = FontDefinitions::default();

    // 首选界面字体：用户自选字体优先；否则用系统自带的苹方作为默认。
    // 均经 font_is_renderable 校验，避免所选字体无法渲染导致整个界面文字消失。
    let mut applied = false;
    if let Some(path) = font_path {
        if let Ok(bytes) = std::fs::read(path) {
            if font_is_renderable(&bytes) {
                fonts
                    .font_data
                    .insert("ui".into(), Arc::new(FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "ui".into());
                log::info!("已应用界面字体：{path}");
                applied = true;
            } else {
                log::warn!("所选字体无法渲染，忽略：{path}");
            }
        }
    }
    if !applied {
        // 默认：macOS 自带的苹方（PingFang.ttc），运行时从系统读取，不内嵌
        if let Some(path) = find_system_pingfang() {
            if let Ok(bytes) = std::fs::read(&path) {
                if font_is_renderable(&bytes) {
                    fonts
                        .font_data
                        .insert("ui".into(), Arc::new(FontData::from_owned(bytes)));
                    fonts
                        .families
                        .entry(egui::FontFamily::Proportional)
                        .or_default()
                        .insert(0, "ui".into());
                    log::info!("已应用系统苹方字体：{path}");
                } else {
                    log::warn!("系统苹方无法渲染，跳过：{path}");
                }
            }
        } else {
            log::warn!("未找到系统苹方字体，使用系统 CJK 兜底");
        }
    }

    // 系统 CJK 字体兜底
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

/// 校验字体数据确实可渲染（ab_glyph 能解析出至少一个常用字符的字形）。
///
/// egui 底层用 ab_glyph 解析字体；系统「保留字体」（如 PingFangUI 等以 `.` 开头的
/// 字体）能被解析但无实际字形，会让界面文字全部消失。这里用 ab_glyph 预检，
/// 确保所选字体至少能渲染出字母与汉字。
fn font_is_renderable(bytes: &[u8]) -> bool {
    use ab_glyph::{Font, FontArc};
    let Ok(font) = FontArc::try_from_vec(bytes.to_vec()) else {
        return false;
    };
    // 检查常见字符是否都有字形：拉丁字母、数字、中文。
    // 用 all 而非 any：缺中文字形的纯拉丁字体不应通过界面字体校验。
    let probes = ['A', '1', '中'];
    probes.iter().all(|c| font.glyph_id(*c).0 != 0)
}

/// 枚举系统字体：返回 (家族名, 文件路径) 列表。
///
/// 直接用 `CTFontManagerCopyAvailableFontFamilyNames` 拿干净的家族名，再用
/// `CTFont` 的 URL 属性定位字体文件。过滤掉：
/// - 系统保留字体（家族名以 `.` 开头，如 `.PingFang UI SC`，无实际字形不可渲染）
/// - name 表异常的乱码家族名（含 NUL / 替换字符）
fn list_system_fonts() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let families = CTFontManagerCopyAvailableFontFamilyNames();
        let count = families.count();
        for i in 0..count {
            let ptr = families.value_at_index(i);
            if ptr.is_null() {
                continue;
            }
            let cf = &*(ptr as *const CFString);
            let name = cf.to_string();
            // 跳过保留字体与乱码
            if name.is_empty()
                || name.starts_with('.')
                || name.contains('\0')
                || name.contains('\u{FFFD}')
            {
                continue;
            }
            // 家族名 → 字体文件路径
            if let Some(path) = family_path(&name) {
                out.push((name, path));
            }
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out.dedup_by(|a, b| a.1 == b.1);
    out
}

/// 家族名 → 字体文件路径（经 `CTFont` 的 URL 属性）。
fn family_path(family: &str) -> Option<String> {
    unsafe {
        let cf_name = CFString::from_str(family);
        let font = CTFont::with_name(&cf_name, 12.0, std::ptr::null());
        let attr = font.attribute(&kCTFontURLAttribute)?;
        let url = attr.downcast_ref::<CFURL>()?;
        let cf_path = url.file_system_path(CFURLPathStyle::CFURLPOSIXPathStyle)?;
        let path = cf_path.to_string();
        if path.is_empty() {
            return None;
        }
        Some(path)
    }
}

/// 定位系统自带的苹方字体文件（PingFang.ttc）。
///
/// 注意：不能用 `family_path("PingFang SC")`——它解析到的是 `PingFangUI.ttc`，
/// 其首个字体是系统保留字体 `.PingFangUITextSC-Regular`，ab_glyph 无法渲染，
/// 会导致整个界面文字消失。因此这里直接枚举系统字体文件，找文件名恰为
/// `PingFang.ttc` 的（位于 AssetsV2 缓存），其首个字体 PingFangHK-Regular 可渲染。
fn find_system_pingfang() -> Option<String> {
    unsafe {
        let urls = CTFontManagerCopyAvailableFontURLs();
        let count = urls.count();
        for i in 0..count {
            let ptr = urls.value_at_index(i);
            if ptr.is_null() {
                continue;
            }
            let url = &*(ptr as *const CFURL);
            let Some(cf_path) = url.file_system_path(CFURLPathStyle::CFURLPOSIXPathStyle) else {
                continue;
            };
            let path = cf_path.to_string();
            if path.ends_with("PingFang.ttc") {
                return Some(path);
            }
        }
        None
    }
}

// ---- 设置窗口 ----

impl SmartPdfApp {
    /// 渲染「字体设置」窗口：界面字体选择（点选暂存到 self.settings_pending，点「确认」才应用）。
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        // 底部按钮动作
        let mut confirmed = false;
        let mut cancelled = false;
        let current = self.ui_font.clone();
        // 显示用：待确认的选择优先（若尚未点选则显示当前生效字体）
        let display: Option<String> = self
            .settings_pending
            .clone()
            .unwrap_or_else(|| current.clone());

        egui::Window::new("字体设置")
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(380.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.heading("字体设置");
                    ui.label("默认使用 macOS 自带苹方（PingFang）；也可选择其他系统字体：");
                    ui.add_space(6.0);

                    // CoreText 枚举只跑一次，之后复用缓存（克隆出来避免与
                    // 闭包内对 self 的可变借用冲突）。
                    let fonts = self
                        .font_list
                        .get_or_insert_with(list_system_fonts)
                        .clone();

                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            // 「默认」选项：使用系统苹方
                            let is_default = display.is_none();
                            if ui
                                .selectable_label(is_default, "默认（系统苹方 PingFang）")
                                .clicked()
                            {
                                self.settings_pending = Some(None);
                            }
                            // 各系统字体
                            for (name, path) in &fonts {
                                let selected = display.as_deref() == Some(path.as_str());
                                if ui.selectable_label(selected, name).clicked() {
                                    self.settings_pending = Some(Some(path.clone()));
                                }
                            }
                        });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("当前：");
                        ui.label(
                            display.as_deref().unwrap_or("默认（系统苹方 PingFang）"),
                        );
                    });
                    ui.add_space(6.0);
                    // 确认/取消：点选已存到 self.settings_pending，这里记录按钮动作
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("确认").clicked() {
                                confirmed = true;
                            }
                            if ui.button("取消").clicked() {
                                cancelled = true;
                            }
                        });
                    });
                });
            });

        // 点「确认」才应用并持久化；点「取消」或直接关闭则放弃（不改当前字体）
        if confirmed {
            if let Some(new_font) = self.settings_pending.take() {
                self.apply_ui_font(ctx, new_font.as_deref());
                save_font_config(new_font.as_deref());
            }
            self.settings_pending = None;
            self.show_settings = false;
        } else if cancelled {
            // 放弃待确认选择，保持当前字体；关闭窗口
            self.settings_pending = None;
            self.show_settings = false;
        } else {
            // 用户还没点确认/取消：保持窗口打开（除非用户点了 × 关闭，此时 open 已被置 false）
            if !open {
                self.settings_pending = None;
            }
            self.show_settings = open;
        }
    }

    /// 应用界面字体并持久化。
    fn apply_ui_font(&mut self, ctx: &egui::Context, path: Option<&str>) {
        self.ui_font = path.map(|p| p.to_string());
        setup_cjk_fonts(ctx, path);
    }
}

/// 字体偏好配置文件路径。
fn font_config_path() -> PathBuf {
    let dir = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(dir)
        .join("Library/Application Support/SmartPDF Pro")
        .join("font.conf")
}

/// 读取保存的字体偏好。
fn load_font_config() -> Option<String> {
    let path = font_config_path();
    let s = std::fs::read_to_string(&path).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() || !Path::new(&s).exists() {
        return None;
    }
    Some(s)
}

/// 保存字体偏好（None 表示系统默认，删除配置）。
fn save_font_config(path: Option<&str>) {
    let file = font_config_path();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match path {
        Some(p) => {
            let _ = std::fs::write(&file, p);
        }
        None => {
            let _ = std::fs::remove_file(&file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cumulative_page_offsets, family_path, find_system_pingfang, font_is_renderable,
        list_system_fonts, page_at_offset, visible_page_range,
    };

    #[test]
    fn variable_page_offsets_preserve_real_heights() {
        let offsets = cumulative_page_offsets([100.0, 250.0, 50.0]);
        assert_eq!(offsets, [0.0, 100.0, 350.0, 400.0]);
        assert_eq!(page_at_offset(&offsets, 0.0), Some(0));
        assert_eq!(page_at_offset(&offsets, 99.0), Some(0));
        assert_eq!(page_at_offset(&offsets, 100.0), Some(1));
        assert_eq!(page_at_offset(&offsets, 399.0), Some(2));
        assert_eq!(page_at_offset(&offsets, 999.0), Some(2));
    }

    #[test]
    fn visible_range_uses_real_offsets_and_prefetches_neighbors() {
        let offsets = cumulative_page_offsets([100.0, 250.0, 50.0, 400.0]);
        let viewport = egui::Rect::from_min_max(egui::pos2(0.0, 120.0), egui::pos2(10.0, 360.0));
        assert_eq!(visible_page_range(&offsets, viewport), 0..4);

        let empty = cumulative_page_offsets([]);
        assert_eq!(visible_page_range(&empty, viewport), 0..0);
    }

    #[test]
    fn enumerates_system_fonts() {
        let fonts = list_system_fonts();
        assert!(!fonts.is_empty(), "应能枚举出系统字体");
        // 至少应包含苹方/冬青等中文字体家族
        let has_cjk = fonts.iter().any(|(name, _)| {
            name.contains("PingFang")
                || name.contains("苹方")
                || name.contains("Hiragino")
                || name.contains("Heiti")
        });
        assert!(has_cjk, "应能找到中文字体家族");
        println!(
            "枚举到 {} 个字体，示例：{:?}",
            fonts.len(),
            &fonts[..3.min(fonts.len())]
        );
    }

    #[test]
    fn font_family_lookup_works() {
        // Hiragino 家族名应能定位到其字体文件
        let path = family_path("Hiragino Sans GB");
        assert!(path.is_some(), "应能定位 Hiragino Sans GB 的字体文件");
        println!("Hiragino 字体文件: {path:?}");
    }

    #[test]
    fn renderable_check_rejects_garbage() {
        // 垃圾字节不应被认为可渲染
        assert!(!font_is_renderable(b"not a font at all"));
        // 真实字体应可渲染
        let bytes = std::fs::read("/System/Library/Fonts/Hiragino Sans GB.ttc").unwrap();
        assert!(font_is_renderable(&bytes), "Hiragino 应可渲染");
    }

    #[test]
    fn system_pingfang_is_renderable() {
        // 系统自带苹方应能被定位且可渲染（否则界面文字会消失）
        let path = find_system_pingfang()
            .expect("macOS 应自带苹方字体 PingFang.ttc");
        let bytes = std::fs::read(&path).expect("应能读取苹方字体文件");
        assert!(
            font_is_renderable(&bytes),
            "系统苹方应可渲染：{path}"
        );
        println!("系统苹方: {path}");
    }

    #[test]
    fn diag_kingsoft_family() {
        let name = family_path("Kingsoft UE");
        println!("Kingsoft UE 诊断: {name:?}");
    }
}
