#!/bin/bash
# q2 E2E：配置系统验收取证（产物写入本目录，可重复执行覆盖）。
#
# 验收（PLAN q2「过」：用户既有 ghostty 配置的常用子集直接生效；主题/
# 字体/键位实测 + 热重载 + ninja.toml 只管插件/宿主特有项 + ODP 缺省）→ 测试：
#   A ODP 缺省主题（无用户配置 → bg #282c34/fg #abb2bf/ANSI16，dump + 像素）
#   B 用户既有 ghostty 配置常用子集直接生效（macOS 正宗位置 App Support：
#     theme=Dracula + font-size 18 + keybind 重绑；dump + 像素）
#   C 键位全量继承 + 热重载（⌘G 重绑 new_split:right 真键生效；改文件后
#     ① mtime 监视 ② ⌘⇧,/reload_config action 两条路径都重载；#ff00ff
#     颜色传播到像素）
#   D ninja 特有动作重绑（⌘,=ignore + ⌘⇧P=toggle_visibility：旧键失效、
#     新键 action 到宿主 dispatch，全走 ghostty keybind 配置）
#   E ninja.toml 收缩（v1 终端项/[keys] 警告忽略、[plugins] 只解析不拉起、
#     零插件进程/零 socket）
#   F q0 取证模式回归 PASS（--evidence-dir）
#
# 全程虚拟屏（PLAN「E2E 虚拟屏幕」）：hold → NINJA_E2E_SCREEN=<displayID>
# → 取证 → kill hold；拿不到 displayID 即中止（不落主屏）。真键经
# synth_input（CGEventPostToPid，不抢开发者焦点，q1 证据脚本同款）；
# 像素探针 = screencapture -l <windowID> + probe_window.swift（q0 位图采样
# 同款路径）。断言全部跑命令/读 JSON/读像素，不信实施者的自我声明。
#
# 隔离：macOS App Support 配置路径不走 HOME env（NSSearchPath 实测），
# 隔离测试把真实配置临时移开（trap 恢复）；XDG_CONFIG_HOME 指向临时目录。
# 像素探针走显示色空间（sRGB→P3 系统性压暗 ~10%），绝对色断言容差 ±10，
# 颜色传播断言用强对比色（#ff00ff）。
set -u
cd "$(dirname "$0")/../.."
cargo build --release -p ninja >/dev/null 2>&1 || { echo "FATAL: build 失败"; exit 1; }
BIN=./target/release/ninja
EV=docs/q2-evidence
SYNTH_SRC=docs/q1-evidence/synth_input.swift
SYNTH=/tmp/nq2-synth
PROBE_SRC=docs/q2-evidence/probe_window.swift
PROBE=/tmp/nq2-probe
HOLD_JSON=/tmp/nq2-hold.json
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
  if [ "${got:-}" = "True" ]; then ok "$3"; else bad "${3}（实得: ${got:-}）"; fi
}
zoom() { echo "$1" > "$ZOOMF"; sleep 0.7; }

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
  [ -n "${APP_PID:-}" ] && { kill "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null; APP_PID=""; }
  [ -n "$HOLD_PID" ] && kill "$HOLD_PID" 2>/dev/null
  restore_cfg
}
trap cleanup EXIT

scripts/e2e/virtual-display hold 1440 900 0 > "$HOLD_JSON" 2>/tmp/nq2-hold.err &
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

# ---- 编译输入驱动 + 像素探针 -----------------------------------------------
swiftc -O "$SYNTH_SRC" -o "$SYNTH" 2>/dev/null || { echo "FATAL: synth 编译失败"; exit 1; }
swiftc -O "$PROBE_SRC" -o "$PROBE" 2>/dev/null || { echo "FATAL: probe 编译失败"; exit 1; }
if [ "$($SYNTH trust)" != "trusted" ]; then
  echo "FATAL: 合成键盘事件需要辅助功能授权（TCC）"; exit 1
fi

