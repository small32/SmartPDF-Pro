#!/bin/bash
# 一键构建 SmartPDF Pro macOS 应用包（.app），支持多架构。
# 用法：./build_app.sh [release|dev] [all|x86_64|arm64]
#   all       构建 x86_64 + arm64 两个版本（默认）
#   x86_64    仅 Intel
#   arm64     仅 Apple Silicon
set -euo pipefail
cd "$(dirname "$0")"

MODE="${1:-release}"
ARCH="${2:-all}"

# 架构 → Rust target 与产物目录
case "$ARCH" in
  x86_64)    TARGETS="x86_64-apple-darwin" ;;
  arm64)     TARGETS="aarch64-apple-darwin" ;;
  all)       TARGETS="x86_64-apple-darwin aarch64-apple-darwin" ;;
  *) echo "未知架构: ${ARCH}（应为 all|x86_64|arm64）" >&2; exit 1 ;;
esac

# MODE → cargo 参数与产物子目录
case "$MODE" in
  release) CARGO_FLAGS="--release"; SUBDIR=release ;;
  dev)     CARGO_FLAGS="";          SUBDIR=debug ;;
  *) echo "未知模式: ${MODE}（应为 release|dev）" >&2; exit 1 ;;
esac

echo "==> 1/4 构建 $MODE 可执行文件（架构: ${ARCH}）"
# 确保所需 Rust 目标已安装
for t in $TARGETS; do
  rustup target list --installed | grep -qx "$t" || rustup target add "$t"
done

for t in $TARGETS; do
  echo "    -- cargo build $CARGO_FLAGS --target $t"
  cargo build $CARGO_FLAGS --target "$t"
done

# 与 src/icon.rs 内嵌的图标同源，保证 Dock 图标、窗口图标与 App 图标一致
ICON=assets/icon-1024.png

echo "==> 2/4 生成应用图标"
[ -f "$ICON" ] || cargo run --quiet --example gen_icon -- "$ICON"
# macOS 26 的 iconutil 会把工作区内带 com.apple.provenance 扩展属性的
# .iconset 误判为 Invalid Iconset。在系统临时目录生成可避开该属性，
# 完成后只把最终 .icns 写回 target。
ICON_WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$ICON_WORK_DIR"' EXIT
ICONSET="$ICON_WORK_DIR/AppIcon.iconset"
mkdir -p "$ICONSET"
for sz in 16 32 128 256 512; do
    sips -z "$sz" "$sz" "$ICON" --out "$ICONSET/icon_${sz}x${sz}.png" >/dev/null
    d=$((sz * 2))
    sips -z "$d" "$d" "$ICON" --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o target/AppIcon.icns

# 组装单个 .app：$1=输出路径，$2=二进制路径。
# 应用包本身始终命名为 SmartPDF Pro.app；all 模式仅通过父目录区分架构。
assemble_app() {
  local app="$1" bin="$2"
  echo "==> 组装 $app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$bin" "$app/Contents/MacOS/SmartPDF Pro"
  cp target/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
  cp packaging/Info.plist "$app/Contents/Info.plist"
  plutil -lint "$app/Contents/Info.plist" >/dev/null

  echo "==> 自签名（ad-hoc，无需证书，满足 Gatekeeper 本地校验）"
  codesign --force --deep --sign - "$app"
  codesign --verify --verbose=2 "$app"
}

echo "==> 3/5 组装应用包"
case "$ARCH" in
  x86_64)
    assemble_app "dist/SmartPDF Pro.app" "target/x86_64-apple-darwin/$SUBDIR/smartpdf-pro"
    ;;
  arm64)
    assemble_app "dist/SmartPDF Pro.app" "target/aarch64-apple-darwin/$SUBDIR/smartpdf-pro"
    ;;
  all)
    assemble_app "dist/x86_64/SmartPDF Pro.app" "target/x86_64-apple-darwin/$SUBDIR/smartpdf-pro"
    assemble_app "dist/arm64/SmartPDF Pro.app" "target/aarch64-apple-darwin/$SUBDIR/smartpdf-pro"
    ;;
esac

echo "==> 4/5 完成"
find dist -maxdepth 3 -name 'SmartPDF Pro.app' -print
case "$ARCH" in
  x86_64|arm64) LAUNCH_APP="dist/SmartPDF Pro.app" ;;
  all)          LAUNCH_APP="dist/arm64/SmartPDF Pro.app" ;;
esac
echo "启动方式：open \"$LAUNCH_APP\""
