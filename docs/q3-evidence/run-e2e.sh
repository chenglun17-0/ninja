#!/bin/bash
# q3 E2E：插件系统 + 三门禁验收取证（产物写入本目录，可重复执行覆盖）。
#
# 验收（PLAN q3「过」：三大门禁全部通过——空载内存对照 Ghostty 本尊、
# 第一个插件、关掉即轻）→ 测试：
#   0 协议契约 + 依赖红线（cargo test -p ninja-protocol；cargo tree -p
#     插件 crate 无宿主/ghostty-sys）
#   A 门禁一（空载内存）：ninja 空载（默认零插件、单窗）vs Ghostty 本尊
#     1.3.0 同条件各采 15 样本 ri_phys_footprint 中位数，比值 ≤1.5×；
#     附空载红线（lsof unix socket=0、pgrep 插件空）
#   B 门禁二（第一个插件）：虚拟屏真鼠标（全局 HID tap）⌘click 终端内
#     路径 → hit 广播 → claim → 层出现（像素探针验层内容含文件文本）→
#     Esc 关层焦点回终端；全程只经协议帧（NINJA_ADE_DEBUG 日志逐帧断言）
#   C 门禁三（关掉即轻）：禁用（面板钩子）/杀插件收层+色板回退/SIGKILL
#     宿主三场景——socket 消失、pgrep 插件空、无层、footprint 回空载
#     基线+容差、再启用即重拉；theme.set 应用/插件死亡回退一并取证
#   D q0 取证模式回归 PASS（--evidence-dir）
#
# 全程虚拟屏（PLAN「E2E 虚拟屏幕」）：hold → NINJA_E2E_SCREEN=<displayID>
# → 取证 → kill hold；拿不到 displayID 即中止（不落主屏）。键盘 CGEvent
# PostToPid（app 激活后投递，q1/q2 同款 + 前台激活探子）；鼠标全局 HID
# tap（PostToPid 的鼠标事件不带窗口上下文）；像素探针 = screencapture -l
# + probe_window（avg+std：层文本方差可判）。断言全部跑命令/读 JSON/读
# 像素，不信实施者的自我声明。
#
# 环境事实（实施期实测，见 docs/q3-evidence/README.md）：
# - 虚拟屏 hidpi=0 无显示色空间压暗（q2 在主屏见过 ~10%）；像素断言用
#   原始值 ±14。
# - zsh 多行 prompt：echo 输出落在第 ~3 行（banner 4 行内），点击行梯子
#   3/4/2/5/6 兜底。
# - synth type 只有小写（大写需 shift，未实现）：取证串一律小写。
set -u
cd "$(dirname "$0")/../.."

cargo build --release -p ninja -p ninja-preview -p ninja-theme >/dev/null 2>&1 || { echo "FATAL: build 失败"; exit 1; }
BIN=./target/release/ninja
EV=docs/q3-evidence
SYNTH=/tmp/nq3-synth
PROBE=/tmp/nq3-probe
SAMPLER=/tmp/nq3-sampler
GHOSTTY_BIN=/Applications/Ghostty.app/Contents/MacOS/ghostty
HOLD_JSON=/tmp/nq3-hold.json
LOGDIR=$EV/e2e-logs
mkdir -p "$LOGDIR"
PASS=0; FAIL=0
SAMPLE=/tmp/nq3p/sample.txt