wincount() { $SYNTH wins "$1" | python3 -c "import json,sys;print(sum(1 for w in json.load(sys.stdin) if w['layer']==0))"; }
winid()    { $SYNTH wins "$1" | python3 -c "import json,sys;print([w['id'] for w in json.load(sys.stdin) if w['layer']==0][0])"; }
winbounds(){ $SYNTH wins "$1" | python3 -c "import json,sys;print(' '.join(str(v) for v in [w for w in json.load(sys.stdin) if w['layer']==0][0]['bounds']))"; }
shot()     { [ -n "${2:-}" ] && screencapture -x -l "$2" "$EV/$1" >/dev/null 2>&1; }
# 像素探针：窗口相对区域平均 RGB（0.55..0.95 × 0.30..0.50，终端中部
# 空白背景区；实测窗口底部 ~15% 是暗带（surface 几何伪影，两配置同在），
# 顶部有 prompt/标题栏，中部才是纯背景；px 尺寸回显进 *.txt）。
probe_px() { # tag → stdout "r,g,b"（完整 JSON 追加进 $EV/<tag>-pixel.txt）
  local wid png out
  wid=$(winid "$APP_PID") || true
  [ -n "${wid:-}" ] || { echo "0,0,0"; return; }
  png="/tmp/nq2-shot-$1.png"
  screencapture -x -l "$wid" "$png" >/dev/null 2>&1
  out=$("$PROBE" avg "$png" 0.55 0.30 0.40 0.20 2>/dev/null || echo '{"avg":[0,0,0],"px":[0,0]}')
  echo "$out" >> "$EV/$1-pixel.txt"
  echo "$out" | python3 -c "import json,sys;print(','.join(map(str,json.load(sys.stdin)['avg'])))" 2>/dev/null || echo "0,0,0"
}
assert_px() { # tag desc r g b（±容差 10）
  local rgb; rgb=$(probe_px "$1")
  if python3 -c "import sys;r,g,b=${rgb:-0,0,0};sys.exit(0 if abs(r-$3)<=10 and abs(g-$4)<=10 and abs(b-$5)<=10 else 1)" 2>/dev/null; then
    ok "${2}（像素 ${rgb}，期望 [$3,$4,$5]±10）"
  else
    bad "$2（像素 ${rgb:-空} ≠ [$3,$4,$5]±10）"
  fi
}

# ---- App Support 真实配置隔离 ----------------------------------------------
isolate() { # 移开真实配置（A/C/D 需要；B 写测试配置前备份）
  for f in "$REAL_CFG" "$REAL_TPL"; do
    if [ -f "$f" ]; then
      local bak; bak=$(mktemp /tmp/nq2-cfg.XXXXXX)
      mv "$f" "$bak"
      BACKUPS+=("$f|$bak")
    fi
  done
  echo "(App Support 真实配置已暂存 ${#BACKUPS[@]} 个)"
}

