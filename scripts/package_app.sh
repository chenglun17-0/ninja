#!/usr/bin/env bash
# p7 签名分发：release 构建宿主 → 程序化绘制图标（make_icon.sh）→
# 组 Ninja.app（Info.plist 最小键集 + CFBundleIconFile）→
# 用本机可用代码签名身份对 .app 整体 codesign。
#
# 规则（对应 DISTRIBUTION.md）：
# - 只打 ninja 宿主：bundle 不含 ninja-preview / ninja-theme（PRODUCT：
#   默认零插件，分发物不含任何官方插件——主题插件也不进 DMG，换主题 =
#   用户本地装插件，见 DISTRIBUTION.md）。
# - 身份动态解析（security find-identity -v -p codesigning），优先
#   Developer ID Application，其次 Apple Development；一个身份都没有
#   = 直接失败，绝不静默回落 adhoc（假分发）。
# - 产物落 dist/（已 .gitignore）。DMG 见 scripts/package_dmg.sh。
#
# 用法：scripts/package_app.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

APP_NAME="Ninja"
BUNDLE_ID="dev.ninja.ninja"   # 钉死：同时是 codesign --identifier（签名稳定标识）
VERSION="0.1.0"               # 与 workspace.package version 一致
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"

echo "==> [1/5] cargo build --release -p ninja（仅宿主；ninja-preview/ninja-theme 不进 bundle）"
cargo build --release -p ninja

# LSMinimumSystemVersion 取证依据：产物二进制的 LC_BUILD_VERSION minos
#（rustc 对 aarch64-apple-darwin 的默认部署目标 = 11.0；objc2 0.6 /
# app-kit 0.3 声明的支持下限比它宽，但低于 minos 的声明是谎——dyld 在
# 更旧的系统上拒载该二进制。实机：macOS 26.6.1 arm64）。
MIN_SYS_VER="$(otool -l target/release/ninja \
  | awk '/LC_BUILD_VERSION/{f=1} f && /minos/{print $2; exit}')"
if [[ -z "$MIN_SYS_VER" ]]; then
  echo "错误：读不到二进制 LC_BUILD_VERSION minos（otool）" >&2
  exit 1
fi
echo "    LSMinimumSystemVersion = ${MIN_SYS_VER}（二进制 minos，见脚本注释）"

echo "==> [2/5] 组 $APP 骨架 + 程序化图标（Resources/AppIcon.icns）"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/ninja "$APP/Contents/MacOS/ninja"
# 图标是资源不是插件（「默认零插件」不变）：Swift/CoreGraphics 矢量绘制
# 10 尺寸（16…1024，含 @2x）→ iconutil 合 icns。make_icon.sh 内建
# 像素级回归自检，失败即中止打包——不产出无图标/坏图标的分发物。
scripts/make_icon.sh "$APP/Contents/Resources/AppIcon.icns"

echo "==> [3/5] Info.plist（最小键集 + CFBundleIconFile）"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>ninja</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon.icns</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$APP_NAME</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>$MIN_SYS_VER</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF
# 图标引用自检（回归）：资源与 plist 钥必须同时在、指向一致——任一
# 侧缺失/错名 = Finder/Dock 静默回退通用图标，肉眼难察，必须机器断言。
[[ -f "$APP/Contents/Resources/AppIcon.icns" ]] || {
  echo "错误：Resources/AppIcon.icns 缺失" >&2; exit 1; }
[[ "$(plutil -extract CFBundleIconFile raw "$APP/Contents/Info.plist")" == "AppIcon.icns" ]] || {
  echo "错误：Info.plist CFBundleIconFile ≠ AppIcon.icns" >&2; exit 1; }

echo "==> [4/5] 解析签名身份（security find-identity；缺真实身份 = 失败）"
IDENTITIES="$(security find-identity -v -p codesigning \
  | sed -n 's/^[[:space:]]*[0-9]*)[[:space:]]*[A-Fa-f0-9]* "\(.*\)"$/\1/p' \
  || true)"
if [[ -z "$IDENTITIES" ]]; then
  echo "错误：钥匙串里没有可用代码签名身份。不打包 adhoc 副本（假分发）。" >&2
  exit 1
fi
IDENTITY="$(grep -m1 '^Developer ID Application' <<<"$IDENTITIES" || true)"
if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(head -n1 <<<"$IDENTITIES")"
fi
echo "    身份：$IDENTITY"

echo "==> [5/5] codesign（identifier=${BUNDLE_ID}，hardened runtime）"
# --options runtime：为将来 Developer ID 公证铺路（公证硬性要求）；
# 宿主无 JIT/无动态库插件，运行时不受限。adhoc linker 签名被本步覆盖。
codesign --force \
  --identifier "$BUNDLE_ID" \
  --options runtime \
  --sign "$IDENTITY" \
  "$APP"

echo "==> 验签（--deep --strict）"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "==> spctl 评估（预期：无 Developer ID Application 时不通过——公证残留，见 DISTRIBUTION.md）"
if spctl -a -vv "$APP"; then
  echo "    spctl 通过（有 Developer ID）"
else
  echo "    spctl 不通过（非 Developer ID 签名）：已如实记录为分发残留"
fi

echo "==> bundle 内容清点（应只有 MacOS/ninja 与 Resources/AppIcon.icns；无 ninja-preview/ninja-theme）"
find "$APP" -type f | sort

echo "完成：${APP}（身份：${IDENTITY}）"
