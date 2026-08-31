//! 无界面的渲染验证：直接把样本文档渲染成 PNG，确认 MuPDF 链路可用。

use mupdf::{Colorspace, Document, ImageFormat, Matrix};

fn render_all(path: &str, out_prefix: &str) -> Result<(), mupdf::Error> {
    let doc = Document::open(path)?;
    println!("{path}\n  页数 = {}", doc.page_count()?);
    // 渲染前 2 页（EPUB 等动态排版的文档页数可能很多）
    let pages = doc.page_count()?.min(2);
    for i in 0..pages {
        let page = doc.load_page(i)?;
        let bounds = page.bounds()?;
        println!("  第 {} 页尺寸 = {:.0} x {:.0} pt", i + 1, bounds.width(), bounds.height());
        let pixmap = page.to_pixmap(
            &Matrix::new_scale(1.5, 1.5),
            &Colorspace::device_rgb(),
            true,
            true,
        )?;
        let out = format!("{out_prefix}-p{i}.png");
        pixmap.save_as(&out, ImageFormat::PNG)?;
        println!("  已渲染 {} ({}x{})", out, pixmap.width(), pixmap.height());
    }
    Ok(())
}

fn main() {
    for (path, prefix) in [
        ("samples/demo.pdf", "samples/out-pdf"),
        ("samples/demo.epub", "samples/out-epub"),
        ("samples/demo.cbz", "samples/out-cbz"),
    ] {
        match render_all(path, prefix) {
            Ok(()) => {}
            Err(e) => println!("  !! 渲染失败: {e}"),
        }
    }
}