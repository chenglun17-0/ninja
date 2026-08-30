#!/bin/bash
# q2 E2E：配置系统验收取证（产物写入本目录，可重复执行覆盖）。
#
# 验收项 → 测试：
#   A ODP 缺省主题（无用户配置 → bg #282c34/fg #abb2bf/ANSI16，dump + 像素）
#   B 用户既有 ghostty 配置常用子集直接生效（theme=Dracula + 字号 18，
#     真实配置不隔离；dump + 像素）
#   C 键位全量继承 + 热重载（⌘T 重绑 new_split:right 真键生效；改文件后
#     ① mtime 监视 ② ⌘⇧, reload_config action 两条路径都重载；颜色传播像素）
#   D ninja 特有动作重绑（⌘,=ignore + ⌘⇧P=toggle_visibility：旧键失效、
#     新键开面板，全走 ghostty keybind 配置）
#   E ninja.toml 收缩（v1 终端项/[keys] 警告忽略、[plugins] 只解析不拉起）
#   F q0 取证模式回归
#
# 隔离：macOS App Support 配置路径不走 HOME env（NSSearchPath 实测），
# 隔离测试把真实配置临时移开（trap 恢复）；XDG_CONFIG_HOME 指向临时目录。
# 像素探针走 screencapture（显示色空间，sRGB→P3 系统性压暗 ~10%），
# 绝对色断言容差 ±10，颜色传播断言用强对比色（#ff00ff）。
set -u
cd "$(dirname "$0")/../.."
BIN=./target/release/ninja-embed
EV=docs/q2-evidence
SYNTH="swift tools/verify/synth_input.swift"
SHOT="swift tools/verify/shot_window.swift"
REAL_CFG="$HOME/Library/Application Support/com.mitchellh.ghostty/config"
BACKUP=""
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
probe_px() { # x y w h → stdout avg；PID 全局（$SHOT 不加引号：要按词拆分）
  $SHOT probe "$PID" "$1" "$2" "$3" "$4" 2>/dev/null \
    | python3 -c "import json,sys;print(json.load(sys.stdin)['avg'])" 2>/dev/null
}
restore_cfg() {
  if [ -n "$BACKUP" ]; then
    mv -f "$BACKUP" "$REAL_CFG"; BACKUP=""
    echo "(真实用户配置已恢复)"
  fi
}
trap restore_cfg EXIT
isolate() { # 移开真实配置（A/C/D 需要；B 用真实配置）
  if [ -f "$REAL_CFG" ]; then
    BACKUP=$(mktemp /tmp/nq2-cfg.XXXXXX)
    mv "$REAL_CFG" "$BACKUP"
    rm -f "${REAL_CFG}.ghostty" # loadDefaultFiles 无配置时会写模板，一并挪走
    echo "(真实用户配置暂存 ${BACKUP})"
  fi
}

start_app() { # 环境变量由调用方先 export（不用 eval——$! 必须是二进制本体）
  PID=""
  ( "$BIN" > "$EV/$1.log" 2>&1 & echo $! > /tmp/nq2.pid )
  for _ in $(seq 40); do
    grep -q "q2 shell" "$EV/$1.log" 2>/dev/null && break; sleep 0.25
  done
  PID=$(cat /tmp/nq2.pid)
  sleep 1.2
}
stop_app() {
  [ -n "${PID:-}" ] || return 0
  kill "$PID" 2>/dev/null
  for _ in $(seq 30); do kill -0 "$PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$PID" 2>/dev/null
  sleep 0.5
}
zoom() { # cmd → 写 zoom 钩子并等去抖
  echo "$1" > "$ZOOMF"; sleep 0.6
}

pkill -9 -f "target/release/ninja-embed" 2>/dev/null; sleep 0.3

# ---------------------------------------------------------------------------
say "A ODP 缺省主题（隔离：无任何用户配置）"
rm -rf /tmp/nq2-a; mkdir -p /tmp/nq2-a/xdg
isolate
ZOOMF=/tmp/nq2-a/zoom; DUMP=/tmp/nq2-a/dump.json; ZOUT=/tmp/nq2-a/zoomdump.json
export XDG_CONFIG_HOME=/tmp/nq2-a/xdg NINJA_CFG_DUMP=$DUMP \
       NINJA_ZOOM_FILE=$ZOOMF NINJA_ZOOM_DUMP=$ZOUT NINJA_Q1_DEBUG=1
