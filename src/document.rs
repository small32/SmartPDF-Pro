//! 文档层：对 MuPDF 的封装。
//! 负责打开 PDF/EPUB/MOBI/CBZ/FB2/XPS/SVG/DjVu 等格式、
//! 读取页数与页尺寸、按指定缩放渲染页面像素。

use std::path::Path;

use mupdf::{Colorspace, Document as MuDocument, Matrix};

/// 屏幕逻辑 DPI（egui 以逻辑像素为单位）。
pub const SCREEN_DPI: f32 = 96.0;
/// PDF 等文档使用的点（pt）与英寸换算：1in = 72pt。
pub const PDF_PT_PER_INCH: f32 = 72.0;

/// 缩放换算：逻辑缩放 1.0 => 屏幕上 1.333 px/pt。
pub fn scale_px_per_pt(zoom: f32) -> f32 {
    zoom * SCREEN_DPI / PDF_PT_PER_INCH
}

/// 渲染输出：RGBA 非预乘像素 + 宽高。
pub struct RenderedPage {
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// 一个已打开的文档。
pub struct Document {
    inner: MuDocument,
    pub title: String,
    pub page_count: usize,
    /// 每页尺寸（单位：pt，即 1/72 英寸）。
    pub page_sizes: Vec<(f32, f32)>,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self, String> {
        let inner = MuDocument::open(path).map_err(|e| e.to_string())?;
        let page_count =
            inner.page_count().map_err(|e| e.to_string())? as usize;

        let title = inner
            .metadata(mupdf::document::MetadataName::Title)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "未命名".into())
            });

        // 预读所有页尺寸（供"适应宽度"计算用）；体积很小，一次读完。
        let mut page_sizes = Vec::with_capacity(page_count);
        for i in 0..page_count {
            let page = inner.load_page(i as i32).map_err(|e| e.to_string())?;
            let bounds = page.bounds().map_err(|e| e.to_string())?;
            page_sizes.push((bounds.width(), bounds.height()));
        }

        Ok(Self {
            inner,
            title,
            page_count,
            page_sizes,
        })
    }

    /// 渲染第 `page_idx` 页（0 基），`scale` 单位是像素/点。
    ///
    /// 输出**始终是预合成白底的 RGBA**：透明的页面背景会被填成白色，
    /// 避免在 app 里（特别是深色主题下）背景透出深色面板导致文字看不清。
    pub fn render_page(&self, page_idx: usize, scale: f32) -> Result<RenderedPage, String> {
        let page = self
            .inner
            .load_page(page_idx as i32)
            .map_err(|e| e.to_string())?;
        let matrix = Matrix::new_scale(scale, scale);
        let pixmap = page
            .to_pixmap(&matrix, &Colorspace::device_rgb(), true, false)
            .map_err(|e| e.to_string())?;

        let width = pixmap.width() as usize;
        let height = pixmap.height() as usize;
        let stride = pixmap.stride() as usize;
        let samples: &[u8] = pixmap.samples();

        // 逐像素处理：白底合成 + 输出 RGBA(opaque)
        let mut rgba = Vec::with_capacity(height * width * 4);
        for y in 0..height {
            let row = &samples[y * stride..];
            for x in 0..width {
                let p = x * 4;
                let r = row[p];
                let g = row[p + 1];
                let b = row[p + 2];
                let a = row[p + 3] as u32;
                // 白底合成：out_rgb = rgb*a + 255*(255-a)
                let inv = 255 - a;
                rgba.push(((r as u32 * a + 255 * inv) / 255) as u8);
                rgba.push(((g as u32 * a + 255 * inv) / 255) as u8);
                rgba.push(((b as u32 * a + 255 * inv) / 255) as u8);
                rgba.push(255);
            }
        }

        Ok(RenderedPage {
            rgba,
            width,
            height,
        })
    }

    pub fn page_size_pt(&self, page_idx: usize) -> (f32, f32) {
        self.page_sizes
            .get(page_idx)
            .copied()
            .unwrap_or((612.0, 792.0)) // A4 兜底
    }
}

/// 支持打开的文件扩展名（对应 SumatraPDF 支持的多格式）。
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "pdf", "epub", "mobi", "azw", "azw3", "fb2", "cbz", "cbr", "cb7",
    "xps", "oxps", "svg", "djvu",
];