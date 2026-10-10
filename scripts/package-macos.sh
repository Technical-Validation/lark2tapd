#!/bin/sh
# 本地生成可双击启动的 .app；仅本机临时签名，不发布、不公证。
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo '仅支持 macOS' >&2; exit 1; }
profile=debug
if [ "${1:-}" = --release ]; then
  cargo build --locked -p lark2tapd --release
  profile=release
else
  cargo build --locked -p lark2tapd
fi
app="$PWD/dist/Lark2Tapd.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$profile/lark2tapd" "$app/Contents/MacOS/lark2tapd"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.roxi.lark2tapd</string>
<key>CFBundleName</key><string>Lark2Tapd</string>
<key>CFBundleDisplayName</key><string>Lark → TAPD</string>
<key>CFBundleExecutable</key><string>lark2tapd</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
printf '\n已生成本地应用：%s\n默认真实模式；使用 LARK2TAPD_MOCK=1 或 --mock 启动演示，运行期间不能切换。\n' "$app"
