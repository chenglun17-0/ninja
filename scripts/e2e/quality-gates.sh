#!/usr/bin/env bash
# 质量门：PLAN.md「质量门」五条的常驻回归，一条命令。
#
#   G0 纯逻辑     cargo test（协议契约/golden + 监督器生命周期单测）
#   G1 协议卫生   ninja-protocol 依赖树无宿主/ghostty-sys；golden 逐个可解
#   G2 内核无名词 非测试源码不出现插件名词（preview/editor/lsp；
#                 save/git 与宿主壳词汇冲突——window-save-state、
#                 session::save——不在本门，口径见 PLAN 内核定律）
#   G3 空载不变量 enabled 空 → 零 socket、零插件进程（GUI）
#   G4 启用即拉起/关掉即轻 面板钩子 on/off 全生命周期（GUI）
#   G5 崩溃隔离   SIGKILL 插件 → 宿主存活、socket 仍在、不自动重拉（GUI）
#
# GUI 门默认虚拟屏（virtual-display hold，不落主屏）；不可用按 PLAN 合同
# 回退主屏并标注。--no-gui 只跑 G0–G2。产物不落仓库（mktemp，退出即清）。
set -u
cd "$(dirname "$0")/../.."

NOGUI=0
[ "${1:-}" = "--no-gui" ] && NOGUI=1

BIN=./target/debug/ninja
WORK=$(mktemp -d /tmp/ninja-gates.XXXXXX)
LOGS=$WORK/logs
mkdir -p "$LOGS"
PASS=0
FAIL=0
APP_PID=""
HOLD_PID=""

say()  { printf '\n== %s\n' "$*"; }
ok()   { PASS=$((PASS + 1)); echo "  [PASS] $*"; }
bad()  { FAIL=$((FAIL + 1)); echo "  [FAIL] $*"; }
note() { echo "  [note] $*"; }

