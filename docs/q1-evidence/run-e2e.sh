#!/bin/bash
# q1 E2E：壳验收取证（产物写入本目录，可重复执行覆盖）。
#
# 验收（PLAN q1「过」）→ 断言：
#  ① NEW_TAB/NEW_SPLIT 后 dump 叶几何与网格列×行（两面各自非零）
#  ② ⌘W 双路径：多 pane 只关一面（leaves-1、窗口数不变）；单 pane 关
#     tab/窗（窗口数-1 / 进程退出）。真键 ⌘W=菜单 performClose 路径
#     （windowShouldClose 裸⌘W 决策），ghostty_surface_request_close /
#     bindact=close_surface 绑定路径（close_surface_cb）双通道同语义。
#  ③ EOF（exit）：只拆当前面（process_alive=false 日志 + leaves 变化）
#  ④ ⌘⇧Enter：多 pane 放大焦点面（隐藏面网格冻结）、再按还原；单 pane
#     = 窗口 zoom；TOGGLE_SPLIT_ZOOM 走 bindact 路径同语义。
#  ⑤ 焦点：点击/goto_split（⌘]）切换后，键入回显落目标面网格
#  ⑥ resize：窗口角拖拽、分隔条拖拽、⌘⌃←（RESIZE_SPLIT）、⌃⌘=
#     （EQUALIZE_SPLITS）→ 叶几何与网格列数变化
#  ⑦ 截图按窗口 ID（screencapture -l）留档本目录
#  ⑧ 空载纪律：宿主子进程仅 PTY shell；无插件 socket
#  ⑨ q0 取证模式回归 PASS（--evidence-dir）
#
# 全程虚拟屏（PLAN「E2E 虚拟屏幕」）：hold → NINJA_E2E_SCREEN=<displayID>
# → 取证 → kill hold。真键/鼠标经 synth_input（CGEventPostToPid，不抢
# 开发者焦点）；布局 dump 走 NINJA_ZOOM_FILE/NINJA_ZOOM_DUMP 钩子；
# 决策日志 NINJA_Q1_DEBUG=1。断言全部跑命令/读 JSON，不信自我声明。
set -u
cd "$(dirname "$0")/../.."
BIN=./target/release/ninja
EV=docs/q1-evidence
SYNTH=/tmp/nq1-synth
HOLD_JSON=/tmp/nq1-hold.json
LOGDIR=$EV/e2e-logs
mkdir -p "$LOGDIR"
PASS=0; FAIL=0

say()  { printf '\n== %s\n' "$*"; }
ok()   { PASS=$((PASS+1)); echo "  [PASS] $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  [FAIL] $*"; }
assert_eq() { # desc expected actual
  if [ "$2" = "$3" ]; then ok "$1 = $3"; else bad "$1: 期望 $2 实得 $3"; fi
}
assert_json() { # file py-expr desc（expr 为 True 断言）
  local got
  got=$(python3 -c "import json;d=json.load(open('$1'));print($2)" 2>&1)
  if [ "${got:-}" = "True" ]; then ok "$3"; else bad "$3（实得: ${got:-}）"; fi
}
zoom() { echo "$1" > "$ZOOMF"; sleep 0.7; }
dump() { zoom "dump$1"; cp "$ZOUT" "$EV/$2"; echo "$2 ok"; }

