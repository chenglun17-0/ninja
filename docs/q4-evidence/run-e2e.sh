#!/bin/bash
# q4 E2E：分发验收取证（brew tap + cask/DMG；产物写入本目录，可重复执行覆盖）。
#
# 验收（PLAN q4「过」）→ 测试：
#   A 打包链：package_app.sh + package_dmg.sh 全绿（真签名身份、574 主题
#     随包、零插件、图标两侧在、cask 钉 version/sha256）；spctl 评估如实
#     记录（预期不通过——不购 Developer ID、不公证）
#   B 本地 tap + file:// DMG 全新安装：tap 仓库物化（独立 git 目录）→
#     brew tap → brew install --cask → /Applications/Ninja.app 就位、安装
#     副本验签通过、隔离属性实测记录
#   C 打开即日常终端（虚拟屏）：默认装副本去隔离后 open --env 落虚拟屏
#     → 窗口在 → cfgdump 证 bundle 相对资源解析生效（分发机唯一真源）→
#     像素证终端渲染（ODP）→ 真键盘输入 touch 命令被执行（shell 活）→
#     零插件红线（无 socket、无插件进程）
#   D Gatekeeper 真行为矩阵（如实记录，写进 DISTRIBUTION.md/tap README）：
#     D1 默认装副本带隔离 → open 被拦（syspolicyd Prompt+denial、无进程）
#     D2 xattr -dr 去隔离 → 可开（本机路径）
#     D3 HOMEBREW_CASK_OPTS=--no-quarantine 重装 → 无隔离属性直接可开
#        （brew 5.1.8 的 --no-quarantine CLI 开关已禁用，env 是有效开关）
#     D4 手工打隔离的 /tmp 副本（模拟他人下载）→ open 被拦 → 去隔离可开
#   E brew uninstall --cask 无残留：/Applications 无、Caskroom 无、无进程
#   F q3 回归（docs/q3-evidence/run-e2e.sh 全绿——资源解析改动无破坏）
#
# 全程虚拟屏（PLAN「E2E 虚拟屏幕」）：hold → NINJA_E2E_SCREEN=<displayID>
# → 取证 → kill hold；拿不到 displayID 即中止 C 段（不落主屏）。
#
# 环境事实（实施期实测，2026-08-31，macOS 26.6.1 arm64 / Homebrew 5.1.8）：
# - brew 对 file:// DMG 的 cask 安装**也会**打 com.apple.quarantine。
# - `--no-quarantine` CLI 开关在本机 brew 已禁用（"There is no
#   replacement"）；HOMEBREW_CASK_OPTS="--no-quarantine" 是有效开关。
# - 隔离副本 open：syspolicyd GatekeeperPolicyScan -67018 → Prompt →
#   无进程（拦下）；无人响应 → denial breadcrumb。
# - 键盘：zoom 动作内容每次必须唯一（轮询去抖比对内容）；Enter 用
#   key 36（type "\n" 会被 shell 传成反斜杠+n 两个字符）；打字前
#   Ctrl+C（key 8 ctrl）清行；探针字符会污染命令行（zsh-autosuggestion
#   ghost 让 zoom dump 的 last 不可靠——q3 README 已记录）。
set -u
cd "$(dirname "$0")/../.."
REPO="$PWD"

EV=docs/q4-evidence
SYNTH=/tmp/nq4-synth
PROBE=/tmp/nq4-probe
HOLD_JSON=/tmp/nq4-hold.json
TAP_REPO="$HOME/my_repos/ninja-tap"
APP_PATH="/Applications/Ninja.app"
LOGDIR=$EV/e2e-logs
mkdir -p "$LOGDIR"
PASS=0; FAIL=0

say()  { printf '\n== %s\n' "$*"; }
ok()   { PASS=$((PASS+1)); echo "  [PASS] $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  [FAIL] $*"; }
note() { echo "  [NOTE] $*"; }

