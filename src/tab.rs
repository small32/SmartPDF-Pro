//! 标签页层：一个文档一个标签，维护当前页、缩放与多页渲染缓存。
//!
//! 渲染在独立线程中进行：UI 线程通过 channel 发请求、按期接收
//! 像素结果后上传 GPU 纹理，避免大页面渲染阻塞界面。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use egui::{
    ColorImage, Context, TextureHandle, TextureId, TextureOptions,
};

use crate::document::{scale_px_per_pt, Document, RenderedPage};

/// 渲染请求（UI → 渲染线程）。
struct RenderRequest {
    page: usize,
    zoom: f32,
    scale: f32,
    /// 优先请求（用户视线中心页，如点击缩略图跳转的目标页）。渲染线程每轮
    /// 会把通道里积压的请求一次取出并优先处理这一类，避免预取请求排在
    /// 前面导致跳页后长时间停留在模糊占位图上。
    priority: bool,
}

/// 渲染结果（渲染线程 → UI）。
struct RenderResult {
    page: usize,
    zoom: f32,
    rendered: Result<RenderedPage, String>,
}

/// 缓存 key：页码 + 缩放千分比。
type RenderKey = (usize, u32);

fn clamp_render_scale(
    width_pt: f32,
    height_pt: f32,
    requested: f32,
    max_texture_side: usize,
) -> f32 {
    let longest_pt = width_pt.abs().max(height_pt.abs());
    if longest_pt <= 0.0 || !longest_pt.is_finite() {
        return requested;
    }
    // 给 MuPDF 的像素边界取整留少量余量，避免恰好超出一两个像素。
    let safe_side = max_texture_side.saturating_sub(2).max(1) as f32;
    requested.min(safe_side / longest_pt)
}

/// UI 线程持有的页面缓存（GPU 纹理）。
struct CachedPage {
    handle: TextureHandle,
}

