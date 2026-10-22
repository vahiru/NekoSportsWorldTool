#!/usr/bin/env bash
# 打包 macOS 通用 .app（arm64 + x86_64）：
#   scripts/macos-app.sh <arm64 二进制> <x86_64 二进制> <版本号> <输出目录>
# 产物：<输出目录>/NekoSportsWorldTool-macos-universal.zip（内含 NekoSportsWorldTool.app）
#
# 自更新仍走单架构 tar.gz（src/update.rs asset_name）；.app 内的程序位于
# Contents/MacOS/，current_exe 指向它，自替换同样适用。
set -euo pipefail

ARM_BIN=$1
X64_BIN=$2
VERSION=${3#v}
OUT=$4
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NAME=NekoSportsWorldTool
EXE=nekosportsworldtool

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
APP="$WORK/$NAME.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT"

# 通用二进制；系统 strip（rust-objcopy 在部分工具链版本上无法运行）
lipo -create "$ARM_BIN" "$X64_BIN" -output "$APP/Contents/MacOS/$EXE"
strip -x "$APP/Contents/MacOS/$EXE"
chmod 755 "$APP/Contents/MacOS/$EXE"

# 图标：1024 源图 → iconset → icns
ICONSET="$WORK/$NAME.iconset"
mkdir -p "$ICONSET"
SRC="$ROOT/assets/icon/app_icon_1024.png"
for s in 16 32 128 256 512; do
  sips -z $s $s "$SRC" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$SRC" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/$NAME.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleIdentifier</key><string>io.github.yanamineko.nekosportsworldtool</string>
  <key>CFBundleExecutable</key><string>$EXE</string>
  <key>CFBundleIconFile</key><string>$NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
EOF
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# ad-hoc 签名：arm64 必须有有效签名才能运行（lipo/strip 会使原签名失效）
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

ZIP="$OUT/$NAME-macos-universal.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "$ZIP"