say()  { printf '\n== %s\n' "$*"; }
ok()   { PASS=$((PASS+1)); echo "  [PASS] $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  [FAIL] $*"; }
assert_eq() { # desc expected actual
  if [ "$2" = "$3" ]; then ok "$1 = $3"; else bad "$1: 期望 $2 实得 $3"; fi
}
assert_json() { # file py-expr desc（expr 为 True 断言）
  local got
  got=$(python3 -c "import json;d=json.load(open('$1'));print($2)" 2>&1)
  if [ "${got:-}" = "True" ]; then ok "$3"; else bad "${3}（实得: ${got:-}）"; fi
}
zoom() { echo "$1" > "$ZOOMF"; sleep 0.8; }
dump() { zoom "dump${2:-}"; cp "$ZOUT" "$EV/$1" 2>/dev/null; }
winid()    { $SYNTH wins "$1" | python3 -c "import json,sys;print([w['id'] for w in json.load(sys.stdin) if w['layer']==0][0])"; }
winbounds(){ $SYNTH wins "$1" | python3 -c "import json,sys;print(' '.join(str(v) for v in [x for x in json.load(sys.stdin) if x['layer']==0][0]['bounds']))"; }
shot()     { [ -n "${2:-}" ] && screencapture -x -l "$2" "$EV/$1" >/dev/null 2>&1; }
probe_px() { # tag mode x0 y0 w h → 完整 JSON 进 $EV/<tag>-pixel.txt
  local wid png out
  wid=$(winid "$APP_PID") || true
  [ -n "${wid:-}" ] || { echo '{"avg":[0,0,0],"std":[0,0,0]}'; return; }
  png="/tmp/nq3-shot-$1.png"
  screencapture -x -l "$wid" "$png" >/dev/null 2>&1
  out=$("$PROBE" "$2" "$png" "$3" "$4" "$5" "$6" 2>/dev/null || echo '{"avg":[0,0,0],"std":[0,0,0]}')
  echo "$out" >> "$EV/$1-pixel.txt"
  echo "$out"
}
assert_px() { # tag desc r g b tol x0 y0 w h
  local rgb; rgb=$(probe_px "$1" avg "$7" "$8" "$9" "${10}")
  if python3 -c "import sys,json;r,g,b=json.loads('''$rgb''')['avg'];sys.exit(0 if abs(r-$3)<=$6 and abs(g-$4)<=$6 and abs(b-$5)<=$6 else 1)" 2>/dev/null; then
    ok "${2}（像素 ${rgb}，期望 [$3,$4,$5]±$6）"
  else
    bad "$2（像素 ${rgb:-空} ≠ [$3,$4,$5]±$6）"
  fi
}

# ---- 虚拟屏（拿不到 displayID 即中止，不落主屏） ---------------------------
APP_PID=""; HOLD_PID=""
APPS_DIR="$HOME/Library/Application Support/com.mitchellh.ghostty"
REAL_CFG="$APPS_DIR/config"; REAL_TPL="$APPS_DIR/config.ghostty"
BACKUPS=()
restore_cfg() {
  for pair in "${BACKUPS[@]:-}"; do
    [ -n "$pair" ] || continue
    src="${pair%%|*}"; bak="${pair##*|}"
    if [ -f "$bak" ]; then mv -f "$bak" "$src"; fi
  done
  BACKUPS=()
}
cleanup() {
  [ -n "${APP_PID:-}" ] && { kill "$APP_PID" 2>/dev/null; for _ in $(seq 20); do kill -0 "$APP_PID" 2>/dev/null || break; sleep 0.1; done; kill -9 "$APP_PID" 2>/dev/null; APP_PID=""; }
  pkill -f "target/release/ninja-preview" 2>/dev/null
  pkill -f "target/release/ninja-theme" 2>/dev/null
  [ -n "${HOLD_PID:-}" ] && kill "$HOLD_PID" 2>/dev/null
  restore_cfg
}
trap cleanup EXIT

isolate() { # 移开真实 App Support 配置（B/C 需要隔离的 XDG 配置）
  for f in "$REAL_CFG" "$REAL_TPL"; do
    if [ -f "$f" ]; then
      local bak; bak=$(mktemp /tmp/nq3-cfg.XXXXXX)
      mv "$f" "$bak"
      BACKUPS+=("$f|$bak")
    fi
  done
}

scripts/e2e/virtual-display hold 1440 900 0 > "$HOLD_JSON" 2>/tmp/nq3-hold.err &
HOLD_PID=$!
for _ in $(seq 20); do [ -s "$HOLD_JSON" ] && break; sleep 0.3; done
DISPLAY_ID=$(python3 -c "import json;print(json.load(open('$HOLD_JSON'))['displayID'])" 2>/dev/null || true)
if [ -z "${DISPLAY_ID:-}" ]; then
  echo "FATAL: 虚拟屏未就绪，中止取证（不许落主屏）"; exit 1