/// 渲染线程主体：持有独立的 Document 实例（MuPDF 原生指针不可跨线程，
/// 但可以在本线程内长期复用），循环处理请求。
///
/// 每轮先把通道里积压的请求一次排空取出，稳定排序让 `priority` 请求
/// 插队优先渲染（同级保持 FIFO），保证「点击缩略图/翻页」后目标页
/// 最快出图，不被更早进入通道的预取请求挡住。
fn render_worker(path: PathBuf, rx: Receiver<RenderRequest>, tx: Sender<RenderResult>) {
    let mut doc: Option<Document> = None;
    loop {
        // 排空通道：取第一个请求（阻塞等待），再收集本轮已就绪的其余请求。
        let mut batch: Vec<RenderRequest> = Vec::new();
        match rx.recv() {
            Ok(first) => batch.push(first),
            Err(_) => break, // 发送端全部关闭，渲染线程退出
        }
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        // 稳定排序：优先请求在前，同级保持原到达顺序（FIFO）。
        batch.sort_by_key(|req| !req.priority);

        for RenderRequest {
            page,
            zoom,
            scale,
            ..
        } in batch
        {
            if doc.is_none() {
                match Document::open(&path) {
                    Ok(d) => doc = Some(d),
                    Err(error) => {
                        let _ = tx.send(RenderResult {
                            page,
                            zoom,
                            rendered: Err(error),
                        });
                        continue;
                    }
                }
            }
            if let Some(d) = doc.as_ref() {
                let rendered = d.render_page(page, scale);
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
    /// 上次计算 fit_width 时的文档区域宽度（用于避免每帧振荡重算；不含左侧缩略图栏）。
    pub fit_width_viewport: f32,
    /// 多页渲染缓存，缓存页数上限见 [`Self::CACHE_LIMIT`]。
    caches: HashMap<RenderKey, CachedPage>,
    /// 插入顺序（用于清理最旧缓存）。
    order: Vec<RenderKey>,
    req_tx: Sender<RenderRequest>,
    res_rx: Receiver<RenderResult>,
    /// 在途请求集合（同一 key 不重复发送；正文与缩略图缩放不同，多个 key 并存）。
    pending: std::collections::HashSet<RenderKey>,
    /// 在途请求的发出时刻。渲染线程异常退出等情况下结果可能永远不会到达，超时后回收
    /// 该 key（否则该页永久空白，且 UI 会为一个永不到达的结果持续排帧重绘）。
    pending_since: HashMap<RenderKey, Instant>,
    /// 最近失败的请求；短暂冷却后允许重试，避免永久空白和每帧重试。
    failed_at: HashMap<RenderKey, Instant>,
}

impl DocTab {
    /// 缓存页数上限：缩略图与正文共存（不同缩放），太小会频繁淘汰导致反复渲染。
    const CACHE_LIMIT: usize = 64;
    const RETRY_DELAY: Duration = Duration::from_secs(1);
    /// 在途请求超时：超过该时长仍未收到结果即视为丢失，解除占位以便重新发起。
    const PENDING_TIMEOUT: Duration = Duration::from_secs(10);

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
            pending_since: HashMap::new(),
            failed_at: HashMap::new(),
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
    ///
    /// `priority = true` 表示该页是用户当前视线中心（跳转目标页/当前页），
    /// 渲染线程会插队优先处理；预取页（视口上下各一页）用 `false`。
    pub fn request_render(&mut self, page: usize, zoom: f32, max_texture_side: usize, priority: bool) {
        let key = Self::key(page, zoom);
        if self.caches.contains_key(&key) || self.pending.contains(&key) {
            return;
        }
        if self
            .failed_at
            .get(&key)
            .is_some_and(|failed| failed.elapsed() < Self::RETRY_DELAY)
        {
            return;
        }
        self.failed_at.remove(&key);
        self.pending.insert(key);
        let scale = self.render_scale(page, zoom, max_texture_side);
        if self
            .req_tx
            .send(RenderRequest { page, zoom, scale, priority })
            .is_err()
        {
            self.pending.remove(&key);
            self.failed_at.insert(key, Instant::now());
        } else {
            self.pending_since.insert(key, Instant::now());
        }
    }

    /// 计算安全的 MuPDF 渲染比例，保证输出纹理任一边不超过 GPU 上限。
    ///
    /// 超长海报/画册 PDF 即使适应窗口宽度，高度仍可能超过 Metal 的纹理上限。
    /// 此时只降低底层纹理分辨率，UI 仍按原 zoom 显示，因此页面尺寸与比例不变。
    fn render_scale(&self, page: usize, zoom: f32, max_texture_side: usize) -> f32 {
        let requested = scale_px_per_pt(zoom);
        let (width_pt, height_pt) = self.doc.page_size_pt(page);
        clamp_render_scale(width_pt, height_pt, requested, max_texture_side)
    }

    /// 收集渲染结果并上传纹理；返回是否有新缓存插入（调用方可据此决定是否重绘）。
    /// 由 UI 每帧调用。
    pub fn poll_render(&mut self, ctx: &Context) -> bool {
        let mut inserted = false;
        while let Ok(res) = self.res_rx.try_recv() {
            let key = Self::key(res.page, res.zoom);
            self.pending.remove(&key);
            self.pending_since.remove(&key);
            let rendered = match res.rendered {
                Ok(rendered) => {
                    self.failed_at.remove(&key);
                    rendered
                }
                Err(error) => {
                    self.failed_at.insert(key, Instant::now());
                    log::warn!(
                        "页面渲染失败，稍后重试: page={} zoom_permille={}: {}",
                        res.page, key.1, error
                    );
                    continue;
                }
            };
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
                [rendered.width, rendered.height],
                &rendered.rgba,
            );
            let handle = ctx.load_texture("page", image, TextureOptions::LINEAR);
            self.order.retain(|k| *k != key);
            self.order.push(key);
            self.caches.insert(key, CachedPage { handle });
            inserted = true;
        }
        // 超时回收：长时间无结果的在途请求视为丢失，解除占位（下次会被重新请求）。
        let expired: Vec<RenderKey> = self
            .pending_since
            .iter()
            .filter(|(_, sent_at)| sent_at.elapsed() > Self::PENDING_TIMEOUT)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            log::warn!(
                "渲染结果超时未返回，重新排队: page={} zoom_permille={}",
                key.0,
                key.1
            );
            self.pending.remove(&key);
            self.pending_since.remove(&key);
            self.failed_at.insert(key, Instant::now());
        }
        inserted
    }

    /// 是否仍有渲染请求在途。
    ///
    /// 供 UI 决定是否需要继续排帧：渲染线程出图后没有任何输入事件时，eframe 会回到
    /// 空闲状态，新纹理不会被画出来（表现为「点了缩略图主区迟迟不刷新」）。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
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
                priority: false,
            })
            .unwrap();
        drop(req_tx); // 让 worker 循环在完成后退出

        let res = res_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert_eq!(res.page, 0);
        let rendered = res.rendered.unwrap();
        assert!(rendered.width > 0 && rendered.height > 0);
        assert_eq!(
            rendered.rgba.len(),
            rendered.width * rendered.height * 4
        );
    }

    #[test]
    fn worker_returns_errors_so_failed_requests_can_be_retried() {
        let (req_tx, req_rx) = channel();
        let (res_tx, res_rx) = channel();
        thread::Builder::new()
            .name("test-worker-error".into())
            .spawn(move || render_worker(PathBuf::from("samples/demo.pdf"), req_rx, res_tx))
            .unwrap();

        req_tx
            .send(RenderRequest {
                page: usize::MAX,
                zoom: 1.0,
                scale: 1.5,
                priority: false,
            })
            .unwrap();
        drop(req_tx);

        let res = res_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(res.rendered.is_err());
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

    #[test]
    fn ultra_tall_page_is_clamped_to_gpu_texture_limit() {
        let scale = clamp_render_scale(2839.5, 20767.5, 1.0, 8192);
        assert!(20767.5 * scale <= 8190.0);
        assert!(2839.5 * scale > 0.0);
        assert_eq!(clamp_render_scale(612.0, 792.0, 1.0, 8192), 1.0);
    }
}
