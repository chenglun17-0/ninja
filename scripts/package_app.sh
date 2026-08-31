#!/usr/bin/env bash
# q4 签名打包：release 构建宿主 → 程序化图标（make_icon.sh）→
# 组 Ninja.app（Info.plist 最小键集 + CFBundleIconFile + ghostty 资源）→
# 用本机可用代码签名身份对 .app 整体 codesign。
#
# 规则（对应 DISTRIBUTION.md）：
# - 只打 ninja 宿主：bundle 不含 ninja-preview / ninja-theme（PRODUCT：
#   默认零插件，分发物不含任何官方插件——主题插件也不进 DMG，换主题 =
#   用户本地装插件，见 DISTRIBUTION.md）。
# - 身份动态解析（security find-identity -v -p codesigning），优先
#   Developer ID Application，其次 Apple Development；一个身份都没有
#   = 直接失败，绝不静默回落 adhoc（假分发）。2026-08-31 用户决策：不购
#   99 刀 Developer ID、不做公证——本机仅 Apple Development 是预期常态。
# - ghostty 主题资源（vendored 补丁 0002 装出，574 主题）是**资源不是
#   插件**，随包进 Contents/Resources/ghostty；宿主 ensure_resources_dir
#   按 bundle 相对路径优先解析（分发机上烘入的绝对开发路径不存在）。
# - xterm-ghostty terminfo 随包进 Contents/Resources/terminfo（与 ghostty
#   资源兄妹目录）。libghostty 据此设 TERM/TERMINFO；缺则 zsh zle 重绘乱字。
# - shell-integration 随包进 Contents/Resources/ghostty/shell-integration
#   （embed 构建不跑 GhosttyResources）。缺则 OSC-7 不来，相对路径 ⌘+click
#   无法解析。
# - 产物落 dist/（已 .gitignore）。DMG 见 scripts/package_dmg.sh。
#
# 用法：scripts/package_app.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

APP_NAME="Ninja"
BUNDLE_ID="dev.ninja.ninja" # 钉死：同时是 codesign --identifier（签名稳定标识）
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
GHOSTTY_RES="$ROOT/vendor/ghostty/out/share/ghostty"

# version 单源 = workspace Cargo.toml [workspace.package]（cask / DMG 文件
# 名 / Info.plist 全部由此派生，见 scripts/package_dmg.sh）。
VERSION="$(awk '/^\[workspace\.package\]/{f=1} f && /^version[[:space:]]*=/{gsub(/[\" ]/,"",$3); print $3; exit}' Cargo.toml)"
[[ -n "$VERSION" ]] || {
	echo "错误：读不到 workspace version（Cargo.toml）" >&2
	exit 1
}

echo "==> [1/6] cargo build --release -p ninja（仅宿主；ninja-preview/ninja-theme 不进 bundle）"
cargo build --release -p ninja

# LSMinimumSystemVersion 取证依据：产物二进制的 LC_BUILD_VERSION minos
#（rustc 对 aarch64-apple-darwin 的默认部署目标 = 11.0；objc2 0.6 /
# app-kit 0.3 声明的支持下限比它宽，但低于 minos 的声明是谎——dyld 在
# 更旧的系统上拒载该二进制。实机：macOS 26.6.1 arm64）。
MIN_SYS_VER="$(otool -l target/release/ninja |
	awk '/LC_BUILD_VERSION/{f=1} f && /minos/{print $2; exit}')"
if [[ -z "$MIN_SYS_VER" ]]; then
	echo "错误：读不到二进制 LC_BUILD_VERSION minos（otool）" >&2
	exit 1
fi
echo "    LSMinimumSystemVersion = ${MIN_SYS_VER}（二进制 minos，见脚本注释）"

echo "==> [2/6] 组 $APP 骨架 + 程序化图标（Resources/AppIcon.icns）"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/ninja "$APP/Contents/MacOS/ninja"
# 图标是资源不是插件（「默认零插件」不变）：Swift/CoreGraphics 矢量绘制
# 10 尺寸（16…1024，含 @2x）→ iconutil 合 icns。make_icon.sh 内建
# 像素级回归自检，失败即中止打包——不产出无图标/坏图标的分发物。
scripts/make_icon.sh "$APP/Contents/Resources/AppIcon.icns"

