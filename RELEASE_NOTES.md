# SmartPDF Pro 0.2.2

本次更新修复了 macOS 文件关联和超长 PDF 渲染问题。

## 修复

- 修复在 Finder 中右键选择“打开方式”时，SmartPDF Pro 提示无法打开 PDF 格式的问题。
- 完善冷启动和应用运行中的文件打开链路，兼容 `openURLs:`、`openFiles:` 和 `openFile:`。
- 修复超长页面 PDF 因纹理尺寸超过 GPU 上限而闪退的问题。渲染分辨率会根据设备上限自动调整，页面尺寸和比例保持不变。
