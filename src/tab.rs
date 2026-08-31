//! 标签页层：一个文档一个标签，维护当前页、缩放与多页渲染缓存。
//!
//! 渲染在独立线程中进行：UI 线程通过 channel 发请求、按期接收
//! 像素结果后上传 GPU 纹理，避免大页面渲染阻塞界面。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use egui::{
    ColorImage, Context, TextureHandle, TextureId, TextureOptions,
};

use crate::document::{scale_px_per_pt, Document, RenderedPage};

/// 渲染请求（UI → 渲染线程）。
struct RenderRequest {
    page: usize,
    zoom: f32,
    scale: f32,
}

/// 渲染结果（渲染线程 → UI）。
struct RenderResult {
    page: usize,
    zoom: f32,
    rendered: RenderedPage,
}

/// 缓存 key：页码 + 缩放千分比。
type RenderKey = (usize, u32);

/// UI 线程持有的页面缓存（GPU 纹理）。
struct CachedPage {
    handle: TextureHandle,
}

/// 渲染线程主体：持有独立的 Document 实例（MuPDF 原生指针不可跨线程，
/// 但可以在本线程内长期复用），循环处理请求。
fn render_worker(path: PathBuf, rx: Receiver<RenderRequest>, tx: Sender<RenderResult>) {
    let mut doc: Option<Document> = None;
    while let Ok(req) = rx.recv() {
        let RenderRequest {
            page,
            zoom,
            scale,
        } = req;
        if doc.is_none() {
            match Document::open(&path) {
                Ok(d) => doc = Some(d),
                Err(_) => continue, // 文件打开失败：UI 侧会提示，这里静默
            }
        }
        if let Some(d) = doc.as_ref() {
            if let Ok(rendered) = d.render_page(page, scale) {
                let _ = tx.send(RenderResult { page, zoom, rendered });
            }
        }
    }
}

/// 每个打开的文档对应一个标签页。
pub struct DocTab {
    pub path: PathBuf,
    pub title: String,
    pub doc: Document,
    /// 当前页（0 基）。
    pub page: usize,
    /// 逻辑缩放，1.0 = 100%。
    pub zoom: f32,
    /// 是否处于"适应宽度"模式。
    pub fit_width: bool,
    /// 上次计算 fit_width 时的 viewport 宽度（用于避免每帧振荡重算）。
    pub fit_width_viewport: f32,
    /// 多页渲染缓存，缓存页数上限见 [`Self::CACHE_LIMIT`]。
    caches: HashMap<RenderKey, CachedPage>,
    /// 插入顺序（用于清理最旧缓存）。
    order: Vec<RenderKey>,
    req_tx: Sender<RenderRequest>,
    res_rx: Receiver<RenderResult>,
    /// 在途请求集合（同一 key 不重复发送；正文与缩略图缩放不同，多个 key 并存）。
    pending: std::collections::HashSet<RenderKey>,
}

impl DocTab {
    /// 缓存页数上限：缩略图与正文共存（不同缩放），太小会频繁淘汰导致反复渲染。
    const CACHE_LIMIT: usize = 64;

    pub fn open(path: &Path) -> Result<Self, String> {
        let doc = Document::open(path)?;
        let title = doc.title.clone();

        let (req_tx, req_rx) = channel();
        let (res_tx, res_rx) = channel();
        let worker_path = path.to_path_buf();
        thread::Builder::new()
            .name("render-worker".into())
            .spawn(move || render_worker(worker_path, req_rx, res_tx))
            .map_err(|e| format!("创建渲染线程失败: {e}"))?;

        Ok(Self {
            path: path.to_path_buf(),
            title,
            doc,
            page: 0,
            zoom: 1.0,
            fit_width: true,
            fit_width_viewport: 0.0,
            caches: HashMap::new(),
            order: Vec::new(),
            req_tx,
            res_rx,
            pending: std::collections::HashSet::new(),
        })
    }

    fn key(page: usize, zoom: f32) -> RenderKey {
        (page, (zoom * 1000.0).round() as u32)
    }

    pub fn page_size_pt(&self) -> (f32, f32) {
        self.doc.page_size_pt(self.page)
    }

