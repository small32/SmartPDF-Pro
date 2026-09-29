//! 文档层。
//! - 对 MuPDF 的封装：PDF/EPUB/MOBI/CBZ/FB2/XPS/SVG 等格式；
//! - 对 ofd-core 的封装：OFD（GB/T 33190—2016）格式。
//! 统一对外暴露页数、页尺寸（pt）与按缩放渲染页面像素的接口。

use std::fs::File;
use std::path::Path;
use std::sync::Mutex;

use mupdf::{Colorspace, Document as MuDocument, Matrix};
use ofd_core::render::RenderOptions;
use ofd_core::{LoadedDocument, OfdReader, StBox};

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

/// 将 MuPDF 的预乘 Alpha 颜色通道合成到白色背景。
fn premultiplied_channel_on_white(channel: u8, alpha: u8) -> u8 {
    (channel as u16 + (255 - alpha) as u16).min(255) as u8
}

/// OFD 页面尺寸以毫米计，这里换算成 pt 以便复用统一的「像素/点」渲染缩放。
const PT_PER_MM: f32 = 72.0 / 25.4;

/// OFD 用毫米表示页面尺寸，SmartPDF 对外统一用 pt（1/72 英寸）。
fn mm_to_pt(mm: f64) -> f32 {
    (mm * PT_PER_MM as f64) as f32
}

/// 底层引擎差异：内存/指针模型不同，互不通用。
enum DocumentInner {
    /// MuPDF 负责的多格式文档。
    Mu(MuDocument),
    /// ofd-core 负责的 OFD 文档。渲染需 `&mut self`，用互斥锁做内部可变。
    Ofd(OfdDoc),
}

/// OFD 分支持有的状态：读取器 + 已装载的版式文档。
struct OfdDoc {
    reader: Mutex<OfdReader<File>>,
    loaded: LoadedDocument,
}

/// 一个已打开的文档。
pub struct Document {
    inner: DocumentInner,
    pub title: String,
    pub page_count: usize,
    /// 每页尺寸（单位：pt，即 1/72 英寸）。
    pub page_sizes: Vec<(f32, f32)>,
}

fn default_title(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名".into())
}

impl Document {
    pub fn open(path: &Path) -> Result<Self, String> {
        if is_ofd(path) {
            Self::open_ofd(path)
        } else {
            Self::open_mu(path)
        }
    }

    fn open_mu(path: &Path) -> Result<Self, String> {
        let inner = MuDocument::open(path).map_err(|e| e.to_string())?;
        let page_count =
            inner.page_count().map_err(|e| e.to_string())? as usize;

        let title = inner
            .metadata(mupdf::document::MetadataName::Title)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_title(path));

        // 预读所有页尺寸（供"适应宽度"计算用）；体积很小，一次读完。
        let mut page_sizes = Vec::with_capacity(page_count);
        for i in 0..page_count {
            let page = inner.load_page(i as i32).map_err(|e| e.to_string())?;
            let bounds = page.bounds().map_err(|e| e.to_string())?;
            page_sizes.push((bounds.width(), bounds.height()));
        }

        Ok(Self {
            inner: DocumentInner::Mu(inner),
            title,
            page_count,
            page_sizes,
        })
    }

    fn open_ofd(path: &Path) -> Result<Self, String> {
        let mut reader = OfdReader::open(path).map_err(|e| e.to_string())?;
        let bodies = reader.ofd().doc_bodies.clone();
        let body = bodies
            .first()
            .ok_or_else(|| "OFD 文档不包含任何版式文档".to_string())?
            .clone();
        let loaded = reader.load_document(&body).map_err(|e| e.to_string())?;

        let page_count = loaded.pages().len();
        // 预读页尺寸（毫米 → pt），复用与渲染相同的页面物理区域选择逻辑。
        let mut page_sizes = Vec::with_capacity(page_count);
        for i in 0..page_count {
            let area = ofd_page_area(&mut reader, &loaded, i)?;
            page_sizes.push((mm_to_pt(area.width), mm_to_pt(area.height)));
        }

        let title = reader
            .ofd()
            .doc_bodies
            .first()
            .and_then(|b| b.doc_info.title.as_ref())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| default_title(path));

        Ok(Self {
            inner: DocumentInner::Ofd(OfdDoc {
                reader: Mutex::new(reader),
                loaded,
            }),
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
        match &self.inner {
            DocumentInner::Mu(inner) => render_mu_page(inner, page_idx, scale),
            DocumentInner::Ofd(ofd) => render_ofd_page(ofd, page_idx, scale),
        }
    }

    pub fn page_size_pt(&self, page_idx: usize) -> (f32, f32) {
        self.page_sizes
            .get(page_idx)
            .copied()
            .unwrap_or((612.0, 792.0)) // A4 兜底
    }
}

