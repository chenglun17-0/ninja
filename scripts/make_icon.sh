#!/usr/bin/env bash
# 程序化生成 Ninja 应用图标：scripts/make_icon.swift（CoreGraphics 矢量
# 绘制，One Dark Pro 深底 + 忍者头：头带/眼缝/双眼）→ 10 个标准尺寸
# PNG → iconutil 合成 .icns。
#
# 内建回归自检（失败即非零退出，不产出坏图标）：
#   1) iconset 10 个 PNG 齐且像素尺寸与文件名一致（sips）；
#   2) 像素级采样（make_icon.swift --sample）：透明圆角、底色、兜帽、
#      头带、眼缝、双眼、飘带特征色字节保真落位（色彩空间回归）；
#   3) 32px（Retina 16pt 实际资产）双眼存活性采样；
#   4) iconutil 出 .icns 后回读 iconset，条目数守恒。
#
# 用法：scripts/make_icon.sh <output.icns>
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
SWIFT_ICON="$ROOT/scripts/make_icon.swift"

OUT="${1:?用法: scripts/make_icon.sh <output.icns>}"
ICONSET="$ROOT/target/AppIcon.iconset"

# 期望：iconset 标准名 ↔ 像素尺寸（@2x 覆盖 64/256/512/1024）
declare -a NAMES=(icon_16x16 icon_16x16@2x icon_32x32 icon_32x32@2x \
                  icon_128x128 icon_128x128@2x icon_256x256 icon_256x256@2x \
                  icon_512x512 icon_512x512@2x)
declare -a PX=(16 32 32 64 128 256 256 512 512 1024)

echo "==> [1/4] 矢量绘制 iconset（Swift/CoreGraphics）→ $ICONSET"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
swift "$SWIFT_ICON" "$ICONSET"

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

echo "==> [3/4] 自检：特征色采样（色彩空间与几何回归）"
# 1024 图上采样（y 从顶部数）。颜色字节精确——不精确=CGColor 空间陷阱
# 回归（见 make_icon.swift 头注释）或几何被动过。
sample() { # <png> <x> <y> <期望 "#RRGGBB a=NNN">
  local got
  got="$(swift "$SWIFT_ICON" --sample "$1" "$2" "$3")"
  [[ "$got" == "$4" ]] || {
    echo "错误：$1 ($2,$3) 期望 $4，实际 $got" >&2; exit 1; }
}
BIG="$ICONSET/icon_512x512@2x.png"
sample "$BIG"   6   6 '#000000 a=0'      # 圆角外透明（圆角方形，非满幅方块）
sample "$BIG" 512  64 '#282C34 a=255'    # 底：ODP 深底
sample "$BIG" 512 250 '#333842 a=255'    # 兜帽剪影
sample "$BIG" 512 404 '#E06C75 a=255'    # 头带（ODP 红，在头部上半）
sample "$BIG" 512 615 '#14161B a=255'    # 眼缝开口（头带之下）
sample "$BIG" 412 615 '#61AFEF a=255'    # 左眼（ODP 蓝）
sample "$BIG" 900 374 '#E06C75 a=255'    # 右上飘带
# 32px（=icon_16x16@2x，Retina 16pt 实际资产）：双眼必须存活。
# 眼宽 ~3.75px，采样眼中心整像素（1024 空间右眼心 (612,615) → (19,19)）。
sample "$ICONSET/icon_16x16@2x.png" 19 19 '#61AFEF a=255'

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
