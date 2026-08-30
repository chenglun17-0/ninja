# q1 壳重接取证（ninja-embed）

- 二进制：`cargo build --release -p ninja-embed && ./target/release/ninja-embed`（默认 = q1 交互壳；
  `--evidence-dir DIR` = q0 取证模式，回归见下）
- 引擎：libghostty 钉点 a887df42（静态链入，见 docs/Q0-CAPABILITY-AUDIT.md）
- 取证方法：NINJA_P2_SELFTEST / NINJA_ZOOM_FILE+NINJA_ZOOM_DUMP 钩子（宿主内直驱，
  免 CGEvent 抖动）+ 真实 CGEvent 键/鼠（tools/verify/synth_input.swift，
  ⌘W/⌘T/⌘N/⌘⇧Enter/点击/拖分隔条）；断言全部跑命令/读 dump JSON，不信 summary。
  NINJA_Q1_DEBUG=1 打开壳的决策日志（windowShouldClose / close_surface_cb / action tag）。

## 结论清单（对应 q1 验收）

| 验收项 | 证据 |
| --- | --- |
| 多窗/标签/分屏布局树接 surface | runA-step0（selftest tab,split → 2 panes 各 37x22）+ ⌘T/⌘N 真键（closebinding-tab-window.log：wincount 1→2） |
| resize 全链 | 分屏 relayout、拖分隔条（divider-drag-after.json：438/438 → 572/304）、ghostty 默认键位 ⌘⌃← resize_split + ⌃⌘= equalize（真键实测，几何变化取证于 /tmp 脚本运行） |
| 焦点全链 | 点击右 pane 夺焦 → 键入回显落该面（eof-panel.log 时段的 dump 断言）；become/resign → surface_set_focus |
| ⌘W 双路径 | 菜单路径：真键 ⌘W 多 pane → log `close-request keyDown mods=0x100000 bare_cmd=true → close_leaf → windowShouldClose -> false`（runA.log，只关一面）；ghostty 绑定路径：`ghostty_surface_request_close`（= close_surface 键位绑定的正常触发流，embedded.zig L1922）→ `close_surface_cb alive=true process_alive=true` → 多 pane 只关一面（closebinding-tab-window.log + closebinding-after.json） |
| ⌘W 单 pane 关 tab | 真键 ⌘W 单 pane → windowShouldClose → true → tab 关（wincount 2→1） |
| EOF 关面 | 在焦点 pane 键 `exit⏎` → `close_surface_cb alive=true process_alive=false` → 只拆该面（eof-after.json leaves=1） |
| ⌘⇧Enter 三态 | runA-step1/2：多 pane 放大（隐藏面网格冻结 37x22、放大面占满 880pt）/还原（几何回分屏态）；runA-step4：单 pane = 窗口 zoom（window.zoomed=true）；真键 ⌘⇧Enter 放大/再按还原（runA-step5/6） |
| 键盘端到端 | keypid 序列 `h`,`i` → 网格回显（runA-step7 last 行） |
| 面板入口 | 真键 ⌘, → 空面板窗打开（shot-panel.png；wincount +1）；**空载零插件进程/零 socket**（lsof -U unix=0；子进程只有 PTY shell/login） |
| 截图 | shot-split-state.png / shot-split-tab.png / shot-panel.png |
| q0 取证模式回归 | `./target/release/ninja-embed --evidence-dir /tmp/q0ev` → report.txt `OVERALL: PASS`（exit 0，审计文档复现入口未破坏） |

## 运行方式（可复现）

```sh
cargo build --release -p ninja-embed
# 交互壳 + 取证钩子
NINJA_P2_SELFTEST=tab,split NINJA_ZOOM_FILE=/tmp/z NINJA_ZOOM_DUMP=/tmp/d.json \
  NINJA_Q1_DEBUG=1 ./target/release/ninja-embed
#   echo split|toggle|zoom|unzoom|dump1 > /tmp/z   # dump → NINJA_ZOOM_DUMP JSON
# 真键取证（⌘W=13 ⌘⇧Enter=36 ⌘T=17 ⌘N=45）
swift tools/verify/synth_input.swift keypidcmd 13 <pid>
```

- runA.log / closebinding-tab-window.log / eof-panel.log：`NINJA_Q1_DEBUG=1` 的完整决策日志。
- runA-stepN.json：zoom dump 序列（布局/隐藏/网格尺寸/最下非空行——对齐 v1 X3 dump 字段）。
