#!/usr/bin/env bash
# 收 E2E 残留。workflow 被杀时 trap 跑不到，会留下：
# - virtual-display hold（隐形屏，鼠标滑进去像卡死）
# - Ninja 宿主（含 App Translocation / cargo 产物）
# 下一轮 `open` 会附着到没窗口的旧实例，看起来启动卡死。
set -u
keep_hold=0
[ "${1:-}" = "--keep-hold" ] && keep_hold=1

kill_pat() {
  local pat="$1"
  local pids
  pids=$(pgrep -f "$pat" 2>/dev/null || true)
  [ -n "$pids" ] || return 0
  # shellcheck disable=SC2086
  kill -TERM $pids 2>/dev/null || true
  sleep 0.15
  # shellcheck disable=SC2086
  kill -KILL $pids 2>/dev/null || true
}

[ "$keep_hold" = 1 ] || kill_pat 'scripts/e2e/virtual-display hold'
# 宿主：安装包、隔离副本、cargo 产物。不用裸 "ninja"（会误伤脚本/workflow）。
kill_pat 'Contents/MacOS/ninja'
kill_pat 'target/release/ninja'
kill_pat 'target/debug/ninja'
kill_pat 'ninja-preview'
kill_pat 'ninja-theme'
kill_pat '/.config/ninja/plugins/'
exit 0