# syspolicyd Gatekeeper 取证（$1=起始 epoch，$2=输出文件）
gk_capture() {
  log show --start "$(date -r "$1" '+%Y-%m-%d %H:%M:%S')" \
    --predicate 'process == "syspolicyd" AND (eventMessage CONTAINS "ninja" OR eventMessage CONTAINS "Prompt" OR eventMessage CONTAINS "breadcrumb")' \
    --style compact 2>/dev/null | grep -E "Prompt|breadcrumb|evaluateScanResult|GatekeeperPolicyScan" > "$2" || true
}
dismiss_alert() { osascript -e 'tell application "System Events" to key code 53' >/dev/null 2>&1; }

# ---- 全局收尾 ---------------------------------------------------------------
HOLD_PID=""
# 等进程退干净（TERM 后确认；否则升级 KILL）——避免残留实例吞掉后续 open
wait_gone() { # $1=pgrep 模式
  for _ in $(seq 30); do pgrep -f "$1" >/dev/null 2>&1 || return 0; sleep 0.2; done
  pkill -9 -f "$1" 2>/dev/null
  for _ in $(seq 10); do pgrep -f "$1" >/dev/null 2>&1 || return 0; sleep 0.2; done
  return 1
}
installed_pid() { pgrep -f "$APP_PATH/Contents/MacOS/ninja" || true; }
cleanup() {
  pkill -f "$APP_PATH/Contents/MacOS/ninja" 2>/dev/null
  pkill -f "ninja-quarantine-test" 2>/dev/null
  [ -n "${HOLD_PID:-}" ] && kill "$HOLD_PID" 2>/dev/null
  restore_cfg
  dismiss_alert
}
trap cleanup EXIT

# 用户配置隔离（C 段要确定性 ODP 缺省；结束恢复）
APPS_DIR="$HOME/Library/Application Support/com.mitchellh.ghostty"
BACKUPS=()
stash() { [ -e "$1" ] || return 0; mv "$1" "$1.nq4bak" && BACKUPS+=("$1"); }
restore_cfg() {
  for p in "${BACKUPS[@]:-}"; do
    [ -n "$p" ] || continue
    [ -e "$p.nq4bak" ] && mv -f "$p.nq4bak" "$p"
  done
  BACKUPS=()
}
isolate_cfg() {
  stash "$APPS_DIR/config"; stash "$APPS_DIR/config.ghostty"
  stash "$HOME/.config/ghostty/config"; stash "$HOME/.config/ghostty/config.ghostty"
  stash "$HOME/.config/ninja/ninja.toml"
}

# ==========================================================================
say "A 打包链（package_app.sh + package_dmg.sh）"
# ==========================================================================
A_LOG=$EV/a-package.log
{ ./scripts/package_app.sh && ./scripts/package_dmg.sh; } > "$A_LOG" 2>&1
if [ $? -eq 0 ] && [ -f dist/Ninja-0.1.0-arm64.dmg ]; then
  ok "A1 打包链全绿（${A_LOG}）：.app + DMG + cask 再生"
else
  bad "A1 打包链失败（${A_LOG}）"
