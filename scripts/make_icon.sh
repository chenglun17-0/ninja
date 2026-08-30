#!/usr/bin/env bash
# 从 assets/icon-source.png 生成 Ninja 应用图标：
#   scripts/icon_from_png.swift（aspect-fill + macOS 圆角 alpha 蒙版，
#   高质量降采样）→ 10 个标准尺寸 PNG → iconutil 合成 .icns。
# 源图 2026-08-30 由用户选定（ChatGPT 生成，1254×1254）。
#
# 内建回归自检（失败即非零退出，不产出坏图标）：
#   1) iconset 10 个 PNG 齐且像素尺寸与文件名一致（sips）；
#   2) 像素采样（icon_from_png.swift --sample）：圆角外透明
#      （蒙版真的套上了）、中心不透明且非全黑（内容真的画上了），
#      32px 小尺寸同样验证（Retina 16pt 实际资产）；
#   3) iconutil 出 .icns 后回读 iconset，条目数守恒。
#
# 用法：scripts/make_icon.sh <output.icns>
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
SWIFT_ICON="$ROOT/scripts/icon_from_png.swift"
SOURCE="$ROOT/assets/icon-source.png"

OUT="${1:?用法: scripts/make_icon.sh <output.icns>}"
ICONSET="$ROOT/target/AppIcon.iconset"

# 期望：iconset 标准名 ↔ 像素尺寸（@2x 覆盖 64/256/512/1024）
declare -a NAMES=(icon_16x16 icon_16x16@2x icon_32x32 icon_32x32@2x \
                  icon_128x128 icon_128x128@2x icon_256x256 icon_256x256@2x \
                  icon_512x512 icon_512x512@2x)
declare -a PX=(16 32 32 64 128 256 256 512 512 1024)

[[ -f "$SOURCE" ]] || { echo "错误：源图不存在 $SOURCE" >&2; exit 1; }

echo "==> [1/4] 源图 → iconset（Swift/CoreGraphics）→ $ICONSET"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
swift "$SWIFT_ICON" "$SOURCE" "$ICONSET"

echo "==> [2/4] 自检：文件数/像素尺寸"
[[ $(find "$ICONSET" -name '*.png' | wc -l | tr -d ' ') -eq 10 ]] || {
  echo "错误：iconset 应 10 个 PNG" >&2; exit 1; }
for i in "${!NAMES[@]}"; do
  f="$ICONSET/${NAMES[$i]}.png"; want=${PX[$i]}
  got="$(sips -g pixelWidth -g pixelHeight "$f" 2>/dev/null \
    | awk '/pixel/{print $2}' | paste -sd, -)"
  [[ "$got" == "$want,$want" ]] || {
    echo "错误：${NAMES[$i]}.png 期望 ${want}×${want}，实际 $got" >&2; exit 1; }
done

echo "==> [3/4] 自检：圆角蒙版与内容采样"
# 采样输出 "#RRGGBB a=NNN"（y 从顶部数），内联调用 icon_from_png.swift --sample。
BIG="$ICONSET/icon_512x512@2x.png"
# 圆角外必须透明
for pt in "6 6" "1018 6" "6 1018" "1018 1018"; do
  set -- $pt
  got="$(swift "$SWIFT_ICON" --sample "$BIG" "$1" "$2")"
  [[ "$got" == *"a=0" ]] || {
    echo "错误：1024 图 ($1,$2) 圆角外应透明，实际 $got（蒙版未生效？）" >&2; exit 1; }
done
# 中心必须不透明且非全黑（内容画上了）
got="$(swift "$SWIFT_ICON" --sample "$BIG" 512 512)"
[[ "$got" == *"a=255" && "$got" != "#000000 a=255" ]] || {
  echo "错误：1024 图中心内容缺失（$got）" >&2; exit 1; }
# 32px（=icon_16x16@2x，Retina 16pt 实际资产）：角透明 + 中心有内容
SMALL="$ICONSET/icon_16x16@2x.png"
got="$(swift "$SWIFT_ICON" --sample "$SMALL" 1 1)"
[[ "$got" == *"a=0" ]] || {
  echo "错误：32px 角不透明（$got）" >&2; exit 1; }
got="$(swift "$SWIFT_ICON" --sample "$SMALL" 16 16)"
[[ "$got" == *"a=255" ]] || {
  echo "错误：32px 中心无内容（$got）" >&2; exit 1; }

echo "==> [4/4] iconutil → ${OUT}；回读自检"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
iconutil -c icns -o "$OUT" "$ICONSET"
RT="$(mktemp -d)/rt.iconset"
iconutil -c iconset -o "$RT" "$OUT"
[[ $(find "$RT" -name '*.png' | wc -l | tr -d ' ') -eq 10 ]] || {
  echo "错误：icns 回读 iconset 不是 10 个 PNG（坏 icns）" >&2; exit 1; }
rm -rf "$(dirname "$RT")"

echo "完成：${OUT}（10 尺寸，16…1024）"
