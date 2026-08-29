#!/usr/bin/env bash
# p7：把 scripts/package_app.sh 产出的 dist/Ninja.app 打成 DMG（拖拽
# 安装：staging 里附 /Applications 符号链接）。DMG 内的 .app 保持
# 原签名（拷贝不重签）；本脚本附卷自检（挂载 → codesign --verify →
# 卸载）。
#
# 用法：scripts/package_dmg.sh   （先跑 scripts/package_app.sh）
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

DIST="$ROOT/dist"
APP="$DIST/Ninja.app"
[[ -d "$APP" ]] || { echo "错误：$APP 不存在，先跑 scripts/package_app.sh" >&2; exit 1; }

VERSION="$(defaults read "$APP/Contents/Info" CFBundleShortVersionString)"
DMG="$DIST/Ninja-$VERSION-arm64.dmg"
STAGE="$DIST/dmg-stage"

echo "==> 组 staging（Ninja.app + /Applications 链接，拖拽安装）"
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/Ninja.app"
ln -s /Applications "$STAGE/Applications"

echo "==> hdiutil create（UDZO）"
hdiutil create -volname "Ninja" -srcfolder "$STAGE" -format UDZO -ov "$DMG" >/dev/null
rm -rf "$STAGE"

echo "==> 自检：挂载 → 验签 → 清点 → 卸载"
MNT="/tmp/ninja-dmg-check.$$.mnt"   # 不存在的路径：hdiutil 直接在此挂载
hdiutil attach -nobrowse -readonly -mountpoint "$MNT" "$DMG" >/dev/null
codesign --verify --deep --strict --verbose=2 "$MNT/Ninja.app"
find "$MNT" -mindepth 1 -maxdepth 1 | sort
[[ -f "$MNT/Ninja.app/Contents/MacOS/ninja" ]] || { echo "错误：DMG 内缺宿主二进制" >&2; hdiutil detach "$MNT" >/dev/null; exit 1; }
[[ ! -e "$MNT/Ninja.app/Contents/MacOS/ninja-preview" ]] || { echo "错误：DMG 内进了 ninja-preview（违反默认零插件）" >&2; hdiutil detach "$MNT" >/dev/null; exit 1; }
[[ ! -e "$MNT/Ninja.app/Contents/MacOS/ninja-theme" ]] || { echo "错误：DMG 内进了 ninja-theme（违反默认零插件；T2 主题插件也不随分发物）" >&2; hdiutil detach "$MNT" >/dev/null; exit 1; }
# 图标卷内生效断言（回归）：icns 资源与 plist 引用都要在拖拽安装的
# 副本里——丢任一侧 = DMG 里 Finder 回退通用图标。
[[ -f "$MNT/Ninja.app/Contents/Resources/AppIcon.icns" ]] || {
  echo "错误：DMG 卷内缺 Resources/AppIcon.icns" >&2; hdiutil detach "$MNT" >/dev/null; exit 1; }
[[ "$(defaults read "$MNT/Ninja.app/Contents/Info" CFBundleIconFile)" == "AppIcon.icns" ]] || {
  echo "错误：DMG 卷内 Info.plist CFBundleIconFile ≠ AppIcon.icns" >&2; hdiutil detach "$MNT" >/dev/null; exit 1; }
hdiutil detach "$MNT" >/dev/null

echo "完成：$DMG"
echo "安装：双击挂载 → 把 Ninja.app 拖进 Applications → 弹出卷（公证残留见 DISTRIBUTION.md）"