fi
for _ in $(seq 10); do
  FRAME=$(scripts/e2e/virtual-display list | python3 -c "
import json,sys
for d in json.load(sys.stdin):
    if d['id']==$DISPLAY_ID:
        print(d['x'])
        break
" 2>/dev/null || true)
  if [ -n "$FRAME" ] && [ "$FRAME" != "0" ]; then break; fi
  sleep 0.5
done
echo "virtual display: $DISPLAY_ID (frame x=$FRAME)"

# ---- 编译工具 ---------------------------------------------------------------
swiftc -O "$EV/synth_input.swift" -o "$SYNTH" 2>/dev/null || { echo "FATAL: synth 编译失败"; exit 1; }
swiftc -O "$EV/probe_window.swift" -o "$PROBE" 2>/dev/null || { echo "FATAL: probe 编译失败"; exit 1; }
clang -O2 "$EV/footprint_sampler.c" -o "$SAMPLER" 2>/dev/null || { echo "FATAL: sampler 编译失败"; exit 1; }
if [ "$($SYNTH trust)" != "trusted" ]; then
  echo "FATAL: 合成事件需要辅助功能授权（TCC）"; exit 1
fi

# ---- 宿主进程管理 -----------------------------------------------------------
start_app() { # $1=tag
  mkdir -p "/tmp/nq3-$1"
  ZOOMF="/tmp/nq3-$1/zoom"; ZOUT="/tmp/nq3-$1/zoomdump.json"
  PANELF="/tmp/nq3-$1/panel"
  : > "$ZOOMF"; : > "$PANELF"
  export NINJA_E2E_SCREEN="$DISPLAY_ID" NINJA_ZOOM_FILE="$ZOOMF" NINJA_ZOOM_DUMP="$ZOUT" \
         NINJA_PANEL_PLUGIN_FILE="$PANELF"
  ( "$BIN" > "$LOGDIR/$1.log" 2>&1 & echo $! > /tmp/nq3-app.pid )
  for _ in $(seq 40); do grep -q "q2 shell" "$LOGDIR/$1.log" 2>/dev/null && break; sleep 0.25; done
  APP_PID=$(cat /tmp/nq3-app.pid)
  sleep 1.0
  # 窗必须落在主屏之外（E2E 纪律：不落主屏）。
  for _ in $(seq 6); do
    WX=$(winbounds "$APP_PID" | cut -d' ' -f1)
    case "${WX:-1}" in
      -*|1[5-9][0-9][0-9]|2[0-9][0-9][0-9]) break ;;
    esac
    sleep 0.5
  done
}
activate() { # 拉前台 + 键盘就绪探子（普通键 'z' 的 keyDown 必须进日志）
  osascript -e "tell application \"System Events\" to set frontmost of first application process whose unix id is $APP_PID to true" >/dev/null 2>&1
  sleep 0.6
  for _ in $(seq 10); do
    $SYNTH key "$APP_PID" 6 >/dev/null 2>&1; sleep 0.35
    if grep -q "keyDown code=6" "$LOGDIR/$1.log" 2>/dev/null; then
      # 探子的 'z' 会留在命令行——退格清掉（不能回车：用户 zsh 装了 z
      # 跳转命令，回车会打出目录表把 echo 输出顶出首屏，实测踩过）。
      $SYNTH key "$APP_PID" 51 >/dev/null 2>&1; sleep 0.5
      return 0
    fi
    osascript -e "tell application \"System Events\" to set frontmost of first application process whose unix id is $APP_PID to true" >/dev/null 2>&1
    sleep 0.3
  done
  echo "WARN: $1 键盘探子未命中（后续键事件可能丢失）"
}
stop_app() {
  [ -n "${APP_PID:-}" ] || return 0
  kill "$APP_PID" 2>/dev/null
  for _ in $(seq 30); do kill -0 "$APP_PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null
  APP_PID=""
  sleep 0.4
}
sock_of() { echo "${TMPDIR:-/tmp}/ninja-ade-$1.sock"; }

# 点击点计算：zdump（网格）+ 窗 CG bounds → (cx cy row rows cw ch lx ly lw lh)
click_pos() { # $1=zoomdump
  python3 - "$1" "$APP_PID" <<'EOF'
import json, subprocess, sys
dump = json.load(open(sys.argv[1]))
leaf = [l for l in dump["leaves"] if not l["hidden"]][0]
wins = json.loads(subprocess.check_output(["/tmp/nq3-synth", "wins", sys.argv[2]]))
wx, wy, ww, wh = [x for x in wins if x["layer"] == 0][0]["bounds"]
cw = leaf["w"] / leaf["cols"]; ch = leaf["h"] / leaf["rows"]
# zsh 多行 prompt：echo 输出 ≈ 第 3 行；titlebar 32pt（win 高 - 内容高）。
tb = wh - leaf["h"]
print(f"{leaf['x']:.1f} {leaf['y']:.1f} {cw:.2f} {ch:.2f} {leaf['rows']} {wx:.1f} {wy:.1f} {tb:.1f}")
EOF
}
# 在指定网格行 ⌘click（col 3）
click_row() { # lx cw ch wy tb row
  local cx cy
  cx=$(python3 -c "print(f'{$6 + $1 + 3.5*$2:.1f}')")   # $6=wx
  cy=$(python3 -c "print(f'{$7 + $8 + ($9+0.5)*$3:.1f}')") # $7=wy $8=tb
  $SYNTH click "$APP_PID" "$cx" "$cy" cmd >/dev/null 2>&1
}