cleanup() {
  if [ -n "${APP_PID:-}" ]; then
    kill "$APP_PID" 2>/dev/null
    for _ in $(seq 30); do kill -0 "$APP_PID" 2>/dev/null || break; sleep 0.1; done
    kill -9 "$APP_PID" 2>/dev/null
    wait "$APP_PID" 2>/dev/null
  fi
  if [ -n "${HOLD_PID:-}" ]; then kill "$HOLD_PID" 2>/dev/null; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

wait_for() { # desc timeout_s cmd...（cmd 退出 0 = 达成）
  local desc=$1 t=$2
  shift 2
  local i
  for i in $(seq $((t * 5))); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 0.2
  done
  note "等待超时：""$desc""（${t}s）"
  return 1
}

# ---------------------------------------------------------------------------
# G0 纯逻辑
# ---------------------------------------------------------------------------
say "G0 纯逻辑（cargo test）"
if cargo test -p ninja-protocol -p ninja >"$LOGS/cargo-test.log" 2>&1; then
  ok "cargo test（协议契约 + 宿主单测）"
else
  bad "cargo test 失败（见 $LOGS/cargo-test.log，脚本退出后即清——重跑复现）"
fi

# ---------------------------------------------------------------------------
# G1 协议卫生
# ---------------------------------------------------------------------------
say "G1 协议卫生"
TREE=$(cargo tree -p ninja-protocol 2>/dev/null)
if printf '%s' "$TREE" | grep -qE 'ghostty-sys|/crates/ninja[) ]'; then
  bad "ninja-protocol 依赖树混入宿主/ghostty-sys"
  printf '%s\n' "$TREE" | grep -E 'ghostty-sys|/crates/ninja[) ]'
else
  ok "ninja-protocol 依赖树无宿主、无 ghostty-sys"
fi
GOLDEN_DIR=crates/ninja-protocol/tests/golden
GOLDEN_N=0
GOLDEN_BAD=0
for j in "$GOLDEN_DIR"/*.json; do
  [ -e "$j" ] || continue
  GOLDEN_N=$((GOLDEN_N + 1))
  python3 -c "import json;d=json.load(open('$j'));assert 'v' in d and 'type' in d" 2>/dev/null || {
    GOLDEN_BAD=$((GOLDEN_BAD + 1))
    bad "golden 不可解或缺 v/type：$j"
  }
done
[ "$GOLDEN_N" -gt 0 ] && [ "$GOLDEN_BAD" -eq 0 ] && ok "golden ${GOLDEN_N} 个全部带 v/type 可解"

# ---------------------------------------------------------------------------
# G2 内核无名词（非测试源码）
# ---------------------------------------------------------------------------
say "G2 内核无名词"
HITS=$(
  for f in crates/ninja/src/*.rs crates/ninja/src/plugins/*.rs crates/ninja-protocol/src/*.rs; do
    awk '/#\[cfg\(test\)\]/{exit} {print FILENAME":"FNR":"$0}' "$f"
  done | grep -E '\b(preview|editor|lsp)\b' |
    grep -vE 'link-previews|link_previews|editor\.(background|foreground)|editorCursor'
)
if [ -n "$HITS" ]; then
  bad "内核出现插件名词（插件名词不进内核，见 AGENTS/PLAN）："
  printf '%s\n' "$HITS"
else
  ok "非测试源码无 preview/editor/lsp（link-previews 键与 ODP 源键除外）"
fi

if [ "$NOGUI" = 1 ]; then
  note "--no-gui：跳过 G3–G5（GUI 门）"
  printf '\n== 汇总：PASS %d / FAIL %d\n' "$PASS" "$FAIL"
  [ "$FAIL" -eq 0 ]
  exit
fi

# ---------------------------------------------------------------------------
# GUI 前置：构建 + 清残留 + 虚拟屏
# ---------------------------------------------------------------------------
say "G3–G5 前置（构建 + 虚拟屏）"
cargo build -p ninja >/dev/null 2>&1 || { echo "FATAL: cargo build -p ninja 失败"; exit 1; }
scripts/e2e/reap.sh >/dev/null 2>&1 || true

VD_JSON=$WORK/vd.json
scripts/e2e/virtual-display hold 1440 900 0 >"$VD_JSON" 2>"$WORK/vd.err" &
HOLD_PID=$!
DISPLAY_ID=""
for _ in $(seq 20); do
  [ -s "$VD_JSON" ] && {
    DISPLAY_ID=$(python3 -c "import json;print(json.load(open('$VD_JSON'))['displayID'])" 2>/dev/null)
    break
  }
  sleep 0.3
done
if [ -n "$DISPLAY_ID" ]; then
  ok "虚拟屏就绪（displayID ""$DISPLAY_ID""，窗口不落主屏）"
  export NINJA_E2E_SCREEN="$DISPLAY_ID"
else
  kill "$HOLD_PID" 2>/dev/null
  HOLD_PID=""
  note "虚拟屏不可用 → 按 PLAN 合同回退主屏（本行即标注）"
fi

start_host() { # tag → APP_PID / $LOGS/$tag.log；就绪探子 = 「q2 shell」启动行（恒打印）
  local tag=$1
  mkdir -p "$WORK/$tag-xdg"
  : > "$WORK/$tag-panel"
  env \
    XDG_CONFIG_HOME="$WORK/$tag-xdg" \
    NINJA_CONFIG="$WORK/$tag-ninja.toml" \
    NINJA_ADE_SOCK="$WORK/$tag-ade.sock" \
    NINJA_ADE_DEBUG=1 \
    NINJA_PANEL_PLUGIN_FILE="$WORK/$tag-panel" \
    ${NINJA_E2E_SCREEN:+NINJA_E2E_SCREEN=$NINJA_E2E_SCREEN} \
    ${NINJA_PLUGIN_DIR:+NINJA_PLUGIN_DIR=$NINJA_PLUGIN_DIR} \
    "$BIN" >"$LOGS/$tag.log" 2>&1 &
  APP_PID=$!
  disown "$APP_PID" 2>/dev/null || true
  local _
  for _ in $(seq 40); do
    grep -q "q2 shell" "$LOGS/$tag.log" 2>/dev/null && return 0
    sleep 0.25
  done
  return 1
}

dump_tail() { # tag —— 失败诊断：日志尾部不随退出消失
  echo "  [tail] $LOGS/$1.log 最后 15 行："
  tail -15 "$LOGS/$1.log" 2>/dev/null | sed 's/^/    /'
}

stop_host() {
  [ -n "${APP_PID:-}" ] || return 0
  kill "$APP_PID" 2>/dev/null
  for _ in $(seq 30); do kill -0 "$APP_PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$APP_PID" 2>/dev/null
  wait "$APP_PID" 2>/dev/null
  APP_PID=""
  sleep 0.4
}

# ---------------------------------------------------------------------------
# G3 空载不变量
# ---------------------------------------------------------------------------
say "G3 空载不变量（enabled 空）"
printf '[plugins]\nenabled = []\n' > "$WORK/g3-ninja.toml"
rm -f "$WORK/g3-ade.sock"
if start_host g3; then
  sleep 1.2
  [ ! -e "$WORK/g3-ade.sock" ] && ok "零 socket" || bad "空载创建了 socket"
  if pgrep -f "ninja-gates\..*/gatefake" >/dev/null 2>&1; then
    bad "空载有插件进程"
  else
    ok "零插件进程"
  fi
  kill -0 "$APP_PID" 2>/dev/null && ok "宿主存活（空载是完整终端）" || bad "宿主异常退出"
  stop_host
else
  bad "宿主未就绪"
  dump_tail g3
fi

# ---------------------------------------------------------------------------
# G4 启用即拉起 / 关掉即轻（面板钩子全生命周期）+ G5 崩溃隔离
# ---------------------------------------------------------------------------
say "G4 启用即拉起 / 关掉即轻（gatefake 全生命周期）"
PDIR=$WORK/g4-plugins
mkdir -p "$PDIR"
cat > "$PDIR/gatefake" <<'PY'
#!/usr/bin/env python3
import os, socket, sys, time
sock = os.environ.get("NINJA_ADE_SOCK")
if not sock:
    sys.exit(2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
for _ in range(100):
    try:
        s.connect(sock)
        break
    except OSError:
        time.sleep(0.05)
else:
    sys.exit(2)
while True:
    if not s.recv(4096):
        sys.exit(0)  # 宿主关socket（EOF）= 正常退出
PY
chmod +x "$PDIR/gatefake"
printf '[plugins]\nenabled = ["gatefake"]\n' > "$WORK/g4-ninja.toml"
SOCK="$WORK/g4-ade.sock"
PANEL="$WORK/g4-panel"

# 宿主从 NINJA_PLUGIN_DIR 解析 gatefake（不碰真实 ~/.config）。
if NINJA_PLUGIN_DIR="$PDIR" start_host g4; then
  wait_for "socket 出现" 8 [ -e "$SOCK" ] && ok "启用即绑定 socket" || bad "socket 未出现"
  wait_for "插件进程拉起" 8 pgrep -f "$PDIR/gatefake" && ok "启用即拉起（gatefake 在跑）" || bad "插件未拉起"

  echo "gatefake off" > "$PANEL"
  wait_for "off 后进程回收" 8 sh -c "! pgrep -f '$PDIR/gatefake'" &&
    ok "关掉即轻：进程回收" || bad "off 后进程仍在"
  wait_for "off 后 socket 删除" 5 sh -c "[ ! -e '$SOCK' ]" &&
    ok "关掉即轻：名单空 → socket 删除（回空载形态）" || bad "off 后 socket 仍在"
  grep -q "插件已禁用" "$LOGS/g4.log" && ok "宿主记录禁用收口（层/连接/子进程/socket）" ||
    note "日志未见「插件已禁用」行"

  echo "gatefake on" > "$PANEL"
  wait_for "重开后 socket 重绑" 8 [ -e "$SOCK" ] && ok "再启用：重绑 socket" || bad "on 后 socket 未回"
  wait_for "重开后插件重拉" 8 pgrep -f "$PDIR/gatefake" && ok "再启用：重新拉起" || bad "on 后未重拉"

  say "G5 崩溃隔离（SIGKILL 插件）"
  FPID=$(pgrep -f "$PDIR/gatefake" | head -1)
  if [ -n "$FPID" ]; then
    kill -9 "$FPID"
    sleep 1.5
    kill -0 "$APP_PID" 2>/dev/null && ok "SIGKILL 插件后宿主存活" || bad "宿主被插件带倒"
    [ -e "$SOCK" ] && ok "崩溃后 socket 仍在（名单未空，插件面还在）" || bad "socket 意外消失"
    if pgrep -f "$PDIR/gatefake" >/dev/null 2>&1; then
      bad "插件被自动重拉（违反「别再试」语义）"
    else
      ok "不自动重拉（面板显式操作才重试）"
    fi
  else
    bad "拿不到插件 pid，G5 未执行"
  fi
  stop_host
else
  bad "宿主未就绪"
  dump_tail g4
fi

[ -n "${NINJA_E2E_SCREEN:-}" ] && unset NINJA_E2E_SCREEN
printf '\n== 汇总：PASS %d / FAIL %d\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
