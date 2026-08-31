#!/usr/bin/env bash
# Compile vendored xterm-ghostty terminfo into $1/terminfo.
#
# libghostty Exec.zig (termio/Exec.zig): if GHOSTTY_RESOURCES_DIR is set,
# TERM=xterm-ghostty and TERMINFO=<dirname(resources)>/terminfo.
# Ghostty.app layout is Resources/{ghostty,terminfo}; ninja matches that.
# The embed zig build (-Dapp-runtime=none) skips GhosttyResources, so this
# step is required for both cargo-run (out/share) and the .app bundle.
set -euo pipefail
cd "$(dirname "$0")"
DEST="${1:?destination directory (share/ or Contents/Resources)}"
SRC="$PWD/xterm-ghostty.terminfo"
[[ -f "$SRC" ]] || {
	echo "install-terminfo: missing $SRC" >&2
	exit 1
}
mkdir -p "$DEST/terminfo"
tic -x -o "$DEST/terminfo" "$SRC"
[[ -f "$DEST/terminfo/78/xterm-ghostty" ]] || {
	echo "install-terminfo: tic did not emit 78/xterm-ghostty under $DEST/terminfo" >&2
	exit 1
}
echo "install-terminfo: $DEST/terminfo (xterm-ghostty + ghostty alias)"
