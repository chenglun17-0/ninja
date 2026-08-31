#!/bin/bash
# ninja q0: build the pinned full-embed libghostty (static, darwin) via zig.
#
# Decision (recorded in docs/Q0-CAPABILITY-AUDIT.md): route (a)
# "zig static archive" over route (b) "xcodebuild xcframework".
#   (a) one combined libghostty-internal.a linked straight into the Rust
#       host; tiny build.zig patch (patches/0001) installs it on darwin
#       (upstream only installs the libghostty-vt artifacts there);
#   (b) official macOS route is macos/Ghostty.xcodeproj -> Ghostty.xcframework
#       via xcodebuild inside zig; heavyweight (universal multi-slice, full
#       Xcode orchestration) and oriented at the Swift GUI app, not needed
#       for static linkage into a Rust binary.
#
# Toolchain is pinned with the ghostty pin: zig 0.15.2
# (build.zig.zon minimum_zig_version). Zig != 0.15.2 -> hard error.
#
# Host workaround (documented in docs/Q0-CAPABILITY-AUDIT.md): Xcode 26.6's
# SDK tbd stubs dropped "arm64-macos" (arm64e only), so zig 0.15.2 cannot
# link even `zig init && zig build` natively (its native SDK detection goes
# through xcrun and ignores SDKROOT). Fix: prepend a contained xcrun shim
# (xcrun-shim/) that answers --show-sdk-path with the newest CommandLineTools
# SDK whose libSystem.B.tbd still has arm64-macos, and pass an explicit
# -Dtarget. The shim only affects PATH inside this script.
#
# Output: out/lib/libghostty-internal.a, out/include/ghostty.h
set -euo pipefail
cd "$(dirname "$0")"

PIN=a887df42c56f6de86c0fe6da9c4eeca37931e083
ZIG_REQUIRED=0.15.2
zig_version="$(zig version 2>/dev/null || true)"
if [ "${zig_version}" != "${ZIG_REQUIRED}" ]; then
  echo "build: zig ${ZIG_REQUIRED} is required (pinned with ghostty ${PIN}), found '${zig_version}'" >&2
  exit 1
fi

# --- SDK workaround (see header comment): contained xcrun shim ---
case "$(uname -m)" in
  arm64) zig_target=aarch64-macos ;;
  x86_64) zig_target=x86_64-macos ;;
  *) echo "build: unsupported native arch $(uname -m)" >&2; exit 1 ;;
esac
default_sdk="$(xcrun --show-sdk-path 2>/dev/null || true)"
if ! awk '/^targets:/ {print; exit}' "${default_sdk}/usr/lib/libSystem.B.tbd" 2>/dev/null | grep -qw "arm64-macos"; then
  export PATH="${PWD}/xcrun-shim:${PATH}"
  echo "build: SDK '${default_sdk}' libSystem.B.tbd targets lack arm64-macos; using xcrun shim -> $(./xcrun-shim/xcrun --show-sdk-path)" >&2
fi

./fetch.sh

# Apply vendor patches once. Reverse-apply-then-forward used to duplicate
# the Darwin `else` (21 copies → zig parse error). Marker in build.zig is
# the idempotency check.
apply_once() {
  local patch_file="$1" marker="$2"
  if grep -q "$marker" src/build.zig; then
    return 0
  fi
  patch -s -N -p1 -d src --input "$patch_file"
}
apply_once "${PWD}/patches/0001-darwin-install-static-embed-lib.patch" \
  "ninja q0: install the static embed library"
apply_once "${PWD}/patches/0002-install-themes-on-embed-route.patch" \
  "ninja q2: also install the bundled themes"

mkdir -p out
( cd src && zig build \
  -Dtarget="${zig_target}" \
  -Dapp-runtime=none \
  -Demit-xcframework=false \
  -Demit-docs=false \
  -Doptimize=ReleaseFast \
  --prefix "${PWD}/../out" \
  --cache-dir .zig-cache \
  --summary all )

test -f out/lib/libghostty-internal.a
test -f out/include/ghostty.h
test -d out/share/ghostty/themes
# embed 路线不跑 GhosttyResources：terminfo 必须另装到 resources 的兄
# 弟目录，否则 PTY 的 TERMINFO 指向空目录，zsh zle/autosuggestions 光标
# 回退失败，输入看起来像 llsls。
./install-terminfo.sh "${PWD}/out/share"
test -f out/share/terminfo/78/xterm-ghostty
echo "build: out/lib/libghostty-internal.a + out/include/ghostty.h + out/share/ghostty/themes + out/share/terminfo" >&2