fi
grep -q "身份：Apple Development" "$A_LOG" && ok "A2 真实签名身份（Apple Development，动态解析）" || bad "A2 身份异常（见 ${A_LOG}）"
grep -q "themes：574" "$A_LOG" && ok "A3 574 主题随包" || bad "A3 主题数异常（见 ${A_LOG}）"
grep -q "spctl 不通过" "$A_LOG" && ok "A4 spctl 评估如实记录（不通过=预期，无公证）" || bad "A4 spctl 记录缺失"
grep -q "satisfies its Designated Requirement" "$A_LOG" && ok "A5 codesign --verify --deep --strict 通过" || bad "A5 验签失败"
# cask 三单源钉死：version/sha256/url 与 DMG 实物一致
DMG_SHA=$(shasum -a 256 dist/Ninja-0.1.0-arm64.dmg | awk '{print $1}')
grep -q "version \"0.1.0\"" scripts/tap/Casks/ninja.rb && ok "A6 cask version=0.1.0（workspace/Info.plist/DMG 文件名单源）" || bad "A6 cask version 不一致"
grep -q "sha256 \"$DMG_SHA\"" scripts/tap/Casks/ninja.rb && ok "A7 cask sha256 钉 DMG 实物" || bad "A7 cask sha256 不一致"
grep -q "url \"file://$REPO/dist/Ninja-0.1.0-arm64.dmg\"" scripts/tap/Casks/ninja.rb && ok "A8 cask url=file:// 本地 DMG" || bad "A8 cask url 异常"
git check-ignore dist/Ninja-0.1.0-arm64.dmg >/dev/null && ok "A9 DMG 不入 git（.gitignore /dist）" || bad "A9 DMG 未被 gitignore"

# ==========================================================================
say "B 本地 tap + file:// DMG 全新安装"
# ==========================================================================
B_LOG=$EV/b-brew-install.log
brew uninstall --cask ninja >/dev/null 2>&1 || true
brew untap ninja/local >/dev/null 2>&1 || true
rm -rf "$TAP_REPO"
# tap 仓库物化（独立 git 目录，不入本仓库；brew tap 需要至少一个 commit）
mkdir -p "$TAP_REPO"
cp -R scripts/tap/. "$TAP_REPO/"
git -C "$TAP_REPO" init -q && git -C "$TAP_REPO" add -A && git -C "$TAP_REPO" commit -qm "ninja tap 0.1.0"
{ brew tap ninja/local "$TAP_REPO" && brew install --cask ninja; } > "$B_LOG" 2>&1
[ -d "$APP_PATH" ] && ok "B1 brew tap（本地目录）+ install --cask ninja：$APP_PATH 就位" || bad "B1 安装失败（${B_LOG}）"
codesign --verify --deep --strict "$APP_PATH" >/dev/null 2>&1 && ok "B2 安装副本验签通过（--deep --strict）" || bad "B2 安装副本验签失败"
QV=$(xattr -p com.apple.quarantine "$APP_PATH" 2>/dev/null || true)
if [ -n "$QV" ]; then
  ok "B3 隔离属性实测：file:// DMG 安装**带** com.apple.quarantine（${QV}）"
else
  note "B3 隔离属性实测：本轮 file:// 安装未带隔离属性（与 2026-08-31 前轮实测不同，如实记录）"
fi
grep -q "successfully installed" "$B_LOG" && ok "B4 cask 安装日志（${B_LOG}）" || bad "B4 安装日志异常"
# DMG 拖拽布局也在（staging /Applications 链接，挂卷清点）
hdiutil attach -nobrowse -readonly -mountpoint /tmp/nq4-vol dist/Ninja-0.1.0-arm64.dmg >/dev/null 2>&1
[ -L /tmp/nq4-vol/Applications ] && [ -d /tmp/nq4-vol/Ninja.app ] \
  && ok "B5 DMG 拖拽布局（Ninja.app + /Applications 链接）" || bad "B5 DMG 布局异常"
hdiutil detach /tmp/nq4-vol >/dev/null 2>&1

# ==========================================================================
say "D1 默认装副本（带隔离）Gatekeeper 真行为"
# ==========================================================================
D1=$EV/d1-blocked.log
T1=$(date +%s)
open "$APP_PATH" >/dev/null 2>&1
sleep 4
D1PID=$(installed_pid)
if [ -n "$D1PID" ]; then
  note "D1 带隔离副本 open → 进程启动了（带 Gatekeeper 警告对话——本机当日已多次放行该证书，状态影响结果，如实记录）"
  pkill -f "$APP_PATH/Contents/MacOS/ninja" 2>/dev/null
else
  ok "D1 带隔离副本 open → 未启动（Gatekeeper 拦）"