    /// 跳转页码（0 基，越界自动钳制）。
    pub fn goto(&mut self, page: usize) {
        let clamped = page.min(self.doc.page_count.saturating_sub(1));
        if clamped != self.page {
            self.page = clamped;
        }
    }

    // ---- 渲染请求 / 结果收集 ----

    /// 请求渲染指定页（未缓存且不在途时才发送）。
    pub fn request_render(&mut self, page: usize, zoom: f32) {
        let key = Self::key(page, zoom);
        if self.caches.contains_key(&key) || self.pending.contains(&key) {
            return;
        }
        self.pending.insert(key);
        let scale = scale_px_per_pt(zoom);
        let _ = self.req_tx.send(RenderRequest { page, zoom, scale });
    }

    /// 收集渲染结果并上传纹理；返回是否有新缓存插入（调用方可据此决定是否重绘）。
    /// 由 UI 每帧调用。
    pub fn poll_render(&mut self, ctx: &Context) -> bool {
        let mut inserted = false;
        while let Ok(res) = self.res_rx.try_recv() {
            let key = Self::key(res.page, res.zoom);
            self.pending.remove(&key);
            log::debug!("渲染完成: page={} zoom_permille={}", res.page, key.1);
            // 已缓存同 key，跳过（worker 可能重复返回同一请求）
            if self.caches.contains_key(&key) {
                continue;
            }
            // 清理最旧缓存，控制内存
            if self.order.len() >= Self::CACHE_LIMIT {
                let oldest = self.order.remove(0);
                self.caches.remove(&oldest);
            }
            let image = ColorImage::from_rgba_unmultiplied(
                [res.rendered.width, res.rendered.height],
                &res.rendered.rgba,
            );
            let handle = ctx.load_texture("page", image, TextureOptions::LINEAR);
            self.order.retain(|k| *k != key);
            self.order.push(key);
            self.caches.insert(key, CachedPage { handle });
            inserted = true;
        }
        inserted
    }

    // ---- 获取显示纹理 ----

    /// 指定页的显示纹理（优先精确缩放，其次该页任意缩放，避免翻页白屏）。
    pub fn display_texture_for(&self, page: usize) -> Option<TextureId> {
        self.display_texture_at(page, self.zoom)
    }

    /// 指定缩放下的页面纹理（优先精确 key，其次该页任意缩放）。
    pub fn display_texture_at(&self, page: usize, zoom: f32) -> Option<TextureId> {
        if let Some(c) = self.caches.get(&Self::key(page, zoom)) {
            return Some(c.handle.id());
        }
        self.caches
            .iter()
            .find(|((p, _), _)| *p == page)
            .map(|(_, c)| c.handle.id())
    }

    /// 仅精确匹配指定缩放的纹理（演示模式用，避免低清缩略图拉伸模糊）。
    pub fn texture_exact(&self, page: usize, zoom: f32) -> Option<TextureId> {
        self.caches
            .get(&Self::key(page, zoom))
            .map(|c| c.handle.id())
    }
}#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 验证渲染 worker 线程闭环：收到请求 → MuPDF 渲染 → 像素结果回传。
    #[test]
    fn worker_renders_and_returns_pixels() {
        let (req_tx, req_rx) = channel();
        let (res_tx, res_rx) = channel();
        thread::Builder::new()
            .name("test-worker".into())
            .spawn(move || render_worker(PathBuf::from("samples/demo.pdf"), req_rx, res_tx))
            .unwrap();

        req_tx
            .send(RenderRequest {
                page: 0,
                zoom: 1.0,
                scale: 1.5,
            })
            .unwrap();
        drop(req_tx); // 让 worker 循环在完成后退出

        let res = res_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert_eq!(res.page, 0);
        assert!(res.rendered.width > 0 && res.rendered.height > 0);
        assert_eq!(
            res.rendered.rgba.len(),
            res.rendered.width * res.rendered.height * 4
        );
    }

    /// 页码跳转越界钳制正确。
    #[test]
    fn goto_clamps_bounds() {
        let mut tab = DocTab::open(Path::new("samples/demo.pdf")).unwrap();
        tab.goto(usize::MAX);
        assert_eq!(tab.page, tab.doc.page_count - 1);
        tab.goto(0);
        assert_eq!(tab.page, 0);
    }
}