# ---- 宿主进程管理 -----------------------------------------------------------
start_app() { # $1=tag $2=键盘探子(1/0)
  # 注：不 rm -rf 标签目录——调用方可能已把 XDG 配置写在里面（q2 首跑
  # 教训：删了配置导致 C/D 组装载不到用户配置）；只清运行产物。
  mkdir -p "/tmp/nq2-$1"
  ZOOMF="/tmp/nq2-$1/zoom"; ZOUT="/tmp/nq2-$1/zoomdump.json"
  : > "$ZOOMF"   # 清残留指令，防止新进程首拍重放上一轮的 zoom 动作
  export NINJA_E2E_SCREEN="$DISPLAY_ID" NINJA_ZOOM_FILE="$ZOOMF" NINJA_ZOOM_DUMP="$ZOUT"
  ( "$BIN" > "$LOGDIR/$1.log" 2>&1 & echo $! > /tmp/nq2-app.pid )
  for _ in $(seq 40); do grep -q "q2 shell" "$LOGDIR/$1.log" 2>/dev/null && break; sleep 0.25; done
  APP_PID=$(cat /tmp/nq2-app.pid)
  sleep 2.0
  # 窗必须落在主屏之外（E2E 纪律：不落主屏；等窗服排位稳定）。
  for _ in $(seq 6); do
    WX=$(winbounds "$APP_PID" | cut -d' ' -f1)
    case "${WX:-1}" in
      -*|1[5-9][0-9][0-9]|2[0-9][0-9][0-9]) break ;;
    esac
    sleep 0.5
  done
  if [ "${2:-0}" = "1" ]; then
    # 键盘就绪探针：普通键必须在日志出现 keyDown（app 激活/焦点就位）。
    $SYNTH type "$APP_PID" "z" >/dev/null 2>&1; sleep 0.3
    for _ in $(seq 10); do
      grep -q "keyDown code=6" "$LOGDIR/$1.log" 2>/dev/null && break
      $SYNTH type "$APP_PID" "z" >/dev/null 2>&1; sleep 0.4
    done
    grep -q "keyDown code=6" "$LOGDIR/$1.log" 2>/dev/null || echo "WARN: $1 键盘探针未命中（后续键事件可能丢失）"
  fi
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
say "A ODP 缺省主题（隔离：无任何用户配置）"
rm -rf /tmp/nq2-a; mkdir -p /tmp/nq2-a/xdg
isolate
export XDG_CONFIG_HOME=/tmp/nq2-a/xdg NINJA_CFG_DUMP=/tmp/nq2-a/dump.json NINJA_Q1_DEBUG=1
unset NINJA_CONFIG NINJA_P2_SELFTEST
start_app a 0
zoom cfgdump
assert_json /tmp/nq2-a/dump.json "d['odp_applied'] and d['user_theme']==False" "A1 ODP 层装载（user_theme=False）"
assert_json /tmp/nq2-a/dump.json "d['background']==[40,44,52]" "A2 background=#282c34（ODP）"
assert_json /tmp/nq2-a/dump.json "d['foreground']==[171,178,191]" "A3 foreground=#abb2bf（ODP）"
assert_json /tmp/nq2-a/dump.json "d['palette16'][0]==[63,68,81] and d['palette16'][15]==[230,230,230]" "A4 ANSI16=ODP 钉值"
assert_px a "A5 像素背景 ≈ #282c34" 40 44 52
# q0 审计遗留记录（不阻塞）：app 级句柄读 link-previews 的回读怪象。
echo "  (记录) link_previews_readback=$(python3 -c "import json;print(json.load(open('/tmp/nq2-a/dump.json'))['link_previews_readback'])")（q0 审计怪象：surface 层动作实际放行）"
shot shot-odp-default.png "$(winid "$APP_PID")"
cp /tmp/nq2-a/dump.json "$EV/a-dump.json"
stop_app
restore_cfg

# ===========================================================================
say "B 用户既有配置常用子集直接生效（App Support 正宗位置：theme/字号/键位）"
rm -rf /tmp/nq2-b; mkdir -p /tmp/nq2-b/xdg
isolate
cat > "$REAL_CFG" <<'EOF'
theme = Dracula
font-size = 18
keybind = super+shift+o=new_split:right
EOF
export XDG_CONFIG_HOME=/tmp/nq2-b/xdg NINJA_CFG_DUMP=/tmp/nq2-b/dump.json NINJA_Q1_DEBUG=1
start_app b 0
zoom cfgdump
assert_json /tmp/nq2-b/dump.json "d['user_theme']==True and d['odp_applied']==False" "B1 theme=Dracula 探测 → ODP 让位"
assert_json /tmp/nq2-b/dump.json "d['background']==[40,42,54] and d['foreground']==[248,248,242]" "B2 Dracula bg/fg 生效（具名主题真实解析）"
assert_json /tmp/nq2-b/dump.json "d['palette16'][0]==[33,34,44]" "B3 Dracula ANSI 生效（themes 资源目录）"
assert_json /tmp/nq2-b/dump.json "d['font_size']==18" "B4 用户 font-size=18 生效"
assert_json /tmp/nq2-b/dump.json "d['triggers']['new_split:right']==\"shift+super+'o'\"" "B5 用户 keybind 重绑生效（⌘⇧O→new_split:right）"
assert_px b "B6 像素背景 ≈ Dracula #282a36" 40 42 54
shot shot-user-dracula.png "$(winid "$APP_PID")"
cp /tmp/nq2-b/dump.json "$EV/b-dump.json"
stop_app
rm -f "$REAL_CFG"   # 测试配置不是用户文件：移除后再恢复备份
restore_cfg