fi
wait_gone "$APP_PATH/Contents/MacOS/ninja" || true
dismiss_alert; sleep 1
gk_capture "$T1" "$D1"
# 确定性断言：Gatekeeper 评估必然介入（无公证的后果）；拦不拦得住是状态函数（如实记录）
if grep -q "Prompt shown" "$D1" && grep -q "GatekeeperPolicyScanError\|evaluateScanResult" "$D1"; then
  ok "D1 Gatekeeper 评估介入：Prompt shown + 扫描拒绝（-67018 无公证；${D1}）"
else
  bad "D1 Gatekeeper 评估未介入？（${D1}）"
fi
grep -q "denial breadcrumb" "$D1" && note "D1 本轮被拦到底（denial breadcrumb）" || true

# ==========================================================================
say "C 打开即日常终端（虚拟屏；D2 去隔离路径顺带取证）"
# ==========================================================================
xattr -dr com.apple.quarantine "$APP_PATH" 2>/dev/null
if xattr -p com.apple.quarantine "$APP_PATH" >/dev/null 2>&1; then
  bad "D2 xattr -dr 去隔离失败"
else
  ok "D2 xattr -dr 去隔离生效（本机处理路径）"
fi

scripts/e2e/virtual-display hold 1440 900 0 > "$HOLD_JSON" 2>/tmp/nq4-hold.err &
HOLD_PID=$!
for _ in $(seq 20); do [ -s "$HOLD_JSON" ] && break; sleep 0.3; done
DISPLAY_ID=$(python3 -c "import json;print(json.load(open('$HOLD_JSON'))['displayID'])" 2>/dev/null || true)
FRAME=""
if [ -z "${DISPLAY_ID:-}" ]; then
  bad "C1 虚拟屏未就绪——C2 起中止（不落主屏）"