echo "==> [3/6] ghostty 主题资源随包 → Contents/Resources/ghostty"
# 分发机没有开发树：vendored 主题资源必须进 bundle（资源不是插件，零
# 插件约束不变）。宿主 resolve bundle 相对（Contents/Resources/ghostty）
# 优先于 build.rs 烘入的绝对开发路径（config.rs ensure_resources_dir）。
[[ -d "$GHOSTTY_RES/themes" ]] || {
	echo "错误：$GHOSTTY_RES/themes 不存在（先跑 vendor/ghostty/build.sh）" >&2
	exit 1
}
ditto "$GHOSTTY_RES" "$APP/Contents/Resources/ghostty"
THEME_N="$(find "$APP/Contents/Resources/ghostty/themes" -type f | wc -l | tr -d ' ')"
echo "    themes：${THEME_N} 个文件（开发树同源）"
# shell-integration：Exec.zig 从 $GHOSTTY_RESOURCES_DIR/shell-integration
# 注入 zsh/bash 的 OSC-7。embed 构建不跑 GhosttyResources，必须从源树补进。
SHELL_INT="$ROOT/vendor/ghostty/src/src/shell-integration"
[[ -d "$SHELL_INT/zsh" ]] || {
	echo "错误：$SHELL_INT/zsh 不存在" >&2
	exit 1
}
ditto "$SHELL_INT" "$APP/Contents/Resources/ghostty/shell-integration"
echo "    shell-integration：$(ls "$APP/Contents/Resources/ghostty/shell-integration" | tr '\n' ' ')"
# terminfo 与 ghostty 资源兄妹目录（Exec.zig TERMINFO=dirname(resources)/terminfo）。
# 缺这一步时 TERM=xterm-ghostty 但库找不到，zsh-autosuggestions/syntax-highlighting
# 重绘光标失败，打 ls 会显示成 ~ llsls。
"$ROOT/vendor/ghostty/install-terminfo.sh" "$APP/Contents/Resources"

echo "==> [4/6] Info.plist（最小键集 + CFBundleIconFile）"

cat >"$APP/Contents/Info.plist" <<EOF
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
	echo "错误：Resources/AppIcon.icns 缺失" >&2
	exit 1
}
[[ "$(plutil -extract CFBundleIconFile raw "$APP/Contents/Info.plist")" == "AppIcon.icns" ]] || {
	echo "错误：Info.plist CFBundleIconFile ≠ AppIcon.icns" >&2
	exit 1
}
[[ "$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")" == "$VERSION" ]] || {
	echo "错误：Info.plist CFBundleShortVersionString ≠ $VERSION" >&2
	exit 1
}

echo "==> [5/6] 解析签名身份（security find-identity；缺真实身份 = 失败）"
IDENTITIES="$(security find-identity -v -p codesigning |
	sed -n 's/^[[:space:]]*[0-9]*)[[:space:]]*[A-Fa-f0-9]* "\(.*\)"$/\1/p' ||
	true)"
if [[ -z "$IDENTITIES" ]]; then
	echo "错误：钥匙串里没有可用代码签名身份。不打包 adhoc 副本（假分发）。" >&2
	exit 1
fi
IDENTITY="$(grep -m1 '^Developer ID Application' <<<"$IDENTITIES" || true)"
if [[ -z "$IDENTITY" ]]; then
	IDENTITY="$(head -n1 <<<"$IDENTITIES")"
fi
echo "    身份：$IDENTITY"

echo "==> [6/6] codesign（identifier=${BUNDLE_ID}，hardened runtime）"
# --options runtime：hardened runtime（公证硬性要求，本机不公证但保持
# 同形——将来补 Developer ID 即可走 notarytool，无需重打签名策略）；
# 宿主无 JIT/无动态库插件，运行时不受限。adhoc linker 签名被本步覆盖。
codesign --force \
	--identifier "$BUNDLE_ID" \
	--options runtime \
	--sign "$IDENTITY" \
	"$APP"

echo "==> 验签（--deep --strict）"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "==> spctl 评估（2026-08-31 决策：不购 Developer ID、不公证——预期不通过，如实记录，见 DISTRIBUTION.md）"
if spctl -a -vv "$APP"; then
	echo "    spctl 通过（有 Developer ID——非预期但如实记录）"
else
	echo "    spctl 不通过（Apple Development 签名，非 Developer ID）：按决策如实记录"
fi

echo "==> bundle 内容清点（零插件断言：无 ninja-preview/ninja-theme）"
find "$APP" -type f -not -path '*/Resources/ghostty/*' | sort
[[ ! -e "$APP/Contents/MacOS/ninja-preview" ]] || {
	echo "错误：bundle 进了 ninja-preview" >&2
	exit 1
}
[[ ! -e "$APP/Contents/MacOS/ninja-theme" ]] || {
	echo "错误：bundle 进了 ninja-theme" >&2
	exit 1
}
[[ "$THEME_N" -gt 500 ]] || {
	echo "错误：主题资源异常少（${THEME_N}）" >&2
	exit 1
}
[[ -f "$APP/Contents/Resources/terminfo/78/xterm-ghostty" ]] || {
	echo "错误：bundle 缺 terminfo/78/xterm-ghostty" >&2
	exit 1
}
[[ -e "$APP/Contents/Resources/ghostty/shell-integration/zsh" ]] || {
	echo "错误：bundle 缺 shell-integration/zsh" >&2
	exit 1
}

echo "完成：${APP}（version=${VERSION}，身份：${IDENTITY}）"
