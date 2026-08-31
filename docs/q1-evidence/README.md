# q1 壳取证（crates/ninja）

- 二进制：`cargo build --release -p ninja && ./target/release/ninja`（默认 = q1 交互壳；
  `--evidence-dir DIR` = q0 取证模式回归，见 docs/Q0-CAPABILITY-AUDIT.md）。
- 引擎：libghostty 钉点 a887df42（静态链入）。
- 实现结构：`crates/ninja/src/`——`surface.rs`（SurfaceHostView：键盘/IME/鼠标
  翻译 + resize 叶子端）、`keymap.rs`（NSEvent→ghostty 键事件桥）、`pane.rs`
  （二叉 split 布局树 + 分隔条 + 焦点环 + zoom 状态机）、`shell.rs`（多窗/
  原生 tabbing/裸⌘W 决策/GOTO·MOVE·CLOSE_TAB）、`host.rs`（宿主单例 + action
  全分发 + inherited_config 建面 + surface 延迟 free）、`app.rs`（AppDelegate +
  菜单 + 取证钩子）、`q0_demo.rs`（q0 审计取证机收编）。
- 取证方法（全部虚拟屏）：`run-e2e.sh` 起 `scripts/e2e/virtual-display hold` →
  `NINJA_E2E_SCREEN=<displayID>` 跑宿主 → 真键/鼠标经 `synth_input.swift`
  （键盘 CGEventPostToPid、鼠标全局 HID tap——PostToPid 的鼠标事件不带窗口
  上下文，目标 app 不会命中）→ 布局 dump 走 `NINJA_ZOOM_FILE`/`NINJA_ZOOM_DUMP`
  钩子 → 决策日志 `NINJA_Q1_DEBUG=1`。断言全部跑命令/读 dump JSON。
- 复跑：`bash docs/q1-evidence/run-e2e.sh`（连跑两轮 45/45 断言全绿，见
  e2e-summary.txt；结束时自动 kill 自己的 hold）。

## 结论清单（对应 q1 验收「过」）

| 验收项 | 证据 |
| --- | --- |
| 布局树接 surface（tab+split） | a-step1.json：selftest tab,split → 2 叶各自非零（39x23 网格、对半几何），tab 并入同一 CG 窗；shot-split-tab.png |
| ⌘⇧Enter 多 pane 放大/还原 | a-step2/3：真键（菜单路径）放大焦点面、隐藏面网格冻结（39x23）、再按还原对半；a-step4/5：`bindact:toggle_split_zoom`（GHOSTTY_ACTION_TOGGLE_SPLIT_ZOOM，action_cb 路径）同语义；shot-zoomed.png |
| ⌘⇧Enter 单 pane | b-step1/2：window.zoomed=true/false（窗口 zoom 非全屏） |
| ⌘W 多 pane 只关一面（菜单路径） | b-step4：真键 ⌘W（performClose→windowShouldClose 裸⌘W 决策）→ leaves 2→1、窗口数不变；log（e2e-logs/b.log、eof-cmdw.log）：`close-request keyDown bare_cmd=true` → `windowShouldClose -> false` |
| ⌘W ghostty 绑定路径（close_surface） | c-step1/2：`ghostty_surface_request_close` 与 `bindact:close_surface` → `close_surface_cb alive=true process_alive=true` → leaves-1、窗口存活（closebinding.log） |
| ⌘W 单 pane 关 tab/窗 | d-step：⌘T 真键 → 同窗两 tab；⌘W 关当前 tab（窗口存活）；再 ⌘W 关最后 tab → 关窗退出（tab-close.log） |
| EOF 关面 | b-step5：焦点面键 `exit⏎` → `close_surface_cb process_alive=false` → 单 pane performClose → 进程退出 0 |
| 焦点链 | a-step6：真鼠标点击左面 → `echoL` 回显落左面；a-step7：⌘]（goto_split next，真键）→ `echoR` 落右面 |
| resize 全链 | a-step8 窗口角拖小（列数 39→26）；a-step9 分隔条拖拽（左叶 217.5→127.5）；a-step10 ⌘⌃←（RESIZE_SPLIT 真键）→127.5→117.5；a-step11 ⌃⌘=（EQUALIZE_SPLITS 真键）→回对半 |
| 多窗 | c-step3：⌘N 真键（NEW_WINDOW，inherited_config WINDOW）→ 窗口数 1→2 |
| 空载纪律 | empty-children.txt：子进程仅 PTY shell（login×3）；宿主 lsof unix socket=0（零插件 socket） |
| q0 回归 | q0-regression-report.txt：`--evidence-dir` 模式 exit 0、overall: PASS（5 检查项） |
| 纯逻辑单测 | `cargo test -p ninja`：12/12（zoom 决策状态机、裸⌘W 决策、ratio 夹取、mods/IME 文本过滤、scroll 打包） |

## 关键实测语义（宿主钉死）

- **⌘W 双路径同语义**：菜单路径（performClose → `windowShouldClose` 的裸⌘W
  决策：多 pane 只关焦点面并拦整窗 close，单 pane 放行原生关 tab/窗）与 ghostty
  绑定路径（close_surface → `close_surface_cb` → 同一个 `close_leaf`）都只关
  「当前面」；红绿灯/⇧⌘W/⌥⌘W/EOF 等非裸⌘W 路径一律整窗/整 tab 关（单测覆盖）。
- **⌘⇧Enter 三态**：单 pane=窗口 zoom；有分屏未放大=放大焦点叶（其余隐藏
  **不销毁**：surface 数据照喂、`set_occlusion(false)` 停画、网格冻结分屏尺寸）；
  已放大=还原。`zoom_decision` 纯函数 + 单测。
- **EOF**：exit → `close_surface_cb(process_alive=false)` ≡ ⌘W 同一决策。

## 合成输入的坑（synth_input.swift 注释同）

- 键盘事件 flags 不能清成 0x0（真实事件恒带 0x100 NonCoalesced；0x0 退化事件
  keyDown 能到视图但 IME insertText 路径不产生，Enter 不执行）。
- 鼠标事件 CGEventPostToPid 不带窗口上下文、目标 app 不命中 → 全局 HID tap
  注入（坐标在虚拟屏上，只影响那边的窗口）。
- `'='` 的 kVK 是 24（0x27 是引号键）——⌃⌘=（EQUALIZE_SPLITS）取证踩过。