# ===========================================================================
say "C 键位继承 + 热重载（隔离：XDG 配置驱动）"
rm -rf /tmp/nq2-c; mkdir -p /tmp/nq2-c/xdg/ghostty
isolate
CFG=/tmp/nq2-c/xdg/ghostty/config
# ⌘G 重绑 new_split:right：非默认键，证明用户 ghostty keybind 改绑直接生效
#（菜单 keyEquivalent 同源跟随）。键事件到达路径：菜单键等价物（与
# trigger 同源）→ binding_action → 同一 action 核心（performBindingAction）。
printf 'font-size = 24\nkeybind = super+g=new_split:right\n' > "$CFG"
export XDG_CONFIG_HOME=/tmp/nq2-c/xdg NINJA_CFG_DUMP=/tmp/nq2-c/dump.json NINJA_Q1_DEBUG=1
start_app c 1
zoom cfgdump
assert_json /tmp/nq2-c/dump.json "d['font_size']==24" "C0 启动即装载 XDG 用户配置（font-size=24）"
assert_json /tmp/nq2-c/dump.json "d['triggers']['new_split:right']==\"super+'g'\"" "C1 重绑生效：⌘G→new_split:right（trigger 表）"
# 真键 ⌘G（CGKeyCode 5）→ new_split:right（ghostty action 路径）。
$SYNTH key "$APP_PID" 5 cmd >/dev/null 2>&1; sleep 1.2
zoom dump2
assert_eq "C2 真键 ⌘G 触发 new_split:right（leaves）" "2" "$(python3 -c "import json;print(len(json.load(open('$ZOUT'))['leaves']))")"
grep -q "action tag=4 " "$LOGDIR/c.log" && ok "C3 NEW_SPLIT action 经 action_cb 分发（c.log）" || bad "C3 NEW_SPLIT action 日志缺失"
# 热重载①（mtime 监视）：⌘G 改绑 decrease_font_size:2（带参数——
# ghostty 动作语义：decrease_font_size 需显式步长，无参写法 InvalidFormat
# 进诊断；不在菜单镜像里——证明非镜像动作也走 ghostty 绑定系统）；
# split 绑定随重载消失回默认 ⌘D。
printf 'font-size = 24\nkeybind = super+g=decrease_font_size:2\n' > "$CFG"
sleep 2.0
zoom cfgdump
assert_json /tmp/nq2-c/dump.json "d['triggers']['new_split:right']==\"super+'d'\"" "C4 mtime 热重载：⌘G 不再绑 split（回默认 ⌘D，配置自默认表重建）"
# 旧键失效实测：真键 ⌘G 不再分屏（leaves 不变）。
$SYNTH key "$APP_PID" 5 cmd >/dev/null 2>&1; sleep 1.0
zoom dump3
assert_eq "C5 旧绑定失效：⌘G 不再分屏" "2" "$(python3 -c "import json;print(len(json.load(open('$ZOUT'))['leaves']))")"
COLS1=$(python3 -c "import json;d=json.load(open('$ZOUT'));print(sum(l['cols'] for l in d['leaves']))")
# 新动作经绑定系统核心（performBindingAction）触发：字号缩小 2pt → 列数变多。
zoom "bindact:decrease_font_size:2"
sleep 1.0
zoom dump4
COLS2=$(python3 -c "import json;d=json.load(open('$ZOUT'));print(sum(l['cols'] for l in d['leaves']))")
if [ "${COLS2:-0}" -gt "${COLS1:-0}" ]; then
  ok "C6 新绑定动作生效 decrease_font_size（cols ${COLS1:-?}→${COLS2:-?}）"
