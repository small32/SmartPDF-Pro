# SmartPDF Pro

用 **Rust** 重构、面向 **macOS** 的轻量多格式阅读器（PDF / EPUB / MOBI / CBZ / FB2 / XPS 等），
**界面为纯 Rust 方案（egui）**。

灵感来源于 [SumatraPDF](https://www.sumatrapdfreader.org/)：
多格式、轻量、快速打开、标签页浏览。

---

## 为什么这么做

原仓库是 SumatraPDF 的完整 C++ 源码（约 190 个源文件、321MB 依赖），
UI 完全绑定 Windows 的 Win32/WPF，本身不支持 macOS。逐文件移植不现实。

因此本项目以 **Rust 从零重写阅读器外壳**，复用互联网上成熟的引擎：

| 层 | 选型 | 说明 |
|---|---|---|
| 文档内核 | [`mupdf`](https://crates.io/crates/mupdf)（MuPDF 官方安全绑定） | 与 SumatraPDF 同源——SumatraPDF 本身深度定制了 MuPDF，天然覆盖多格式 |
| GUI | [`eframe/egui`](https://crates.io/crates/eframe) 0.36 | 纯 Rust 即时模式 GUI，跨平台，macOS 原生运行 |
| 文件对话框 | [`rfd`](https://crates.io/crates/rfd) | 调用 macOS 系统原生面板 |

MuPDF 内置解析器：**PDF、EPUB、MOBI/AZW3、FB2、CBZ/CBR、XPS、SVG、DjVu** ——
与 SumatraPDF 的多格式卖点一一对应。

## 功能

**布局（macOS 原生风格，单窗口单文档）**：左上只留系统三色按钮，**文件名在标题栏居中**，
右上角是「缩略图侧栏开关」与「＋ 打开」两个图标按钮；左侧页面缩略图导航（可收起），
右侧整篇文档纵向连续滚动，底部状态栏。

- ✅ 多格式打开：`⌘O`、**命令行传参**（`smartpdf-pro 文档.pdf`）、**拖拽文件进窗口**（打开/替换当前文档）
- ✅ 翻页：`PgUp/PgDn`、`↑/↓`、`Home/End`；菜单「前往」也有对应项；连续模式**滚动时状态栏页码自动跟随**
- ✅ 缩放：`⌘+ / ⌘− / ⌘0`（菜单「缩放」）；结果就近吸附到 50%~200% 标准档位
- ✅ **适应宽度优先匹配最佳百分比**：按可视宽精确计算后向下吸附到最近的标准档位
  （如 109% → 100%、130% → 125%），默认窗口尺寸下打开即是整洁百分比
- ✅ **多分辨率/Retina 适配**：渲染分辨率自动乘屏幕倍率（`pixels_per_point`），
  Retina 屏以物理像素出图不发虚；窗口移到不同倍率的显示器时自动按新倍率重渲，
  缩略图与演示模式同样清晰
- ✅ **缩略图侧栏可收起**：标题栏右侧侧栏开关（激活态高亮）、菜单「视图 → 缩略图面板」或 `F9`；
  收起时侧栏完全隐藏、正文自动重新适配宽度
- ✅ **`⌘W` = 关闭当前文档**（不退出应用）
- ✅ 适应宽度：打开即默认开启，窗口尺寸变化自动贴合页面宽度
- ✅ 视图模式：菜单 → 视图 → 单页 / 连续显示页面
- ✅ **后台渲染**：页面渲染在独立线程完成，翻页缩放不卡 UI（结果经 channel 回传为 GPU 纹理缓存，LRU 淘汰）
- ✅ **演示模式（类 PPT）**：`⌘P` / `F5` 或 菜单 → 视图 → 演示；全屏黑底单页，`→/空格` 下一页、`←` 上一页、`Esc` 退出
- ✅ **中文界面**：菜单栏为中文，自动加载系统 CJK 字体（Hiragino Sans GB 等）

中文菜单栏：
```
文件：  打开… (⌘O) / 关闭 (⌘W) / 退出
视图：  单页 / 连续显示页面 / 缩略图面板 (F9) / 统一页宽 / 演示 (⌘P)
前往：  下一页 / 上一页 / 首页 / 末页
缩放：  适应宽度 / 实际大小 (⌘0) / 放大 (⌘+) / 缩小 (⌘-) / 200%~50%
```

## 构建 & 运行

依赖：Rust ≥ 1.95、CMake、C/C++ 工具链（`brew install cmake`）。

```bash
# 构建（首次会编译 MuPDF C 引擎，需几分钟）
cargo build

# 运行并直接打开文档
cargo run -- samples/demo.pdf

# 无界面渲染验证（把 samples/ 下三类文档渲染成 PNG）
cargo run --example render_check
```

> macOS 上构建注意：`mupdf-sys` 用 CMake 编译，需要系统里有 `cmake`；
> 默认特性已包含 system-fonts（调用 macOS CoreText 系统字库）。

## 打包成 macOS 应用（.app）

```bash
./build_app.sh            # 一键打包（release），产出 dist/SmartPDF Pro.app
open "dist/SmartPDF Pro.app"   # 像普通 Mac 应用一样启动（Finder 双击亦可）
```

打包内容：
- `Contents/MacOS/SmartPDF Pro` — release 可执行文件
- `Contents/Resources/AppIcon.icns` — 应用图标（由 `assets/icon-1024.png` 经 `iconutil` 生成；
  该 PNG 同时内嵌进二进制作为窗口图标与 Dock 图标，三者同源）
- `Contents/Info.plist` — Bundle 元信息（源码见 `packaging/Info.plist`）

> 应用未签名/未公证（本地开发用途），首次启动如被 Gatekeeper 拦截，
> 在 Finder 中右键应用 →「打开」即可放行。

## 架构

```
src/
├── main.rs     # 入口：eframe 启动、命令行参数、最小窗口尺寸
├── app.rs      # 主界面（egui）：标题栏（居中文件名+右上按钮）/ 缩略图侧栏 / 连续滚动文档 / 状态栏 / 快捷键
├── tab.rs      # 文档会话：页面状态 + 后台渲染线程（channel）+ 倍率感知的纹理缓存
├── icon.rs     # 应用图标：内嵌 PNG → 窗口图标 / Dock 图标（唯一来源）
└── document.rs # 文档层：MuPDF 封装，打开/页数/页尺寸/按缩放渲染 RGBA
samples/        # 测试样本（PDF / EPUB / CBZ 三种格式）
assets/         # icon-1024.png：图标源图（.icns 与内嵌图标均由此生成）
packaging/      # Info.plist（.app 打包源文件）
build_app.sh    # 一键打包脚本 → dist/SmartPDF Pro.app
examples/
├── render_check.rs # 无界面渲染回归测试
└── gen_icon.rs     # 生成应用图标（1024px PNG）
```

对应关系（原 C++ → Rust）：
- `C++ Canvas.cpp` 渲染与视图 → `Rust document.rs::Document::render_page` + `app.rs` 中央面板
- `C++ AppTools.cpp` 打开文件 → `Rust app.rs::SmartPdfApp::open_path`
- `C++ WindowTab` 文档管理 → `Rust tab.rs::DocTab`（单窗口单文档，打开即替换）
- `C++ DisplayModel::ViewSinglePage/ViewContinuous` → `Rust app.rs::ViewMode`

渲染管线：UI 线程把 `(页码, 缩放)` 请求投递给文档专属的渲染 worker 线程
（**MuPDF 原生指针不可跨线程，worker 在自己的线程内长期持有一份 `Document`
实例**）；worker 用 MuPDF 渲染出 RGBA 像素，经 channel 回传，UI 线程上传为
GPU 纹理并缓存（LRU 淘汰）。渲染 scale = 逻辑缩放 × 屏幕倍率，Retina 以
物理像素出图；缓存 key 含 (页码, 缩放, 倍率)，跨显示器移动窗口时按新倍率
自动重渲。渲染结果到达后自动请求重绘，未出图时显示占位，翻页瞬间显示旧缓存。

## 测试

```bash
cargo test        # 渲染 worker 闭环 + 页码跳转钳制
```

## 已知边界

- 连续模式用 `ScrollArea::show_rows` 只渲染可见行（等高近似）；混排页尺寸的文档跳页定位与页码跟随是近似值。
- 文件打开（解析元数据）为同步操作，超大文档首次打开须等待片刻。
- 未实现注释编辑、书签、搜索（MuPDF 绑定都支持，接口可渐进补齐）。