start_app a
zoom cfgdump
assert_json "$DUMP" "d['odp_applied'] and d['user_theme']==False" "A1 ODP 层装载（user_theme=False）"
assert_json "$DUMP" "d['background']==[40,44,52]" "A2 background=#282c34（ODP）"
assert_json "$DUMP" "d['foreground']==[171,178,191]" "A3 foreground=#abb2bf（ODP）"
assert_json "$DUMP" "d['palette16'][0]==[63,68,81] and d['palette16'][15]==[230,230,230]" "A4 ANSI16=ODP 钉值"
PX=$(probe_px 0.30 0.55 0.40 0.20)
echo "$PX" > "$EV/a-pixel.txt"
if python3 -c "import sys;r,g,b=${PX:-0,0,0};sys.exit(0 if abs(r-40)<=10 and abs(g-44)<=10 and abs(b-52)<=10 and b>r else 1)" 2>/dev/null; then
  ok "A5 像素背景 ≈ #282c34（显示空间 ${PX}）"
else
  bad "A5 像素背景 ${PX:-空} ≠ [40,44,52]±10 且 b>r"
fi
cp "$DUMP" "$EV/a-dump.json"
stop_app
restore_cfg

# ---------------------------------------------------------------------------
say "B 用户既有配置直接生效（真实配置：theme=Dracula + JetBrainsMono 18）"
[ -f "$REAL_CFG" ] || bad "B0 真实配置缺位（${REAL_CFG}）"
rm -rf /tmp/nq2-b; mkdir -p /tmp/nq2-b
ZOOMF=/tmp/nq2-b/zoom; DUMP=/tmp/nq2-b/dump.json; ZOUT=/tmp/nq2-b/zoomdump.json
export XDG_CONFIG_HOME=/tmp/nq2-b/xdg NINJA_CFG_DUMP=$DUMP \
       NINJA_ZOOM_FILE=$ZOOMF NINJA_ZOOM_DUMP=$ZOUT
mkdir -p /tmp/nq2-b/xdg # 空目录：XDG 无配置，真实 App Support 配置仍装载
start_app b
zoom cfgdump
assert_json "$DUMP" "d['user_theme']==True and d['odp_applied']==False" "B1 theme=Dracula 探测 → ODP 让位"
assert_json "$DUMP" "d['background']==[40,42,54] and d['foreground']==[248,248,242]" "B2 Dracula bg/fg 生效"
assert_json "$DUMP" "d['palette16'][0]==[33,34,44]" "B3 Dracula ANSI 生效（theme 资源目录）"
assert_json "$DUMP" "d['font_size']==18" "B4 用户 font-size=18 生效"
PX=$(probe_px 0.30 0.55 0.40 0.20)
echo "$PX" > "$EV/b-pixel.txt"
if python3 -c "import sys;r,g,b=${PX:-0,0,0};sys.exit(0 if abs(r-40)<=10 and abs(g-42)<=10 and abs(b-54)<=10 and b>r else 1)" 2>/dev/null; then
  ok "B5 像素背景 ≈ Dracula #282a36（显示空间 ${PX}）"
else
  bad "B5 像素背景 ${PX:-空} ≠ [40,42,54]±10"
fi
cp "$DUMP" "$EV/b-dump.json"
stop_app

# ---------------------------------------------------------------------------
say "C 键位继承 + 热重载（隔离：XDG 配置驱动）"
rm -rf /tmp/nq2-c; mkdir -p /tmp/nq2-c/xdg/ghostty
isolate
CFG=/tmp/nq2-c/xdg/ghostty/config
# ⌘G 重绑 new_split:right：非默认键，证明用户 ghostty keybind 改绑直接生效。
# 键事件到达路径：菜单键等价物（与 trigger 同源）→ binding_action → 同一
# action 核心（performBindingAction）；E2E 环境被 Orca 抢焦点（keyWindow
# 常空），菜单等价物路径不依赖窗口 key 态，实测稳定。
printf 'font-size = 24\nkeybind = super+g=new_split:right\n' > "$CFG"
ZOOMF=/tmp/nq2-c/zoom; DUMP=/tmp/nq2-c/dump.json; ZOUT=/tmp/nq2-c/zoomdump.json
export XDG_CONFIG_HOME=/tmp/nq2-c/xdg NINJA_CFG_DUMP=$DUMP \
       NINJA_ZOOM_FILE=$ZOOMF NINJA_ZOOM_DUMP=$ZOUT NINJA_Q1_DEBUG=1