/// OFD 页面物理区域（毫米）。优先本页 Area，其次文档默认 PageArea，否则 A4。
/// 与 ofd-core 渲染时的选择保持一致，保证页尺寸与渲染结果吻合。
fn ofd_page_area(
    reader: &mut OfdReader<File>,
    loaded: &LoadedDocument,
    page_idx: usize,
) -> Result<StBox, String> {
    let page_ref = loaded
        .pages()
        .get(page_idx)
        .ok_or_else(|| format!("页码 {page_idx} 越界"))?
        .clone();
    let page = reader.load_page(loaded, &page_ref).map_err(|e| e.to_string())?;
    Ok(page
        .area
        .as_ref()
        .or(loaded.document.common_data.page_area.as_ref())
        .map(|a| a.physical_box)
        .unwrap_or(StBox::A4_MM))
}

fn render_mu_page(
    inner: &MuDocument,
    page_idx: usize,
    scale: f32,
) -> Result<RenderedPage, String> {
    let page = inner.load_page(page_idx as i32).map_err(|e| e.to_string())?;
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
            // MuPDF 输出预乘 Alpha：白底合成时不能再次乘 alpha。
            rgba.push(premultiplied_channel_on_white(r, a as u8));
            rgba.push(premultiplied_channel_on_white(g, a as u8));
            rgba.push(premultiplied_channel_on_white(b, a as u8));
            rgba.push(255);
        }
    }

    Ok(RenderedPage {
        rgba,
        width,
        height,
    })
}

fn render_ofd_page(
    ofd: &OfdDoc,
    page_idx: usize,
    scale: f32,
) -> Result<RenderedPage, String> {
    let mut reader = ofd
        .reader
        .lock()
        .map_err(|_| "OFD 渲染锁获取失败".to_string())?;
    // SmartPDF 的 scale 是像素/点；ofd-core 按 DPI（像素/英寸）渲染，
    // 而 1 英寸 = 72 点，故 dpi = scale × 72。白底保证透明处不透底色。
    let dpi = scale * PDF_PT_PER_INCH;
    let opts =
        RenderOptions::with_dpi(dpi as f64).background(Some([255, 255, 255, 255]));
    let img = reader
        .render_page_to_image(&ofd.loaded, page_idx, &opts)
        .map_err(|e| e.to_string())?;
    let (width, height) = (img.width() as usize, img.height() as usize);
    Ok(RenderedPage {
        rgba: img.into_raw(),
        width,
        height,
    })
}

/// 判断路径是否为 OFD 文档（按扩展名路由到 ofd-core，其余交给 MuPDF）。
fn is_ofd(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ofd"))
}

/// 支持打开的文件扩展名（对应 SumatraPDF 支持的多格式 + OFD）。
// 注意：不含 djvu（捆绑的 MuPDF 1.27.2 无 DjVu 解码器）；
// 不含 cbr/cb7（RAR/7z 需 mupdf-sys 的 libarchive 特性，未启用），仅 zip 系 cbz 可开。
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "pdf", "epub", "mobi", "azw", "azw3", "fb2", "cbz", "xps", "oxps",
    "svg", "ofd",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composites_premultiplied_channels_on_white_without_multiplying_twice() {
        assert_eq!(premultiplied_channel_on_white(0, 0), 255);
        assert_eq!(premultiplied_channel_on_white(64, 128), 191);
        assert_eq!(premultiplied_channel_on_white(128, 128), 255);
        assert_eq!(premultiplied_channel_on_white(200, 255), 200);
    }

    /// OFD 走 ofd-core 分支：页数、毫米→pt 页尺寸换算、按缩放渲染像素。
    #[test]
    fn opens_and_renders_ofd() {
        let doc = Document::open(Path::new("samples/sample.ofd")).expect("打开 OFD");
        assert_eq!(doc.page_count, 2);
        // 第 0 页用文档默认 PageArea（210×297 mm）；毫米换算 pt 需带 2px 容差。
        let (w0, h0) = doc.page_size_pt(0);
        assert!((w0 - 210_f32 * PT_PER_MM).abs() < 2.0);
        assert!((h0 - 297_f32 * PT_PER_MM).abs() < 2.0);
        // 第 1 页覆盖为逐页 Area（100×200 mm）。
        let (w1, h1) = doc.page_size_pt(1);
        assert!((w1 - 100_f32 * PT_PER_MM).abs() < 2.0);
        assert!((h1 - 200_f32 * PT_PER_MM).abs() < 2.0);

        let rendered = doc.render_page(0, 1.5).expect("渲染 OFD 第 0 页");
        assert!(rendered.width > 0 && rendered.height > 0);
        assert_eq!(rendered.rgba.len(), rendered.width * rendered.height * 4);
    }
}
