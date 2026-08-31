#!/bin/bash
# 一键构建 SmartPDF Pro macOS 应用包（.app）。
# 用法：./build_app.sh [release|dev]
set -euo pipefail
cd "$(dirname "$0")"

MODE="${1:-release}"
APP="dist/SmartPDF Pro.app"
# CFBundleExecutable 与 .app 同名
EXE="SmartPDF Pro"

echo "==> 1/4 构建 $MODE 可执行文件"
if [ "$MODE" = "release" ]; then
    cargo build --release
    BIN=target/release/smartpdf-pro
else
    cargo build
    BIN=target/debug/smartpdf-pro
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

echo "==> 3/5 组装应用包"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$EXE"
cp target/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp packaging/Info.plist "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

echo "==> 4/5 自签名（ad-hoc，无需证书，满足 Gatekeeper 本地校验）"
codesign --force --deep --sign - "$APP"
codesign --verify --verbose=2 "$APP"

echo "==> 5/5 完成"
echo "构建产物：$(pwd)/$APP"
echo "启动方式：open \"$APP\""