# ===========================================================================
say "0 协议契约 + 依赖红线"

cargo test --release -p ninja-protocol > "$LOGDIR/contract.log" 2>&1
if grep -q "test result: ok" "$LOGDIR/contract.log"; then
  ok "0.1 协议契约测试（往返/golden 17/信封/策略/帧层/第二语言）"
else
  bad "0.1 协议契约测试失败（$LOGDIR/contract.log）"
fi
assert_eq "0.2 golden 文件数（六类 17 型钉死）" "17" "$(ls crates/ninja-protocol/tests/golden/*.json | wc -l | tr -d ' ')"
# 依赖红线：示例插件只走公开协议（不链宿主、不链 ghostty-sys）。
PREV_TREE=$(cargo tree -p ninja-preview 2>/dev/null | grep -cE 'ghostty-sys|ninja v')
THEME_TREE=$(cargo tree -p ninja-theme 2>/dev/null | grep -cE 'ghostty-sys|ninja v')
assert_eq "0.3 ninja-preview 依赖树无宿主/ghostty-sys" "0" "$PREV_TREE"
assert_eq "0.4 ninja-theme 依赖树无宿主/ghostty-sys" "0" "$THEME_TREE"
cargo tree -p ninja-preview > "$EV/tree-ninja-preview.txt" 2>&1
cargo tree -p ninja-theme > "$EV/tree-ninja-theme.txt" 2>&1
# 宿主生命周期（启用即拉起/禁用回收/清扫/坏协议/空载零 socket）单测
#（socket 级集成：python3 最小插件直连）。
cargo test --release -p ninja > "$LOGDIR/host-unit.log" 2>&1
if grep -q "test result: ok" "$LOGDIR/host-unit.log"; then
  ok "0.5 宿主插件单测（hit 分发/生命周期/清扫/坏协议/空载零 socket）"
else
  bad "0.5 宿主插件单测失败（$LOGDIR/host-unit.log）"
fi

# ===========================================================================
say "A 门禁一：空载内存对照 Ghostty 本尊（同量级 ≤1.5×）"

GHOSTTY_VER=$(/usr/libexec/PlistBuddy -c "Print CFBundleShortVersionString" /Applications/Ghostty.app/Contents/Info.plist 2>/dev/null || echo "?")
assert_eq "A0 Ghostty 本尊版本" "1.3.0" "$GHOSTTY_VER"

rm -rf /tmp/nq3-idle; mkdir -p /tmp/nq3-idle/xdg
: > /tmp/nq3-idle/ninja.toml   # 空 [plugins]：默认零插件（空载）
isolate
export XDG_CONFIG_HOME=/tmp/nq3-idle/xdg NINJA_CONFIG=/tmp/nq3-idle/ninja.toml
unset NINJA_CFG_DUMP NINJA_ADE_DEBUG NINJA_ZOOM_FILE NINJA_ZOOM_DUMP NINJA_PANEL_PLUGIN_FILE NINJA_THEME
start_app idle
sleep 4   # 预热（Metal shader 编译等）
if ! pgrep -f "target/release/ninja-preview" >/dev/null 2>&1 && ! pgrep -f "target/release/ninja-theme" >/dev/null 2>&1; then
  ok "A1 空载零插件进程"
else
  bad "A1 空载出现插件进程"