else
  bad "C6 decrease_font_size 未生效（cols ${COLS1:-?}→${COLS2:-?}）"
fi
# ⌘T（默认 new_tab）真键：菜单镜像路径（File>New Tab ⌘T 等价物，keyEquivalent
# 从 trigger 推导 → binding_action(new_tab) → action_cb NEW_TAB）。
$SYNTH key "$APP_PID" 17 cmd >/dev/null 2>&1; sleep 1.2
grep -q "menu→binding_action(new_tab)" "$LOGDIR/c.log" && ok "C7 真键 ⌘T 经菜单等价物→binding_action(new_tab)（派生键位）" || bad "C7 ⌘T 菜单等价物日志缺失"
grep -q "action tag=2 " "$LOGDIR/c.log" && ok "C7b NEW_TAB action 到 dispatch（c.log）" || bad "C7b NEW_TAB action 日志缺失"
# 热重载②：⌘⇧,（reload_config action）路径（zoom 钩子 reloadcfg 同途）。
# 同时换背景色 #ff00ff——热重载颜色传播到像素（强对比，免疫显示色空间）。
printf 'font-size = 20\nbackground = #ff00ff\n' > "$CFG"
zoom reloadcfg
sleep 1.5
zoom cfgdump
assert_json /tmp/nq2-c/dump.json "d['font_size']==20" "C8 reload_config action 路径热重载（⌘⇧, 同途）"
assert_json /tmp/nq2-c/dump.json "d['background']==[255,0,255]" "C8b 重载后 background=#ff00ff（dump）"
assert_px c "C10 热重载像素：背景 → 品红（强对比）" 255 0 255
grep -q "配置已重载" "$LOGDIR/c.log" && ok "C9 重载日志（c.log「配置已重载」）" || bad "C9 重载日志缺失"
grep -q "CONFIG_CHANGE" "$LOGDIR/c.log" && ok "C9b CONFIG_CHANGE 回调（update_config 传播）" || bad "C9b CONFIG_CHANGE 日志缺失"
cp /tmp/nq2-c/dump.json "$EV/c-dump.json"; cp "$ZOUT" "$EV/c-zoom.json"
stop_app
restore_cfg

# ===========================================================================
say "D ninja 特有动作重绑（⌘,=ignore、⌘⇧P=toggle_visibility）"
rm -rf /tmp/nq2-d; mkdir -p /tmp/nq2-d/xdg/ghostty
isolate
CFG=/tmp/nq2-d/xdg/ghostty/config
printf 'keybind = super+,=ignore\nkeybind = super+shift+p=toggle_visibility\n' > "$CFG"
export XDG_CONFIG_HOME=/tmp/nq2-d/xdg NINJA_CFG_DUMP=/tmp/nq2-d/dump.json NINJA_Q1_DEBUG=1
start_app d 1
zoom cfgdump
assert_json /tmp/nq2-d/dump.json "d['triggers']['toggle_visibility']==\"shift+super+'p'\"" "D1 toggle_visibility 重绑 ⌘⇧P（宿主层 ⌘, 认领被用户覆盖）"
# 旧键 ⌘,（43）已被 ignore：TOGGLE_VISIBILITY 不进 dispatch。
$SYNTH key "$APP_PID" 43 cmd >/dev/null 2>&1; sleep 1.0
if grep -q "toggle_visibility 收到" "$LOGDIR/d.log"; then
  bad "D2 旧键 ⌘, 仍触发 toggle_visibility（应 ignore）"
else
  ok "D2 旧键 ⌘, 失效（ignore）"
fi
# 新键 ⌘⇧P（35）→ binding → action_cb TOGGLE_VISIBILITY → 宿主 dispatch。
$SYNTH key "$APP_PID" 35 cmd,shift >/dev/null 2>&1; sleep 1.0
grep -q "toggle_visibility 收到" "$LOGDIR/d.log" && ok "D3 新键 ⌘⇧P → TOGGLE_VISIBILITY action 到宿主 dispatch（d.log）" || bad "D3 ⌘⇧P dispatch 日志缺失"
# 绑定系统直证：zoom 钩子 panel → binding_action(toggle_visibility)（不经
# 菜单；q2 dispatch 记日志，面板 UI 是 q3 交付）。
zoom panel
sleep 0.8
N=$(grep -c "toggle_visibility 收到" "$LOGDIR/d.log")
assert_eq "D4 binding_action(toggle_visibility) 直驱 dispatch（累计次数）" "2" "$N"
cp /tmp/nq2-d/dump.json "$EV/d-dump.json"
stop_app
restore_cfg