start_app c
zoom cfgdump
assert_json "$DUMP" "d['triggers']['new_split:right']==\"super+'g'\"" "C1 重绑生效：⌘G→new_split:right（trigger 表）"
zoom dump1
# 基线取拆分后（C5 的 dump3）——分屏本身会把每叶列数减半，比较必须同几何。
$SYNTH activate "$PID" >/dev/null 2>&1; sleep 1.0
# 真键 ⌘G（CGKeyCode 5）→ new_split:right（ghostty action 路径）。
$SYNTH keypidcmd 5 "$PID" >/dev/null 2>&1; sleep 1.2
zoom dump2
assert_eq "C2 真键 ⌘G 触发 new_split:right（leaves）" "2" "$(python3 -c "import json;print(len(json.load(open('$ZOUT'))['leaves']))")"
grep -q "action tag=4 " "$EV/c.log" && ok "C3 NEW_SPLIT action 经 action_cb 分发（c.log）" || bad "C3 NEW_SPLIT action 日志缺失"
# 热重载①（mtime 监视）：⌘G 改绑 decrease_font_size（不在菜单镜像里——
# 证明非镜像动作也走 ghostty 绑定系统）；split 绑定随重载消失回默认 ⌘D。
printf 'font-size = 24\nkeybind = super+g=decrease_font_size\n' > "$CFG"
sleep 2.0
zoom cfgdump
assert_json "$DUMP" "d['triggers']['new_split:right']==\"super+'d'\"" "C4 mtime 热重载：⌘G 不再绑 split（回默认 ⌘D，配置自默认表重建）"
# 旧键失效实测：真键 ⌘G 不再分屏（leaves 不变）。
$SYNTH keypidcmd 5 "$PID" >/dev/null 2>&1; sleep 1.0
zoom dump3
assert_eq "C5 旧绑定失效：⌘G 不再分屏" "2" "$(python3 -c "import json;print(len(json.load(open('$ZOUT'))['leaves']))")"
COLS1=$(python3 -c "import json;d=json.load(open('$ZOUT'));print(sum(l['cols'] for l in d['leaves']))")
# 新动作经绑定系统核心（performBindingAction）触发：字号缩小 → 列数变多。
zoom "bindact:decrease_font_size:4"
sleep 1.0
zoom dump4
COLS2=$(python3 -c "import json;d=json.load(open('$ZOUT'));print(sum(l['cols'] for l in d['leaves']))")
if [ "${COLS2:-0}" -gt "${COLS1:-0}" ]; then
  ok "C6 新绑定动作生效 decrease_font_size（cols ${COLS1:-?}→${COLS2:-?}）"
else
  bad "C6 decrease_font_size 未生效（cols ${COLS1:-?}→${COLS2:-?}）"
fi
# ⌘T（默认 new_tab）真键：菜单镜像路径（File>New Tab ⌘T 等价物 →
# binding_action(new_tab)）。注：macOS 26 本机实测无菜单匹配的 ⌘T/⌘N
# 合成键会被系统吞掉（进不了 keyDown），菜单镜像路径不受影响。
WIN1=$($SYNTH wincount "$PID" 2>/dev/null | tail -1)
$SYNTH keypidcmd 17 "$PID" >/dev/null 2>&1; sleep 1.2
WIN2=$($SYNTH wincount "$PID" 2>/dev/null | tail -1)
if [ "${WIN2:-0}" -gt "${WIN1:-0}" ]; then ok "C7 真键 ⌘T new_tab（win ${WIN1:-?}→${WIN2:-?}）"; else bad "C7 ⌘T 未开新标签（${WIN1:-?}→${WIN2:-?}）"; fi
# 热重载②：⌘⇧,（reload_config action）路径（zoom 钩子 reloadcfg 同途）。
# 同时换背景色 #ff00ff——热重载颜色传播到像素（强对比，免疫显示色空间）。
printf 'font-size = 20\nbackground = #ff00ff\n' > "$CFG"
zoom reloadcfg
sleep 1.5
zoom cfgdump
assert_json "$DUMP" "d['font_size']==20" "C8 reload_config action 路径热重载（⌘⇧, 同途）"
PX=$(probe_px 0.30 0.55 0.40 0.20)
echo "$PX" > "$EV/c-pixel.txt"
if python3 -c "import sys;r,g,b=${PX:-0,0,0};sys.exit(0 if r>=140 and b>=140 and g<=80 else 1)" 2>/dev/null; then
  ok "C10 热重载像素：背景 → 品红（${PX}）"
else
  bad "C10 热重载像素未变品红（${PX:-空}）"
fi
grep -q "配置已重载" "$EV/c.log" && ok "C9 重载日志（c.log）" || bad "C9 重载日志缺失"
cp "$DUMP" "$EV/c-dump.json"; cp "$ZOUT" "$EV/c-zoom.json"
stop_app
restore_cfg