fi
LSOF=$(lsof -a -U -p "$APP_PID" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
assert_eq "A2 空载宿主 unix socket 数（零插件 socket）" "0" "$LSOF"
NINJA_MED=$($SAMPLER "$APP_PID" 15 500)
echo "ninja idle footprint median: $NINJA_MED bytes" | tee "$EV/footprint-idle.txt"
stop_app

"$GHOSTTY_BIN" > "$LOGDIR/ghostty.log" 2>&1 &
GH_PID=$!
sleep 6
GH_MED=$($SAMPLER "$GH_PID" 15 500)
echo "ghostty 1.3.0 footprint median: $GH_MED bytes" | tee -a "$EV/footprint-idle.txt"
kill "$GH_PID" 2>/dev/null; wait "$GH_PID" 2>/dev/null
RATIO=$(python3 -c "print(f'{$NINJA_MED/$GH_MED:.3f}')")
echo "ratio ninja/ghostty = $RATIO" | tee -a "$EV/footprint-idle.txt"
python3 -c "import sys;sys.exit(0 if $NINJA_MED <= 1.5*$GH_MED else 1)" \
  && ok "A3 空载内存同量级（ninja $NINJA_MED / ghostty $GH_MED = ${RATIO}× ≤1.5×）" \
  || bad "A3 空载内存超 1.5×（${RATIO}×）"
IDLE_MED=$NINJA_MED
restore_cfg

# ===========================================================================
say "B 门禁二：第一个插件（⌘click 路径 → 终端内看文本 → Esc 关层焦点回终端）"

rm -rf /tmp/nq3-b /tmp/nq3p; mkdir -p /tmp/nq3-b/xdg/ghostty /tmp/nq3p
# 终端背景 #3a2a5b（紫）与预览层 #282c34（暗）强对比——像素可判。
printf 'background = #3a2a5b\n' > /tmp/nq3-b/xdg/ghostty/config
cat > /tmp/nq3-b/ninja.toml <<'EOF'
[plugins]
enabled = ["ninja-preview"]
EOF
# 样本文件：首行密集 '#'（命中行高亮+前景密集字形 → 层内容方差可判）。
python3 - <<'EOF'
lines = ["#" * 90]
lines += [f"q3-preview-line-{i:02d} lorem ipsum dolor sit amet" for i in range(1, 60)]
open("/tmp/nq3p/sample.txt", "w").write("\n".join(lines) + "\n")
EOF
isolate
export XDG_CONFIG_HOME=/tmp/nq3-b/xdg NINJA_CONFIG=/tmp/nq3-b/ninja.toml \
       NINJA_CFG_DUMP=/tmp/nq3-b/dump.json NINJA_ADE_DEBUG=1 NINJA_Q1_DEBUG=1
start_app b
# 启用即拉起。
grep -q '已拉起插件 "ninja-preview"' "$LOGDIR/b.log" && ok "B1 启用即拉起（宿主日志）" || bad "B1 拉起日志缺失"
PREVIEW_PID=$(pgrep -f "target/release/ninja-preview" | head -1)
[ -n "$PREVIEW_PID" ] && ok "B2 ninja-preview 进程在跑（pid ${PREVIEW_PID}）" || bad "B2 插件进程不在"
[ -S "$(sock_of $APP_PID)" ] && ok "B3 ADE socket 存在（$(sock_of $APP_PID)）" || bad "B3 socket 缺失"
# 像素基线：终端背景（虚拟屏无色空间压暗，直接对原始值）。
assert_px b-before "B4 点击前终端背景 ≈ #3a2a5b" 58 42 91 14 0.45 0.30 0.40 0.30
# 输入路径行 + 激活 + 键盘探子。
activate b
$SYNTH type "$APP_PID" "echo /tmp/nq3p/sample.txt" >/dev/null 2>&1; sleep 0.3
$SYNTH key "$APP_PID" 36 >/dev/null 2>&1; sleep 1.2
dump b-grid.json
read -r LX LY CW CH CROWS WXW WYH TB <<< "$(click_pos /tmp/nq3-b/zoomdump.json)" || true
shot b-before-click.png "$(winid "$APP_PID")"
# ⌘+click（全局 HID tap）——行梯子 3/4/2/5/6（zsh 多行 prompt 下 echo
# 输出在 ~3 行；命中行梯子兜底）。
CLICKED=""
for TRY_ROW in 3 4 2 5 6; do
  click_row "$LX" "$CW" "$CH" "$LY" "$CROWS" "$WXW" "$WYH" "$TB" "$TRY_ROW"
  sleep 1.3
  if grep -q "layer.present" "$LOGDIR/b.log"; then CLICKED="$TRY_ROW"; break; fi
done
[ -n "$CLICKED" ] && ok "B5 ⌘+click 路径 → 插件 present 层（命中行 row=${CLICKED}，真鼠标全局 HID tap）" || bad "B5 点击未产生层"
grep -q 'hit id=.*kind=Path text="/tmp/nq3p/sample.txt"' "$LOGDIR/b.log" && ok "B6 hit 广播（kind=path、text=路径）" || bad "B6 hit 广播日志缺失"
grep -q "claim priority=100" "$LOGDIR/b.log" && ok "B7 hit.claim priority=100（ninja-preview 认领）" || bad "B7 claim 日志缺失"
grep -q "layer.open → ready" "$LOGDIR/b.log" && ok "B8 layer.open → layer.ready（IOSurface global id）" || bad "B8 层握手日志缺失"
grep -q "layer.present handle=" "$LOGDIR/b.log" && ok "B9 layer.present（宿主合成）" || bad "B9 present 日志缺失"
shot b-layer.png "$(winid "$APP_PID")"
# 层内容像素：锚行之下 ≈ 预览层背景 #282c34±14；文本行方差 = 文件内容
# 在层里的证据（空白背景 std≈0）。
# 截图（screencapture -l）是窗口局部坐标：Y0 = (titlebar + anchor + 2行) / 窗高。
Y0=$(python3 -c "
tb=$TB; ly=$LY; ch=$CH; row=$CLICKED; wh=$(winbounds "$APP_PID" | cut -d' ' -f4)
print(f'{min(0.9, max(0.05, (tb + ly + (row+2.0)*ch) / wh)):.4f}')")
PROBE_OUT=$(probe_px b-layer var 0.08 "$Y0" 0.80 0.18)
# B10（背景）：层的右下段——行号/文本在左侧，右侧是预览层纯背景
#（避开密集 # 首行把均值拉飞）。
YBG=$(python3 -c "
tb=$TB; ly=$LY; ch=$CH; row=$CLICKED; wh=$(winbounds "$APP_PID" | cut -d' ' -f4)
print(f'{min(0.9, max(0.05, (tb + ly + (row+6.0)*ch) / wh)):.4f}')")
BG_OUT=$(probe_px b-layer-bg avg 0.55 "$YBG" 0.38 0.10)
python3 - "$BG_OUT" <<'EOF' && ok "B10 层背景像素 = 预览层 #282c34（${BG_OUT}）" || bad "B10 层区域像素不是预览层背景（${BG_OUT}）"
import json, sys
d = json.loads(sys.argv[1].strip())
r, g, b = d["avg"]
sys.exit(0 if abs(r-40) <= 14 and abs(g-44) <= 14 and abs(b-52) <= 14 else 1)
EOF
# B11（内容）：层正文带的方差——密集字形存在 = 文件文本画进了层。
python3 - "$PROBE_OUT" <<'EOF' && ok "B11 层内容含文件文本（${PROBE_OUT}：密集字形方差 > 空白背景）" || bad "B11 层内容方差不足（疑似空层）"
import json, sys
d = json.loads(sys.argv[1].strip())
sys.exit(0 if sum(d["std"]) > 18 else 1)
EOF
# Esc 关层 + 焦点回终端。
$SYNTH key "$APP_PID" 53 >/dev/null 2>&1; sleep 1.0
grep -q "Esc 关层" "$LOGDIR/b.log" && ok "B12 Esc 关层（宿主收口 + 通知插件 layer.close）" || bad "B12 Esc 关层日志缺失"
assert_px b-after-esc "B13 关层后像素回终端背景 #3a2a5b" 58 42 91 16 0.45 0.35 0.40 0.25
# 焦点回终端：Esc 关层后键入 11 字符——「进了 surface_key 通道」即焦点
# 回终端的证据（层前台路由 input.key 不产生 surface_key 日志；zsh 的
# autosuggestion ghost 文本让 zoom dump 的 last 不可靠，弃用）。
$SYNTH type "$APP_PID" "focusback42" >/dev/null 2>&1; sleep 0.9
dump b-focus.json
POST_ESC=$(grep -n "Esc 关层" "$LOGDIR/b.log" | tail -1 | cut -d: -f1)
N_KEYS=$(tail -n +$((POST_ESC+1)) "$LOGDIR/b.log" | grep -c "surface_key.*text=")
[ "${N_KEYS:-0}" -ge 9 ] && ok "B14 Esc 后焦点回终端（键入 ${N_KEYS} 字符全走 surface_key 通道）" || bad "B14 Esc 后键入未落终端（surface_key text 事件 ${N_KEYS:-0}）"
cp /tmp/nq3-b/dump.json "$EV/b-dump.json" 2>/dev/null
stop_app
restore_cfg

# ===========================================================================
say "C 门禁三：关掉即轻（禁用回收 / 杀插件收层+色板回退 / SIGKILL 宿主）"

rm -rf /tmp/nq3-c; mkdir -p /tmp/nq3-c/xdg/ghostty
printf 'background = #3a2a5b\n' > /tmp/nq3-c/xdg/ghostty/config
cat > /tmp/nq3-c/ninja.toml <<'EOF'
[plugins]
enabled = ["ninja-preview", "ninja-theme"]
EOF
isolate
export XDG_CONFIG_HOME=/tmp/nq3-c/xdg NINJA_CONFIG=/tmp/nq3-c/ninja.toml \
       NINJA_CFG_DUMP=/tmp/nq3-c/dump.json NINJA_ADE_DEBUG=1 NINJA_Q1_DEBUG=1
start_app c
sleep 3.0   # theme.set → 泵消化 → 热重载传播
zoom cfgdump
assert_json /tmp/nq3-c/dump.json "d['plugin_theme']=='solarized-dark'" "C1 theme.set 应用（覆盖层装载，名 solarized-dark）"
assert_json /tmp/nq3-c/dump.json "d['background']==[0,43,54]" "C2 色板生效（background=#002b36 solarized-dark）"
assert_px c-theme "C3 色板像素 ≈ #002b36" 0 43 54 16 0.45 0.30 0.40 0.30
THEME_PID=$(pgrep -f "target/release/ninja-theme" | head -1)
# C4：杀主题插件 → 连接 EOF → 色板回退基线。
kill -9 "$THEME_PID" 2>/dev/null; sleep 2.5
grep -q "色板回退" "$LOGDIR/c.log" && ok "C4 杀 ninja-theme → 色板回退（宿主日志）" || bad "C4 色板回退日志缺失"
zoom cfgdump
assert_json /tmp/nq3-c/dump.json "d['background']==[58,42,91]" "C5 回退后 background=#3a2a5b（用户基线）"
assert_px c-revert "C6 回退像素 ≈ #3a2a5b" 58 42 91 16 0.45 0.30 0.40 0.30
# C7：点击开层 → SIGKILL 插件 → 层回收。
activate c
$SYNTH type "$APP_PID" "echo /tmp/nq3p/sample.txt" >/dev/null 2>&1; sleep 0.3
$SYNTH key "$APP_PID" 36 >/dev/null 2>&1; sleep 1.2
dump c-grid.json
read -r LX LY CW CH CROWS WXW WYH TB <<< "$(click_pos /tmp/nq3-c/zoomdump.json)" || true
CLICKED_C=""
for TRY_ROW in 3 4 2 5 6; do
  click_row "$LX" "$CW" "$CH" "$LY" "$CROWS" "$WXW" "$WYH" "$TB" "$TRY_ROW"
  sleep 1.3
  if grep -q "layer.present" "$LOGDIR/c.log"; then CLICKED_C="$TRY_ROW"; break; fi
done
[ -n "$CLICKED_C" ] && ok "C7 ⌘+click → 层出现（second claim）" || bad "C7 层未出现"
PREVIEW_PID=$(pgrep -f "target/release/ninja-preview" | head -1)
kill -9 "$PREVIEW_PID" 2>/dev/null; sleep 1.5
grep -q "已回收其全部层" "$LOGDIR/c.log" && ok "C8 SIGKILL ninja-preview → 连接 EOF → 层回收" || bad "C8 层回收日志缺失"
assert_px c-dead "C9 插件死亡后像素回终端背景" 58 42 91 18 0.45 0.30 0.40 0.30
# C10：面板开关（NINJA_PANEL_PLUGIN_FILE 钩子 = UI checkbox 同路径）。
echo "ninja-preview off" > /tmp/nq3-c/panel; sleep 1.6
grep -q '面板开关 "ninja-preview" → off' "$LOGDIR/c.log" && ok "C10 面板 off ninja-preview（写回+回收）" || bad "C10 面板 off 未生效"
! pgrep -f "target/release/ninja-preview" >/dev/null 2>&1 && ok "C11 off 后无 ninja-preview 进程" || bad "C11 off 后插件进程残留"
# 再关 theme（已死）→ 名单空 → socket 删除。
echo "ninja-theme off" > /tmp/nq3-c/panel; sleep 1.6
[ ! -e "$(sock_of $APP_PID)" ] && ok "C12 名单空 → socket 文件删除（关掉即轻）" || bad "C12 socket 残留"
grep -q "插件已禁用" "$LOGDIR/c.log" && ok "C13 禁用收口日志（层/连接/子进程/socket 全清）" || bad "C13 禁用日志缺失"
# C14：off 后宿主 footprint 回空载基线 + 容差（8 MiB：theme 热重载的配置克隆/IO 缓存留在宿主 footprint，同量级即算回空载）。
sleep 1.5
AFTER_MED=$($SAMPLER "$APP_PID" 8 300)
python3 -c "import sys;sys.exit(0 if $AFTER_MED <= $IDLE_MED + 8*1024*1024 else 1)" \
  && ok "C14 禁用后宿主内存回空载基线（$AFTER_MED ≤ idle $IDLE_MED + 8MiB）" \
  || bad "C14 禁用后宿主内存未回基线（$AFTER_MED vs idle ${IDLE_MED}）"
# C15：再启用即重拉。
echo "ninja-preview on" > /tmp/nq3-c/panel; sleep 1.6
pgrep -f "target/release/ninja-preview" >/dev/null 2>&1 && ok "C15 面板 on → 启用即重拉" || bad "C15 重拉失败"
[ -S "$(sock_of $APP_PID)" ] && ok "C16 重拉后 socket 重建" || bad "C16 socket 未重建"
grep -q 'enabled = \["ninja-preview"\]' /tmp/nq3-c/ninja.toml && ok "C17 面板开关写回 ninja.toml" || bad "C17 ninja.toml 写回不符"
# C18：SIGKILL 宿主 → 插件 EOF 自退 + 陈旧 socket 下次启动清扫。
OLD_SOCK=$(sock_of "$APP_PID")
kill -9 "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null; APP_PID=""
for _ in $(seq 50); do pgrep -f "target/release/ninja-preview" >/dev/null 2>&1 || break; sleep 0.1; done
! pgrep -f "target/release/ninja-preview" >/dev/null 2>&1 && ok "C18 宿主 SIGKILL 后插件 EOF 自退（无残留进程）" || bad "C18 插件未自退"
[ -e "$OLD_SOCK" ] && ok "C19 SIGKILL 留下陈旧 socket（下次启动清扫的取证前提）" || bad "C19 陈旧 socket 不在（清扫无从取证）"
# 重启宿主（enabled 仍含 ninja-preview）→ 清扫陈旧 + 新 socket。
start_app c2
sleep 2.0
grep -q "清扫陈旧 ADE socket" "$LOGDIR/c2.log" && ok "C20 启动清扫陈旧 socket（pid 已死才删）" || bad "C20 清扫日志缺失"
[ ! -e "$OLD_SOCK" ] && ok "C21 陈旧 socket 已被清扫" || bad "C21 陈旧 socket 残留"
[ -S "$(sock_of $APP_PID)" ] && ok "C22 新会话 socket（新 pid）" || bad "C22 新 socket 缺失"
cp /tmp/nq3-c/dump.json "$EV/c-dump.json" 2>/dev/null
stop_app
restore_cfg

# ===========================================================================
say "D q0 取证模式回归（--evidence-dir）"
rm -rf /tmp/nq3-d
unset NINJA_CFG_DUMP NINJA_ADE_DEBUG NINJA_ZOOM_FILE NINJA_ZOOM_DUMP NINJA_PANEL_PLUGIN_FILE NINJA_CONFIG
export XDG_CONFIG_HOME=/tmp/nq3-d
"$BIN" --evidence-dir /tmp/nq3-d > "$LOGDIR/d.log" 2>&1
Q0_RC=$?
[ "$Q0_RC" -eq 0 ] && ok "D1 q0 模式 exit 0" || bad "D1 q0 模式 exit $Q0_RC"
grep -qi "overall: pass" /tmp/nq3-d/report.txt && ok "D2 q0 report.txt overall: PASS" || bad "D2 q0 报告非 PASS"
cp /tmp/nq3-d/report.txt "$EV/d-report.txt"

# ===========================================================================
say "结果：PASS=$PASS FAIL=$FAIL"
echo "PASS=$PASS FAIL=$FAIL" | tee "$EV/e2e-summary.txt"
[ "$FAIL" = "0" ] && echo "OVERALL: PASS" | tee -a "$EV/e2e-summary.txt" || echo "OVERALL: FAIL" | tee -a "$EV/e2e-summary.txt"
exit $([ "$FAIL" = "0" ] && echo 0 || echo 1)