# ---- 虚拟屏（拿不到 displayID 即中止，不落主屏） ---------------------------
HOLD_PID=""
cleanup() {
  [ -n "${APP_PID:-}" ] && { kill "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null; APP_PID=""; }
  [ -n "$HOLD_PID" ] && kill "$HOLD_PID" 2>/dev/null
}
trap cleanup EXIT
scripts/e2e/virtual-display hold 1440 900 0 > "$HOLD_JSON" 2>/tmp/nq1-hold.err &
HOLD_PID=$!
for _ in $(seq 20); do [ -s "$HOLD_JSON" ] && break; sleep 0.3; done
DISPLAY_ID=$(python3 -c "import json;print(json.load(open('$HOLD_JSON'))['displayID'])" 2>/dev/null || true)
if [ -z "${DISPLAY_ID:-}" ]; then
  echo "FATAL: 虚拟屏未就绪，中止取证（不许落主屏）"; exit 1
fi
# 等窗服给新虚拟屏排位稳定（刚创建时可能与主屏重叠/镜像 → 落窗会进主屏）。
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

# ---- 编译输入驱动 ---------------------------------------------------------
swiftc -O "$EV/synth_input.swift" -o "$SYNTH" 2>/dev/null || { echo "FATAL: synth 编译失败"; exit 1; }
if [ "$($SYNTH trust)" != "trusted" ]; then
  echo "FATAL: 合成键盘事件需要辅助功能授权（TCC）"; exit 1
fi

wincount() { $SYNTH wins "$1" | python3 -c "import json,sys;print(sum(1 for w in json.load(sys.stdin) if w['layer']==0))"; }
winid()    { $SYNTH wins "$1" | python3 -c "import json,sys;print([w['id'] for w in json.load(sys.stdin) if w['layer']==0][0])"; }
winbounds(){ $SYNTH wins "$1" | python3 -c "import json,sys;print(' '.join(str(v) for v in [w for w in json.load(sys.stdin) if w['layer']==0][0]['bounds']))"; }
shot()     { [ -n "${2:-}" ] && screencapture -x -l "$2" "$EV/$1" >/dev/null 2>&1; }

start_app() { # $1=tag $2=selftest(可为空)
  rm -rf "/tmp/nq1-$1"; mkdir -p "/tmp/nq1-$1"
  ZOOMF="/tmp/nq1-$1/zoom"; ZOUT="/tmp/nq1-$1/dump.json"
  if [ -n "${2:-}" ]; then export NINJA_P2_SELFTEST="$2"; else unset NINJA_P2_SELFTEST; fi
  export NINJA_E2E_SCREEN="$DISPLAY_ID" NINJA_ZOOM_FILE="$ZOOMF" NINJA_ZOOM_DUMP="$ZOUT" NINJA_Q1_DEBUG=1
  ( "$BIN" > "$LOGDIR/$1.log" 2>&1 & echo $! > /tmp/nq1-app.pid )
  for _ in $(seq 40); do grep -q "q1 shell" "$LOGDIR/$1.log" 2>/dev/null && break; sleep 0.25; done
  APP_PID=$(cat /tmp/nq1-app.pid)
  sleep 2.2   # selftest 0.8s 起拍 + shell 落地
  # 窗必须落在主屏之外（E2E 纪律：不落主屏；等窗服排位稳定）。
  for _ in $(seq 6); do
    WX=$(winbounds "$APP_PID" | cut -d' ' -f1)
    case "${WX:-1}" in
      -*|1[5-9][0-9][0-9]|2[0-9][0-9][0-9]) break ;;
    esac
    sleep 0.5
  done
  # 键盘就绪探针：普通键必须在日志出现 keyDown（app 激活/焦点就位）。
  $SYNTH type "$APP_PID" "z\\b"; sleep 0.3
  for _ in $(seq 10); do
    grep -q "keyDown code=6" "$LOGDIR/$1.log" 2>/dev/null && break
    $SYNTH type "$APP_PID" "z\\b"; sleep 0.4
  done
  grep -q "keyDown code=6" "$LOGDIR/$1.log" 2>/dev/null || echo "WARN: $1 键盘探针未命中（后续键事件可能丢失）"
}
stop_app() {
  [ -n "${APP_PID:-}" ] || return 0
  kill "$APP_PID" 2>/dev/null
  for _ in $(seq 30); do kill -0 "$APP_PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null
  APP_PID=""
  sleep 0.4
}