# ---------------------------------------------------------------------------
say "D ninja 特有动作重绑（⌘,=ignore、⌘⇧P=toggle_visibility）"
rm -rf /tmp/nq2-d; mkdir -p /tmp/nq2-d/xdg/ghostty
isolate
CFG=/tmp/nq2-d/xdg/ghostty/config
printf 'keybind = super+,=ignore\nkeybind = super+shift+p=toggle_visibility\n' > "$CFG"
ZOOMF=/tmp/nq2-d/zoom; DUMP=/tmp/nq2-d/dump.json; ZOUT=/tmp/nq2-d/zoomdump.json
export XDG_CONFIG_HOME=/tmp/nq2-d/xdg NINJA_CFG_DUMP=$DUMP \
       NINJA_ZOOM_FILE=$ZOOMF NINJA_ZOOM_DUMP=$ZOUT NINJA_Q1_DEBUG=1
start_app d
zoom cfgdump
assert_json "$DUMP" "d['triggers']['toggle_visibility']==\"shift+super+'p'\"" "D1 toggle_visibility 重绑 ⌘⇧P"
$SYNTH activate "$PID" >/dev/null 2>&1; sleep 0.6
WIN1=$($SYNTH wincount "$PID" 2>/dev/null | tail -1)
$SYNTH keypidcmd 43 "$PID" >/dev/null 2>&1; sleep 1.0 # ⌘,（43）已被 ignore
WIN2=$($SYNTH wincount "$PID" 2>/dev/null | tail -1)
assert_eq "D2 旧键 ⌘, 失效（ignore）" "${WIN1:-?}" "${WIN2:-?}"
$SYNTH keypidcmdshift 35 "$PID" >/dev/null 2>&1; sleep 1.0 # ⌘⇧P（35）→ 面板
WIN3=$($SYNTH wincount "$PID" 2>/dev/null | tail -1)
if [ "${WIN3:-0}" -gt "${WIN2:-0}" ]; then ok "D3 新键 ⌘⇧P 开面板（win ${WIN2:-?}→${WIN3:-?}）"; else bad "D3 ⌘⇧P 未开面板（${WIN2:-?}→${WIN3:-?}）"; fi
# 绑定系统直证：zoom 钩子 panel → binding_action(toggle_visibility) →
# action_cb dispatch（不经菜单拦截；面板已开，以 action 日志为证）。
zoom panel
sleep 0.8
grep -q "action tag=12 " "$EV/d.log" && ok "D4 TOGGLE_VISIBILITY action 到宿主 dispatch（d.log）" || bad "D4 action tag=12 日志缺失"
cp "$DUMP" "$EV/d-dump.json"
stop_app
restore_cfg

# ---------------------------------------------------------------------------
say "E ninja.toml 收缩（v1 键警告忽略；[plugins] 只解析不拉起）"
rm -rf /tmp/nq2-e; mkdir -p /tmp/nq2-e
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
EOF
DUMP=/tmp/nq2-e/dump.json
unset NINJA_ZOOM_FILE NINJA_ZOOM_DUMP NINJA_Q1_DEBUG
export XDG_CONFIG_HOME=/tmp/nq2-e/xdg NINJA_CONFIG=$NT NINJA_CFG_DUMP=$DUMP
mkdir -p /tmp/nq2-e/xdg
start_app e
grep -q "\[keys\] 已收缩" "$EV/e.log" && ok "E1 [keys] 警告（语义不复活）" || bad "E1 [keys] 警告缺失"
grep -qE "(shell|font-family|font-size|theme).{0,40}已收缩" "$EV/e.log" && ok "E2 终端项警告（走 ghostty 配置）" || bad "E2 终端项警告缺失"
grep -q "仅解析" "$EV/e.log" && ok "E2b [plugins] 只解析提示" || bad "E2b 提示缺失"
assert_json "$DUMP" "d['plugins_enabled']==['preview','theme']" "E3 [plugins] 解析收下"
if ! pgrep -laf "ninja-preview|ninja-theme" | grep -v grep; then ok "E4 零插件进程（q2 不拉起）"; else bad "E4 出现插件进程"; fi
cp "$DUMP" "$EV/e-dump.json"
stop_app
unset NINJA_CONFIG

# ---------------------------------------------------------------------------
say "F q0 取证模式回归"
rm -rf /tmp/nq2-f
unset NINJA_CFG_DUMP XDG_CONFIG_HOME
"$BIN" --evidence-dir /tmp/nq2-f > "$EV/f.log" 2>&1
if grep -qi "overall: pass" /tmp/nq2-f/report.txt 2>/dev/null; then ok "F1 q0 report PASS"; else bad "F1 q0 report 缺失/失败"; fi

# ---------------------------------------------------------------------------
say "结果：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] || exit 1