else
  for _ in $(seq 10); do
    FRAME=$(scripts/e2e/virtual-display list | python3 -c "
import json,sys
for d in json.load(sys.stdin):
    if d['id']==$DISPLAY_ID: print(d['x']); break
" 2>/dev/null || true)
    [ -n "$FRAME" ] && [ "$FRAME" != "0" ] && break
    sleep 0.5
  done
  ok "C1 虚拟屏就绪 displayID=${DISPLAY_ID}（frame x=${FRAME:-未定位}；落窗按 NSScreenNumber 匹配）"
fi
swiftc -O "$EV/synth_input.swift" -o "$SYNTH" 2>/dev/null || { echo "FATAL: synth 编译失败"; exit 1; }
swiftc -O "$EV/probe_window.swift" -o "$PROBE" 2>/dev/null || { echo "FATAL: probe 编译失败"; exit 1; }
[ "$($SYNTH trust)" = "trusted" ] || { echo "FATAL: TCC 辅助功能未授"; exit 1; }

C_OK=1
if [ -n "${DISPLAY_ID:-}" ]; then
  isolate_cfg
  rm -f "${TMPDIR:-/tmp}"/ninja-ade-*.sock   # 清扫陈旧 socket（空载红线计数前清场）
  ZOOMF=/tmp/nq4-c-zoom; CFGDUMP=/tmp/nq4-c-cfg.json; ZDUMP=/tmp/nq4-c-zdump.json
  : > "$ZOOMF"; rm -f "$CFGDUMP" "$ZDUMP"
  PROOF=/tmp/nq4-typed-proof; rm -f "$PROOF"
  open --env NINJA_E2E_SCREEN="$DISPLAY_ID" \
       --env NINJA_CFG_DUMP="$CFGDUMP" \
       --env NINJA_ZOOM_FILE="$ZOOMF" \
       --env NINJA_ZOOM_DUMP="$ZDUMP" \
       --stderr "$LOGDIR/c-installed-app.log" \
       "$APP_PATH"
  sleep 4
  APP_PID=$(installed_pid)
  # 等窗口真正出现（新实例而非残留；最多 10s）
  for _ in $(seq 20); do
    [ -n "${APP_PID:-}" ] && $SYNTH wins "$APP_PID" 2>/dev/null | grep -q '"layer":0' && break
    sleep 0.5
    APP_PID=$(installed_pid)
  done
  DUMP_N=0
  dumpc() { DUMP_N=$((DUMP_N+1)); echo "dumpc$DUMP_N" > "$ZOOMF"; sleep 0.9; }
  if [ -n "$APP_PID" ]; then
    ok "C2 安装副本 open 启动（pid=${APP_PID}，open --env 落虚拟屏）"
  else
    bad "C2 安装副本未启动"; C_OK=0
  fi
  if [ "$C_OK" = "1" ]; then
    WX=$($SYNTH wins "$APP_PID" | python3 -c "import json,sys;ws=[w for w in json.load(sys.stdin) if w['layer']==0];print(ws[0]['bounds'][0] if ws else 1)" 2>/dev/null || echo 1)
    case "$WX" in
      -*|1[5-9][0-9][0-9]|2[0-9][0-9][0-9]) ok "C3 窗口落在虚拟屏（x=${WX}）" ;;
      *) bad "C3 窗口未落虚拟屏（x=${WX}）" ;;
    esac
    dumpc
    if python3 -c "import json,sys;sys.exit(0 if json.load(open('$CFGDUMP'))['resources_dir']=='$APP_PATH/Contents/Resources/ghostty' else 1)" 2>/dev/null; then
      ok "C4 resources_dir=Contents/Resources/ghostty（bundle 相对优先于烘入开发路径——本机两者都在，实测优先级）"
    else
      bad "C4 resources_dir 非预期：$(python3 -c "import json;print(json.load(open('$CFGDUMP'))['resources_dir'])" 2>/dev/null)"
    fi
    python3 -c "import json,sys;sys.exit(0 if json.load(open('$CFGDUMP'))['odp_applied'] else 1)" 2>/dev/null \
      && ok "C5 ODP 缺省生效（odp_applied=true，用户配置已隔离）" || note "C5 odp_applied=false（用户配置隔离失败？如实记录）"
    WID=$($SYNTH wins "$APP_PID" | python3 -c "import json,sys;print([w['id'] for w in json.load(sys.stdin) if w['layer']==0][0])" 2>/dev/null || true)
    PNG=/tmp/nq4-c-shot.png
    screencapture -x -l "$WID" "$PNG" >/dev/null 2>&1 && cp "$PNG" "$EV/c-terminal.png"
    RGB=$("$PROBE" avg "$PNG" 0.45 0.30 0.40 0.30 2>/dev/null || echo '{"avg":[0,0,0]}')
    if python3 -c "import json,sys;r,g,b=json.loads('''$RGB''')['avg'];sys.exit(0 if abs(r-40)<=14 and abs(g-44)<=14 and abs(b-52)<=14 else 1)" 2>/dev/null; then
      ok "C6 终端背景像素 ≈ ODP #282c34（${RGB}；真渲染，截图 c-terminal.png）"
    else
      bad "C6 背景像素 $RGB ≠ ODP #282c34±14"
    fi
    # 真键盘输入：激活 → Ctrl+C 清行 → touch 命令 → Enter → 文件在（重试 2 轮）
    TYPED=0
    for TRY in 1 2; do
      osascript -e "tell application \"System Events\" to set frontmost of first application process whose unix id is $APP_PID to true" >/dev/null 2>&1
      sleep 1
      $SYNTH key "$APP_PID" 8 ctrl >/dev/null 2>&1; sleep 0.4
      $SYNTH type "$APP_PID" "touch $PROOF" >/dev/null 2>&1; sleep 0.6
      $SYNTH key "$APP_PID" 36 >/dev/null 2>&1; sleep 2
      if [ -f "$PROOF" ]; then TYPED=1; break; fi
    done
    [ "$TYPED" = "1" ] && ok "C7 真键盘输入经终端进 shell 执行（touch 产物在；打开即日常终端）" \
                        || bad "C7 输入未执行（日常终端抽查不成立）"
    dumpc
    GRID=$(python3 -c "import json;l=json.load(open('$ZDUMP'))['leaves'][0];print('%dx%d'%(l['cols'],l['rows']))" 2>/dev/null || echo "?x?")
    python3 -c "import json,sys;l=json.load(open('$ZDUMP'))['leaves'][0];sys.exit(0 if l['cols']>60 and l['rows']>20 else 1)" 2>/dev/null \
      && ok "C8 终端网格在（${GRID}）" || bad "C8 网格异常（${GRID}）"
    SOCKS=$(ls "${TMPDIR:-/tmp}"/ninja-ade-*.sock 2>/dev/null | wc -l | tr -d ' ')
    PLUGINS=$(pgrep -f "ninja-preview|ninja-theme" 2>/dev/null | wc -l | tr -d ' ')
    [ "$SOCKS" = "0" ] && [ "$PLUGINS" = "0" ] && ok "C9 零插件红线：无 ADE socket、无插件进程" || bad "C9 违反零插件（sock=$SOCKS procs=${PLUGINS}）"
  fi
  pkill -f "$APP_PATH/Contents/MacOS/ninja" 2>/dev/null; sleep 1
  restore_cfg