# ===========================================================================
say "E ninja.toml 收缩（v1 键警告忽略；[plugins] 只解析不拉起）"
rm -rf /tmp/nq2-e; mkdir -p /tmp/nq2-e/xdg
NT=/tmp/nq2-e/ninja.toml
cat > "$NT" <<'EOF'
shell = "/bin/zsh"
font-family = "Menlo"
font-size = 14.0

[theme]
cursor = "#528BFF"

[keys]
new_window = "cmd+n"

[plugins]
enabled = ["preview", "theme"]
[plugins.paths]
theme = "/usr/local/bin/ninja-theme"
EOF
export XDG_CONFIG_HOME=/tmp/nq2-e/xdg NINJA_CONFIG="$NT" NINJA_CFG_DUMP=/tmp/nq2-e/dump.json
unset NINJA_Q1_DEBUG NINJA_ZOOM_FILE NINJA_ZOOM_DUMP
start_app e 0
grep -q "\[keys\] 已收缩" "$LOGDIR/e.log" && ok "E1 [keys] 警告（语义不复活）" || bad "E1 [keys] 警告缺失"
grep -qE "(shell|font-family|font-size).{0,40}已收缩" "$LOGDIR/e.log" && ok "E2 终端项警告（走 ghostty 配置）" || bad "E2 终端项警告缺失"
grep -q "\`theme\` 已收缩" "$LOGDIR/e.log" && ok "E2b [theme] 段警告" || bad "E2b [theme] 警告缺失"
grep -q "仅解析" "$LOGDIR/e.log" && ok "E2c [plugins] 只解析提示" || bad "E2c 提示缺失"
assert_json /tmp/nq2-e/dump.json "d['plugins_enabled']==['preview','theme']" "E3 [plugins] 解析收下"
# 空载红线：无插件进程；宿主 unix socket 零。
if ! pgrep -laf "ninja-preview|ninja-theme" | grep -v grep; then ok "E4 零插件进程（q2 不拉起）"; else bad "E4 出现插件进程"; fi
LSOF=$(lsof -a -U -p "$APP_PID" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
assert_eq "E5 宿主 unix socket 数（零插件 socket）" 0 "$LSOF"
cp /tmp/nq2-e/dump.json "$EV/e-dump.json"
stop_app
unset NINJA_CONFIG

# ===========================================================================
say "F q0 取证模式回归（--evidence-dir）"
rm -rf /tmp/nq2-f
unset NINJA_CFG_DUMP NINJA_ZOOM_FILE NINJA_ZOOM_DUMP
"$BIN" --evidence-dir /tmp/nq2-f > "$LOGDIR/f.log" 2>&1
Q0_RC=$?
[ "$Q0_RC" -eq 0 ] && ok "F1 q0 模式 exit 0" || bad "F1 q0 模式 exit $Q0_RC"
grep -qi "overall: pass" /tmp/nq2-f/report.txt && ok "F2 q0 report.txt overall: PASS" || bad "F2 q0 报告非 PASS"
cp /tmp/nq2-f/report.txt "$EV/f-report.txt"

# ===========================================================================
say "结果：PASS=$PASS FAIL=$FAIL"
echo "PASS=$PASS FAIL=$FAIL" | tee "$EV/e2e-summary.txt"
[ "$FAIL" = "0" ] && echo "OVERALL: PASS" | tee -a "$EV/e2e-summary.txt" || echo "OVERALL: FAIL" | tee -a "$EV/e2e-summary.txt"
exit $([ "$FAIL" = "0" ] && echo 0 || echo 1)