# ===========================================================================
say "A 布局树 + ⌘⇧Enter 三态 + 焦点 + resize（tab,split 自布置）"
start_app a tab,split
: > "$ZOOMF"
# A1 ① tab+split：两叶非零几何与网格，tab 并入同一 CG 窗
dump a1 a-step1.json
assert_json "$EV/a-step1.json" "len(d['leaves'])==2" "A1 selftest tab,split → leaves=2"
assert_json "$EV/a-step1.json" "all(l['w']>50 and l['h']>50 and l['cols']>10 and l['rows']>5 for l in d['leaves'])" "A1 两叶几何/网格各自非零"
assert_json "$EV/a-step1.json" "abs(d['leaves'][0]['cols']-d['leaves'][1]['cols'])<=1" "A1 对半分屏列数一致"
assert_eq "A1 窗口数（tab 并入）" 1 "$(wincount $APP_PID)"
WID=$(winid $APP_PID)
shot shot-split-tab.png "$WID"
# A2 ④ ⌘⇧Enter（真键=菜单路径）放大：隐藏面冻结、放大面占满
$SYNTH key $APP_PID 36 cmd,shift; sleep 0.8
dump a2 a-step2-zoom.json
assert_json "$EV/a-step2-zoom.json" "d['zoomed']==True" "A2 ⌘⇧Enter 放大焦点面"
assert_json "$EV/a-step2-zoom.json" "sum(1 for l in d['leaves'] if l['hidden'])==1" "A2 其余面隐藏"
assert_json "$EV/a-step2-zoom.json" "[l for l in d['leaves'] if not l['hidden']][0]['cols'] > [l for l in d['leaves'] if l['hidden']][0]['cols']" "A2 放大面网格变宽"
ZOOMED_COLS_BEFORE=$($SYNTH wins $APP_PID >/dev/null; python3 -c "import json;d=json.load(open('$ZOUT'));print([l for l in d['leaves'] if not l['hidden']][0]['cols'])")
shot shot-zoomed.png "$WID"
# A3 ④ 再按还原：几何回分屏态
$SYNTH key $APP_PID 36 cmd,shift; sleep 0.8
dump a3 a-step3-restore.json
assert_json "$EV/a-step3-restore.json" "d['zoomed']==False and not any(l['hidden'] for l in d['leaves'])" "A3 ⌘⇧Enter 还原布局"
assert_json "$EV/a-step3-restore.json" "abs([l for l in d['leaves'] if l['x']==0.0][0]['w'] - (d['window']['w']-5)/2) < 2" "A3 还原后对半几何"
# A4 ④ TOGGLE_SPLIT_ZOOM 走 ghostty action 路径（bindact）同语义
zoom "bindact:toggle_split_zoom"
dump a4 a-step4-bindact.json
assert_json "$EV/a-step4-bindact.json" "d['zoomed']==True" "A4 bindact:toggle_split_zoom 放大（action_cb 路径）"
grep -q "action tag=.*" "$LOGDIR/a.log" && ok "A4 action_cb 决策日志在 $LOGDIR/a.log"
zoom "bindact:toggle_split_zoom"
dump a5 a-step5-bindact-restore.json
assert_json "$EV/a-step5-bindact-restore.json" "d['zoomed']==False" "A5 action 路径再按还原"
# A6 ⑤ 点击左面夺焦 → 键入落左面；⌘]（goto_split next）→ 键入落右面
read -r WX WY WW WH <<< "$(winbounds $APP_PID)"
CX_L=$(python3 -c "print($WX + $WW*0.25)"); CY=$(python3 -c "print($WY + $WH*0.55)")
$SYNTH click $APP_PID "$CX_L" "$CY"; sleep 0.4
$SYNTH type $APP_PID "echoL"; sleep 0.5
dump a6 a-step6-focus.json
assert_json "$EV/a-step6-focus.json" "any('echoL' in l['last'] for l in d['leaves'] if l['x']<50)" "A6 点击左面 → 回显落左面"
$SYNTH key $APP_PID 30 cmd; sleep 0.5   # ⌘] goto_split next
$SYNTH type $APP_PID "echoR"; sleep 0.5
dump a7 a-step7-goto.json
assert_json "$EV/a-step7-goto.json" "any('echoR' in l['last'] for l in d['leaves'] if l['x']>50)" "A7 ⌘] goto_split → 回显落右面"
# A8 ⑥ 窗口角拖拽 resize → 叶几何与网格列数变化
read -r WX WY WW WH <<< "$(winbounds $APP_PID)"
COLS_BEFORE=$(python3 -c "import json;print([l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['cols'])")
$SYNTH drag $APP_PID "$(python3 -c "print($WX+$WW-3)")" "$(python3 -c "print($WY+$WH-3)")" "$(python3 -c "print($WX+$WW-203)")" "$(python3 -c "print($WY+$WH-123)")"; sleep 0.8
dump a8 a-step8-winresize.json
COLS_AFTER=$(python3 -c "import json;print([l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['cols'])")
[ "$COLS_AFTER" -lt "$COLS_BEFORE" ] && ok "A8 窗口拖小 → 列数 ${COLS_BEFORE}→$COLS_AFTER" || bad "A8 窗口 resize 列数未减（${COLS_BEFORE}→${COLS_AFTER}）"
# A9 ⑥ 分隔条拖拽 → ratio 变化
read -r WX WY WW WH <<< "$(winbounds $APP_PID)"
W_LEFT=$(python3 -c "import json;print([l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['w'])")
DX=$(python3 -c "import json;print($WX + [l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['w'] + 2.5)")
DY=$(python3 -c "print($WY + $WH/2)")
$SYNTH drag $APP_PID "$DX" "$DY" "$(python3 -c "print($DX-90)")" "$DY"; sleep 0.8
dump a9 a-step9-divider.json
W_LEFT2=$(python3 -c "import json;print([l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['w'])")
[ "$(python3 -c "print(int($W_LEFT2-$W_LEFT))")" -lt -60 ] && ok "A9 分隔条左拖 → 左叶 ${W_LEFT}→$W_LEFT2" || bad "A9 分隔条拖拽未生效（${W_LEFT}→${W_LEFT2}）"
# A10 ⑥ ⌘⌃←（RESIZE_SPLIT，ghostty 默认键位）→ 左叶变窄
W_LEFT3_BEFORE=$(python3 -c "import json;print([l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['w'])")
$SYNTH key $APP_PID 123 cmd,ctrl; sleep 0.6
dump a10 a-step10-resizesplit.json
W_LEFT3=$(python3 -c "import json;print([l for l in json.load(open('$ZOUT'))['leaves'] if l['x']<50][0]['w'])")
[ "$(python3 -c "print(int($W_LEFT3_BEFORE-$W_LEFT3))")" -gt 5 ] && ok "A10 ⌘⌃← resize_split → 左叶 ${W_LEFT3_BEFORE}→$W_LEFT3" || bad "A10 RESIZE_SPLIT 未生效（${W_LEFT3_BEFORE}→${W_LEFT3}）"
# A11 ⑥ ⌃⌘=（EQUALIZE_SPLITS）→ 回对半
$SYNTH key $APP_PID 24 cmd,ctrl; sleep 0.6
dump a11 a-step11-equalize.json
assert_json "$EV/a-step11-equalize.json" "abs(d['leaves'][0]['w']-d['leaves'][1]['w'])<1.5" "A11 ⌃⌘= equalize → 对半"
# A12 ⑧ 空载纪律：子进程仅 PTY shell；宿主无插件 socket
CHILDREN=$(pgrep -lP $APP_PID | awk '{print $2}' | sort | tr '\n' ' ')
echo "  children: $CHILDREN" | tee "$EV/empty-children.txt"
if python3 -c "
import sys,subprocess,re
out=subprocess.run(['pgrep','-lP','$APP_PID'],capture_output=True,text=True).stdout
rows=[l.split(' ',1)[1].strip() for l in out.splitlines() if l.strip()]
sys.exit(0 if all(re.fullmatch(r'(-?zsh|sh|login|-bash|sudo).*|.*zsh.*|.*bash.*', r) for r in rows) else 1)
"; then ok "A12 子进程只有 shell（${CHILDREN}）"; else bad "A12 出现非 shell 子进程：${CHILDREN}"; fi
LSOF=$(lsof -a -U -p "$APP_PID" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
echo "  unix sockets in host: $LSOF" | tee -a "$EV/empty-children.txt"
assert_eq "A12 宿主 unix socket 数（零插件 socket）" 0 "$LSOF"
stop_app

# ===========================================================================
say "B ⌘W 菜单路径多 pane + EOF + 单 pane 窗口 zoom"
start_app b
: > "$ZOOMF"
dump b0 b-step0.json
assert_json "$EV/b-step0.json" "len(d['leaves'])==1 and d['window']['zoomed']==False" "B0 单 pane 起步"
# B1 ④ 单 pane ⌘⇧Enter = 窗口 zoom（最大化非全屏）
$SYNTH key $APP_PID 36 cmd,shift; sleep 0.8
dump b1 b-step1-winzoom.json
assert_json "$EV/b-step1-winzoom.json" "d['window']['zoomed']==True and d['zoomed']==False" "B1 单 pane ⌘⇧Enter → window.zoomed"
$SYNTH key $APP_PID 36 cmd,shift; sleep 0.8
dump b2 b-step2-unzoom.json
assert_json "$EV/b-step2-unzoom.json" "d['window']['zoomed']==False" "B2 再按还原窗口"
# B3 ⌘D 真键分屏（ghostty 默认绑定 NEW_SPLIT:right → action_cb）
$SYNTH key $APP_PID 2 cmd; sleep 0.8
dump b3 b-step3-split.json
assert_json "$EV/b-step3-split.json" "len(d['leaves'])==2" "B3 ⌘D → NEW_SPLIT:right → leaves=2"
grep -q "close_surface_cb alive=true process_alive=true" "$LOGDIR/b.log" && bad "B3 出现意外 close 日志" || ok "B3 无意外 close"
# B4 ② ⌘W 多 pane（菜单 performClose 路径，windowShouldClose 裸⌘W 决策）
WINS_BEFORE=$(wincount $APP_PID)
$SYNTH key $APP_PID 13 cmd; sleep 1.0
dump b4 b-step4-cmdw.json
assert_json "$EV/b-step4-cmdw.json" "len(d['leaves'])==1" "B4 真键 ⌘W 多 pane → leaves=1（只关一面）"
assert_eq "B4 窗口数不变" "$WINS_BEFORE" "$(wincount $APP_PID)"
grep -q "close-request keyDown.*bare_cmd=true" "$LOGDIR/b.log" && ok "B4 决策日志：裸 ⌘W 识别" || bad "B4 缺 close-request keyDown 日志"
grep -q "windowShouldClose -> false" "$LOGDIR/b.log" && ok "B4 决策日志：windowShouldClose false（拦整窗）" || bad "B4 缺 windowShouldClose false 日志"
# B5 ③ EOF：exit → close_surface_cb(process_alive=false) → 单 pane → 关窗退出
$SYNTH type $APP_PID "exit\n"; sleep 2.5
if kill -0 $APP_PID 2>/dev/null; then bad "B5 EOF 后进程未退出"; else ok "B5 EOF(exit) → 单 pane 关窗 → 进程退出"; fi
grep -q "close_surface_cb alive=true process_alive=false" "$LOGDIR/b.log" && ok "B5 close_surface_cb process_alive=false" || bad "B5 缺 close_surface_cb(process_alive=false) 日志"
grep -q "windowWillClose" "$LOGDIR/b.log" && ok "B5 windowWillClose 收尾" || bad "B5 缺 windowWillClose 日志"
cp "$LOGDIR/b.log" "$EV/eof-cmdw.log"
APP_PID=""

# ===========================================================================
say "C ghostty 绑定路径 ⌘W（close_surface）+ ⌘N 多窗"
start_app c split,closebinding
: > "$ZOOMF"
dump c1 c-step1.json
assert_json "$EV/c-step1.json" "len(d['leaves'])==1" "C1 selftest split,closebinding → 绑定路径只关一面（leaves=1）"
kill -0 $APP_PID 2>/dev/null && ok "C1 窗口存活（close_surface_cb 多 pane 不关窗）" || bad "C1 绑定路径把窗关了"
grep -q "close_surface_cb alive=true process_alive=true" "$LOGDIR/c.log" && ok "C1 close_surface_cb(process_alive=true) 日志" || bad "C1 缺 close_surface_cb 日志"
# C2 bindact:close_surface（= 键位绑定同流）多 pane 只关一面
zoom split; zoom "bindact:close_surface"
dump c2 c-step2.json
assert_json "$EV/c-step2.json" "len(d['leaves'])==1" "C2 bindact:close_surface 多 pane → leaves=1"
# C3 ⌘N 真键（NEW_WINDOW）→ 窗口数 1→2
$SYNTH key $APP_PID 45 cmd; sleep 1.0
WINS=$(wincount $APP_PID)
assert_eq "C3 ⌘N 真键 → 窗口数" 2 "$WINS"
dump c3 c-step3-newwin.json
assert_json "$EV/c-step3-newwin.json" "len(d['leaves'])==1" "C3 新窗单 pane（inherited_config WINDOW）"
cp "$LOGDIR/c.log" "$EV/closebinding.log"
stop_app

# ===========================================================================
say "D ⌘W 单 pane：关 tab → 关窗（⌘T 真键）"
start_app d
: > "$ZOOMF"
$SYNTH key $APP_PID 17 cmd; sleep 1.0   # ⌘T New Tab（菜单 newWindowForTab:）
assert_eq "D1 ⌘T 后窗口数（tab 并入）" 1 "$(wincount $APP_PID)"
dump d1 d-step1.json
assert_json "$EV/d-step1.json" "len(d['leaves'])==1" "D1 当前 tab 单 pane"
# D2 ⌘W：关当前 tab（还有 1 个 tab → 窗口/进程存活）
$SYNTH key $APP_PID 13 cmd; sleep 1.0
if kill -0 $APP_PID 2>/dev/null; then ok "D2 ⌘W 关 tab（非最后 tab → 窗口存活）"; else bad "D2 ⌘W 把窗口关了（应只关 tab）"; fi
assert_eq "D2 窗口数" 1 "$(wincount $APP_PID)"
# D3 再 ⌘W：最后 tab → 关窗 → 退出
$SYNTH key $APP_PID 13 cmd; sleep 2.0
if kill -0 $APP_PID 2>/dev/null; then bad "D3 最后 tab ⌘W 后进程未退出"; else ok "D3 ⌘W 最后 tab → 关窗 → 进程退出"; fi
cp "$LOGDIR/d.log" "$EV/tab-close.log"
APP_PID=""

# ===========================================================================
say "E q0 取证模式回归（--evidence-dir）"
rm -rf /tmp/nq1-q0ev
NINJA_E2E_SCREEN="$DISPLAY_ID" "$BIN" --evidence-dir /tmp/nq1-q0ev > "$LOGDIR/q0-regression.log" 2>&1
Q0_RC=$?
[ "$Q0_RC" -eq 0 ] && ok "E1 q0 模式 exit 0" || bad "E1 q0 模式 exit $Q0_RC"
grep -qi "overall: pass" /tmp/nq1-q0ev/report.txt && ok "E2 q0 report.txt overall: PASS（5 检查项）" || bad "E2 q0 报告非 PASS"
cp /tmp/nq1-q0ev/report.txt "$EV/q0-regression-report.txt"

# ===========================================================================
say "汇总"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "OVERALL: PASS" || echo "OVERALL: FAIL"
exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)