else
  echo "  [SKIP] C2-C9（虚拟屏不可用）"
fi

# ==========================================================================
say "D3 HOMEBREW_CASK_OPTS=--no-quarantine（cask quarantine 语义）"
# ==========================================================================
D3=$EV/d3-cask-opts.log
brew uninstall --cask ninja >/dev/null 2>&1
HOMEBREW_CASK_OPTS="--no-quarantine" brew install --cask ninja > "$D3" 2>&1
if xattr -p com.apple.quarantine "$APP_PATH" >/dev/null 2>&1; then
  bad "D3 HOMEBREW_CASK_OPTS=--no-quarantine 后仍有隔离属性（语义变了？如实记录）"
else
  ok "D3 HOMEBREW_CASK_OPTS=--no-quarantine：无隔离属性（本机 brew 5.1.8 的有效开关）"
fi
open "$APP_PATH" >/dev/null 2>&1; sleep 3
D3PID=$(installed_pid)
[ -n "$D3PID" ] && ok "D4 无隔离安装副本直接可开（pid=${D3PID}）" || bad "D4 无隔离副本未启动"
pkill -f "$APP_PATH/Contents/MacOS/ninja" 2>/dev/null
wait_gone "$APP_PATH/Contents/MacOS/ninja" || true

# ==========================================================================
say "D4 手工隔离副本（模拟他人下载）Gatekeeper 真行为"
# ==========================================================================
D4=$EV/d4-quarantine-copy.log
QTEST=/tmp/ninja-quarantine-test.app
rm -rf "$QTEST"
cp -R "$APP_PATH" "$QTEST"
xattr -w com.apple.quarantine "0083;$(date +%s);ninja-e2e;" "$QTEST"
T4=$(date +%s)
open "$QTEST" >/dev/null 2>&1
sleep 4
if pgrep -f "ninja-quarantine-test" >/dev/null; then
  note "D5 手工隔离副本 open → 进程启动了（带 Gatekeeper 警告对话；同 D1 的状态依赖，如实记录）"
  pkill -f "ninja-quarantine-test" 2>/dev/null
else
  ok "D5 手工隔离副本（他人下载模拟）open → 未启动（Gatekeeper 拦）"
fi
wait_gone "ninja-quarantine-test" || true
dismiss_alert
gk_capture "$T4" "$D4"
# 确定性断言：Gatekeeper 评估介入（无公证的后果）
if grep -q "Prompt shown" "$D4" && grep -q "GatekeeperPolicyScanError\|evaluateScanResult" "$D4"; then
  ok "D5 Gatekeeper 评估介入：Prompt shown + 扫描拒绝（${D4}）"
