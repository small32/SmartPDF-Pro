#!/bin/bash
# 一键构建 SmartPDF Pro macOS 应用包（.app），支持多架构。
# 用法：./build_app.sh [release|dev] [all|x86_64|arm64|universal]
#   all       构建 x86_64 + arm64 + universal 三个版本（默认）
#   x86_64    仅 Intel
#   arm64     仅 Apple Silicon
#   universal 仅通用二进制
set -euo pipefail
cd "$(dirname "$0")"

MODE="${1:-release}"
ARCH="${2:-all}"

# 架构 → Rust target 与产物目录
case "$ARCH" in
  x86_64)    TARGETS="x86_64-apple-darwin" ;;
  arm64)     TARGETS="aarch64-apple-darwin" ;;
  universal) TARGETS="x86_64-apple-darwin aarch64-apple-darwin" ;;
  all)       TARGETS="x86_64-apple-darwin aarch64-apple-darwin" ;;
  *) echo "未知架构: ${ARCH}（应为 all|x86_64|arm64|universal）" >&2; exit 1 ;;
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

declare -a BINS=()
for t in $TARGETS; do
  echo "    -- cargo build $CARGO_FLAGS --target $t"
  cargo build $CARGO_FLAGS --target "$t"
  BINS+=("target/$t/$SUBDIR/smartpdf-pro")
done

# universal：用 lipo 合并两个单架构二进制
UNIVERSAL_BIN=""
if [ "$ARCH" = "all" ] || [ "$ARCH" = "universal" ]; then
  mkdir -p "target/universal/$SUBDIR"
  UNIVERSAL_BIN="target/universal/$SUBDIR/smartpdf-pro"
  echo "==> 合并 universal 二进制 (lipo)"
  lipo -create "${BINS[@]}" -output "$UNIVERSAL_BIN"
  file "$UNIVERSAL_BIN"
fi

# 与 src/icon.rs 内嵌的图标同源，保证 Dock 图标、窗口图标与 App 图标一致
ICON=assets/icon-1024.png

echo "==> 2/4 生成应用图标"
[ -f "$ICON" ] || cargo run --quiet --example gen_icon -- "$ICON"
rm -rf target/AppIcon.iconset
mkdir -p target/AppIcon.iconset
for sz in 16 32 128 256 512; do
    sips -z "$sz" "$sz" "$ICON" --out "target/AppIcon.iconset/icon_${sz}x${sz}.png" >/dev/null
    d=$((sz * 2))
    sips -z "$d" "$d" "$ICON" --out "target/AppIcon.iconset/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns target/AppIcon.iconset -o target/AppIcon.icns

# 组装单个 .app：$1=app 名称，$2=二进制路径
assemble_app() {
  local app_name="$1" bin="$2"
  local app="dist/$app_name"
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
    assemble_app "SmartPDF Pro (x86_64).app" "target/x86_64-apple-darwin/$SUBDIR/smartpdf-pro"
    ;;
  arm64)
    assemble_app "SmartPDF Pro (arm64).app" "target/aarch64-apple-darwin/$SUBDIR/smartpdf-pro"
    ;;
  universal)
    assemble_app "SmartPDF Pro (Universal).app" "$UNIVERSAL_BIN"
    ;;
  all)
    assemble_app "SmartPDF Pro (x86_64).app" "target/x86_64-apple-darwin/$SUBDIR/smartpdf-pro"
    assemble_app "SmartPDF Pro (arm64).app" "target/aarch64-apple-darwin/$SUBDIR/smartpdf-pro"
    assemble_app "SmartPDF Pro (Universal).app" "$UNIVERSAL_BIN"
    ;;
esac

echo "==> 4/5 完成"
ls -1 dist/*.app
echo "启动方式：open \"dist/SmartPDF Pro (Universal).app\""