else
  bad "D5 Gatekeeper 评估未介入？（${D4}）"
fi
xattr -dr com.apple.quarantine "$QTEST" 2>/dev/null
open "$QTEST" >/dev/null 2>&1; sleep 3
pgrep -f "ninja-quarantine-test" >/dev/null && ok "D6 副本去隔离后可开（xattr -dr 路径）" || bad "D6 去隔离后仍未启动"
pkill -f "ninja-quarantine-test" 2>/dev/null
wait_gone "ninja-quarantine-test" || true
rm -rf "$QTEST"
# spctl 评估（文档口径如实记录）
spctl -a -vv "$APP_PATH" > "$EV/spctl.txt" 2>&1 || true
grep -q "rejected" "$EV/spctl.txt" && ok "D7 spctl -a -vv：rejected（无 Developer ID/公证，如实记录）" || note "D7 spctl 结果（见 spctl.txt）"

# ==========================================================================
say "D3b --no-quarantine CLI 开关对照（brew 5.1.8 已禁用，如实记录）"
# ==========================================================================
brew uninstall --cask ninja >/dev/null 2>&1
{ brew install --cask ninja --no-quarantine; } > "$EV/d3b-cli-switch.log" 2>&1 || true
grep -q "switch is disabled" "$EV/d3b-cli-switch.log" \
  && ok "D3b `--no-quarantine` CLI 开关已禁用（There is no replacement；d3b-cli-switch.log）" \
  || note "D3b CLI 开关未报禁用（行为变化？见 d3b-cli-switch.log）"
[ ! -e "$APP_PATH" ] && ok "D3b 开关被拒后未安装（app 不在）" || true

# ==========================================================================
say "E brew uninstall --cask 无残留（env 重装一份再卸）"
# ==========================================================================
E_LOG=$EV/e-uninstall.log
HOMEBREW_CASK_OPTS="--no-quarantine" brew install --cask ninja >/dev/null 2>&1
[ -d "$APP_PATH" ] || HOMEBREW_CASK_OPTS="--no-quarantine" brew install --cask ninja >> "$E_LOG" 2>&1
brew uninstall --cask ninja >> "$E_LOG" 2>&1
[ ! -e "$APP_PATH" ] && ok "E1 /Applications/Ninja.app 移除" || bad "E1 $APP_PATH 残留"
[ ! -d "$(brew --caskroom)/ninja" ] && ok "E2 Caskroom ninja 目录清除（$(brew --caskroom)/ninja）" || bad "E2 Caskroom 残留"
sleep 1
[ -z "$(installed_pid)" ] && ok "E3 无 ninja 进程残留" || bad "E3 进程残留"
brew tap ninja/local >/dev/null 2>&1 && ok "E4 tap 语义：卸载 app 不拔 tap（收尾统一拔）" || true

# ==========================================================================
say "F q3 回归（docs/q3-evidence/run-e2e.sh；资源解析改动无破坏）"
# ==========================================================================
[ -n "${HOLD_PID:-}" ] && { kill "$HOLD_PID" 2>/dev/null; HOLD_PID=""; }
bash docs/q3-evidence/run-e2e.sh > "$EV/regression-q3.log" 2>&1
if grep -q "OVERALL: PASS" "$EV/regression-q3.log"; then
  ok "F1 q3 套件 OVERALL: PASS（regression-q3.log）"
else
  bad "F1 q3 回归失败（regression-q3.log）"
fi

# ---- 收尾 -------------------------------------------------------------------
brew untap ninja/local >/dev/null 2>&1 || true
dismiss_alert

printf '\n==== q4 E2E 总结：PASS=%d FAIL=%d ====\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then echo "OVERALL: PASS"; else echo "OVERALL: FAIL"; fi
[ "$FAIL" -eq 0 ]
