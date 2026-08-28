# ninja p1 取证笔记

单终端面（AppKit + Metal + PTY + libghostty-vt）的验证记录与内存基线。

## 内存基线（p2 门禁对照）

`footprint`（Apple Silicon，M4，retina 缩放 2.0，Menlo 13pt，80x24）：

| 状态 | footprint |
| --- | --- |
| 启动 + shell 提示符空闲 | **36 MB** |
| 交互若干轮后（echo/中文/全选/粘贴/滚动） | 39-44 MB |

大头是 Metal/CoreText/AppKit 框架自身；vt 核与 atlas 在其中的占比极小。
p2 加标签/分屏时，单窗空载基线不应显著高于此数字。

## 修复记录（第三轮验证反馈 D1：启动期唤醒注册竞态）

第二轮「同帧上传」修复后，验证员用同款探针（SHELL=立即输出的脚本 +
NINJA_DUMP_ATLAS）复测 6/6 仍黑：atlas 恒 1px（仅白块），上一轮声称的
5125px 不可复现。本轮复现 + 插桩实证：

- 本机 HEAD c3a5c20 探针 6 次：**5/6 BLACK**（4066px vs 1px）——上一轮
  的「过」是运气（快 shell 首批字节碰巧晚于 hook 注册到达）。
- 临时插桩（跑完即还原）：3/3 运行中「first rx enqueue」比
  「wake hook installed」早 6-9ms（Renderer::new 运行时着色器编译的
  时长），wake_hook / source_perform / on_pty_data 全程零调用——
  字节滞留 rx 队列，vt 从未收到，空闲 shell 无后续字节永不自愈。

根因：`TerminalView::new` 里 `Pty::spawn` 在 `set_wake_hook` 之前执行，
中间隔 Renderer::new；快 shell 首批 PTY 字节在 `WAKE_HOOK==0` 时入队，
`wake_main()` 空转丢信号。真实交互 shell（bash/zsh）启动慢于注册，
碰巧赢过竞态，掩盖缺陷。

修复（view.rs，1 行 + 注释）：`set_wake_hook` 注册完后立即补发一次
`wake_hook()`——信号 source + 唤醒主 runloop，起转后 source_perform →
`on_pty_data` 把 [spawn, 注册] 窗口期内到达的字节全部 drain 进 vt；
rx 为空时只多一帧空画，无自旋；之后到达的字节走正常路径，无丢失窗口。

修复后取证：同探针 **6/6 TEXT**（每次 4066px）；删除该行重测 **3/3 FAILED
（ink=1）**→ 判别力实证。新增门控回归测试
`tests/fast_shell_first_frame.rs`（拉真实二进制 + 同款 atlas 探针，
display 无关，锁屏下也有效；默认 skip，`NINJA_E2E=1` 启用；fakesh 用
`read` 阻塞到 master 关闭，不留孤儿进程）。

注：本轮验证会话屏幕同样锁定（CGSSessionScreenIsLocked=1，全屏截图全黑），
屏幕级取证不可用；atlas 探针不受影响（离屏 drawable 照常渲染）。

## 渲染修复记录（第二轮验证反馈：空闲首开全黑）

**事后勘误（第三轮）**：本节 fakesh 5125px 屏照证据是真实数据但属运气，
快 shell 首批字节碰巧晚于 hook 注册到达（见上节：启动期唤醒注册竞态）；
同款探针在第三轮 6/6 黑。同帧上传修复本身正确（缺陷独立存在，判别
单测仍红），但首开黑屏的另一半根因是竞态，第三轮才修净。

第二轮验证发现 fresh launch 空闲 3-4s 后屏幕只有光标块、零文本（shell
启动文本已在 vt 网格，但不上屏）；强制改窗口尺寸（无新 PTY 字节）后文本
立即出现。根因：`renderer.rs` 的 `draw()` 在帧首 drain
`atlas.take_pending()`，而本帧 cell 循环里 `get_or_rasterize` 光栅化的新字形
只画 quad 不上传（等下一帧才进纹理）→ 该帧采样到全零槽位、coverage=0、
alpha=0。且空闲终端没有下一帧：无 PTY 字节，libghostty-vt 默认
`cursor_blinking=false`（view 的 blink_tick 直接 return 不重画）。

修复：重排 `draw()` 为「组顶点（含光栅化）→ 上传 pending → 编码」。
`replaceRegion` 是 CPU 侧立即写入（Shared 存储），同一命令缓冲 commit 后
GPU 才采样 → 本帧新字形当帧可见，不依赖任何后续帧。另：atlas 满版
reset 会作废本帧已建 quad 的槽位，新增 `atlas.resets()` 计数检测、有
reset 就重建顶点（≤3 pass，reset 只由新字形触发，空闲不发生）。不调度
额外重画：无新增槽位时上传 no-op，空闲帧数为 0，无自旋。新增防回归单测
`renderer::tests::first_frame_glyphs_uploaded_same_frame`（离屏 draw 一帧，
读回纹理断言 ink>50 且 pending 清空；把上传挪回帧首则测试必红，对缺陷有
判别力；拿不到 drawable 的纯 headless 环境自动跳过）。

修复后实测（本轮，2026-08-28）：

- 单测（离屏 Metal）：首帧后纹理非零 5123px；临时恢复老顺序重跑同一
  测试 = panicked（ink 0px）→ 判别力实证。
- `SHELL=/tmp/fakesh`（固定输出三行）fresh launch 空闲 4s 截图：
  ink rows 61-80（行1 3498px）、91-115（行2 含中文 1909px）、118-148
  （提示符+块光标 1028px）→ 启动文本全部可见；atlas 读回 5125px 非零
  （修复前恰 1px）；idle 4s→6s dump mtime 不变 → 0 帧空转。
- 真实 `$SHELL`（zsh）同样取证：空闲首开首行 ~30 cell 提示符字形 +
  块光标可见（截图细粒度像素图可读）。

## 渲染修复记录（第一轮验证反馈）

第一轮验证发现字形只占 cell 约 40% 高、贴行顶。根因是
`font.rs` 光栅化把基线放在距位图底 `ascent+1`（26pt 字形的基线上方只剩
约 8px，上半全部裁掉），修复为距底 `descent+1`（基线上方正好
`ascent+1`）。新增防回归单测 `rasterize_baseline_not_clipped`
（M ink ≥ 半高、M 底行贴基线、g 跨基线）。

修复后实测（本轮，2026-08-27 深夜）：

- `cargo run -p ninja --example raster_check`：'M' 位图 16x33、ink 19 行
  （顶距位图顶 6px、底行距底 ≈ descent+1）；'g' ink 20 行含完整 descender。
- 屏幕实测（截图 1px 采样）：`printf 'WWWW mmmm gggg 你好'` 输出行——
  W cap **20px**（26pt 满高）、m x-height 15px、g 含 descender、
  中文宽字 21px 高。行距 33.8px（=cell_h×2）正常。
- NDC 映射标定（临时取证 quad，已移除）：100x100 设备像素 quad 屏幕
  实测 100x100px、位置精确 1:1。
- atlas 纹理读回（`NINJA_DUMP_ATLAS=/tmp/atlas.pgm` 后 `--noswap` 任意
  PGM 查看器）：槽位内容与 raster_check 一致。

**验证注意**：Orca `type-text` 合成输入会丢 Shift（"MM" 实际落屏 "mm"），
测字形高度必须用 `printf` 输出大写字母，不要用 type-text 打大写。
另外 Orca `drag` 不产生 AppKit 鼠标事件（0 事件送达），选区取证必须用
`tools/verify/synth_input.swift`（真实 CGEvent）。

## p1 验收取证（复跑命令）

```bash
cargo test -p ninja            # 25 全绿（20 lib + 1 E2E 门控默认 skip + 4 FFI），0 warning
NINJA_E2E=1 cargo test -p ninja --test fast_shell_first_frame   # 快 shell 首帧回归（拉真窗口）
./target/debug/ninja           # 拉起 $SHELL（-zsh 登录 shell）
# 空闲首开取证（p1 门禁「打开即 bash」，本轮修复场景）：
#   printf '#!/bin/bash\nprintf "line1 abc XYZ\\nline2 你好\\nidle%%%% "\nexec sleep 10000\n' >/tmp/fakesh && chmod +x /tmp/fakesh
#   SHELL=/tmp/fakesh NINJA_DUMP_ATLAS=/tmp/a.pgm ./target/debug/ninja &
#   sleep 4; screencapture -x -R<x,y,w,h> /tmp/s.png  # 窗口区域（System Events 取 bounds）
#   swift tools/verify/shot_text.swift /tmp/s.png rows   # 应见行1/行2/提示符+块光标 ink
#   stat -f %m /tmp/a.pgm（间隔 2s 两次）→ mtime 不变 = 空闲无空转帧
```

| 验收项 | 取证方式 | 结果 |
| --- | --- | --- |
| **空闲首开即出字**（本轮修复） | `SHELL=/tmp/fakesh` fresh launch 空闲 4s 截图 `shot_text rows` | 行1 3498px、行2 含中文 1909px、提示符+块光标 1028px ✔ |
| 空闲首开 atlas 当帧上传 | `NINJA_DUMP_ATLAS` 首帧读回非零像素 | 5125px（修复前恰 1px）✔ |
| 空闲无空转帧 | dump mtime 间隔 2s 两次不变 | 不变 ✔ |
| 打开即 shell | `pgrep -P <pid>` 唯一子进程 `-zsh` | ✔ |
| PTY 输入链路 | 终端内 `touch /tmp/x` | 文件出现 ✔ |
| vt 输出/标题 | 窗口标题 `jal@192: ~/my_repos/ninja`（OSC 0/2） | ✔ |
| 字形满高 | `printf 'WWWW mmmm gggg'` + 截图 1px 采样 | W cap 20px ✔ |
| 中文宽字 | 同上含 `你好` | 21px 双宽 ✔ |
| resize reflow | 窗口 627→1000→500pt + `echo $COLUMNS` | 80→127→63 ✔ |
| 拖选 | `swift tools/verify/synth_input.swift drag X0 Y X1 Y` | 8080 蓝像素、col0-22 ✔ |
| Cmd+C | `synth_input.swift key 8 1` + `pbpaste` | `WWWW mmmm gggg 你好%` ✔ |
| Cmd+A 全选 | `synth_input.swift key 0 1` | 75168 蓝像素全宽 → 196 字节 ✔ |
| Cmd+V | `pbcopy 'echo OK > /tmp/x'` + `key 9 1` + `key 36 0` | 落盘 ✔（bracketed paste） |
| 滚轮 | `synth_input.swift scroll 3 5` / `-3 5` 往复 | 视口滚动、进程稳定 ✔ |
| footprint | `footprint <pid>` | 空闲 36MB ✔ |
| 空载红线 | `lsof -p <pid>` 0 监听 socket；子进程仅 shell | ✔ |

拖选屏幕坐标换算：屏幕 = 窗口 origin（System Events `{position,size}`）
+ 视图点坐标 + 标题栏 28pt；截图（Orca get-app-state）为窗口局部、
scale=2、含 56px 标题栏。

手动项（无法全自动，需人工过一遍）：

- IME：拼音输入法敲中文 → 候选窗定位（firstRectForCharacterRange 接光标
  cell 屏幕矩形）→ 回车上屏（insertText → PTY）→ 预编辑下划线落格渲染。
- 光标闪烁节奏（0.53s 相位）。
- shift+点击扩选、option 拖矩形选区手感。

## 已知残留

- **待补测（第三轮 D2，环境受限）**：验证会话屏幕锁定
  （CGSSessionScreenIsLocked=1，Window Server Display Shield 遮屏，
  CGEvent 无法送达应用），IME、拖选/Cmd+C/Cmd+V（真实 CGEvent）、
  resize reflow、滚轮、空闲截图像素取证近两轮未能复测；需解锁会话后
  按上表复跑（上表历史 ✔ 均为解锁会话下的取证）。
- `OscParser CHANGE_WINDOW_TITLE_STR` 上游恒空（p0 记录）：不影响
  `Terminal::title()`，窗口标题正常。
- 拖选自动滚动（autoscroll tick）未接：拖到窗口外不滚屏（vt gesture 提供
  Autoscroll API，p2 接）。
- marked text 超行尾截断（预编辑串长于剩余列时裁剪，不折行）。
- 窗口跨屏移动后字体 scale 不跟随（固定主屏 scale）。
- Cmd+key 无菜单绑定时按 SUPER 编码直发 PTY（kitty 键盘协议）。

# p2：标签 / 分屏 / 多窗口 + TOML 配置（2026-08-28）

## 架构（改动落点）

- **唤醒链路去单例**：p1 的 `MAIN_VIEW/WAKE_SOURCE/MAIN_RUNLOOP` 三个全局
  与 pty.rs 全局 `WAKE_HOOK` 全部移除，改 per-pane：每个 `TerminalView`
  建自己的 `CFRunLoopSource`（info 指向堆上 `WakeInfo`），唤醒闭包
  （`Arc<dyn Fn + Send + Sync>`）注册进该 pane 的 `PtyInner`。D1 修复
  保留：`install_wake` 后立即补发一次 signal（`signal_wake`）。
- **拆除顺序红线**（本阶段两个 SIGSEGV 的教训，见下）：
  1. `view::shutdown`：标 `WakeInfo.dead=true` → 断 `set_wake(None)` →
     drop PTY（join 读写线程）→ 摘 runloop source。**info 有意泄漏**
     （每 pane ~几十字节）：runloop 对已 signal 未 fire 的 source 可能持
     快照引用，摘除后仍可能回调 perform——free info 即 UAF。
  2. **窗口所有权**：`NSWindow` 默认 `releasedWhenClosed=YES`（close 时
     自释放）；壳的 registry 再持一份强引用 = 过释放 → 关窗必 SIGSEGV
     （pc=0xc8，跳已释放对象）。现 `setReleasedWhenClosed(false)` +
     delegate 持 registry（`Vec<Retained<NSWindow>>`），close 完成后由
     0.05s 一次性 `NSTimer`（`ninjaPruneClosedWindows:`）释放——不能在
     `windowWillClose` 里当场 drop（窗口会拆在自己的 close 调用栈里）。
  3. **不做全局窗口遍历**：关窗瞬间其它窗口可能在拆，焦点环同步/pane
     EOF 只碰 `view.window()` 自己的窗口（`sync_focus_ring_for`）。
  4. `shutdown_all` 只拆资源不摘视图：AppKit close 收尾还会碰子视图。
- **pane 树**（pane.rs）：`PaneContainer`（NSView，窗口 contentView）持
  二叉 split 树（叶子=TerminalView，ratio 可拖）；分隔条子视图可拖调
  比例；焦点环 = 最上层 CALayer 边框视图（hitTest 返回 nil 不挡鼠标）。
  ⌘D/⌘⇧D 插在焦点叶子旁；焦点导航按叶子 frame 几何找相邻重叠面；
  布局尾部统一 `setNeedsDisplay`（分屏时同步 Metal present 在我们自己的
  布局栈里不上屏——曾出现分屏后旧 pane 全黑直到点击，见残留）。
- **壳**（shell.rs + app.rs）：⌘N 新窗口（`ninjaNewWindow:` 落 app
  delegate）；⌘T 走 NSResponder 内建 `newWindowForTab:` + 系统 tab bar；
  `addTabbedWindow:ordered:` 挂 tab 组（统一 tabbingIdentifier）；
  ⌘W `performClose:`；pane shell 退出（EOF）→ `handle_pane_eof` →
  多 pane 拆 pane / 单 pane 关窗；最后窗关闭才退出
  （`applicationShouldTerminateAfterLastWindowClosed`）。
- **配置**（config.rs）：`~/.config/ninja/ninja.toml`（`NINJA_CONFIG`
  可覆盖）。缺文件 = 内置默认照常启动；坏字段 stderr 警告 + 降级默认。
  schema：`shell` / `font-family`（不可用回退 Menlo）/ `font-size`
  （4–200pt）/ `[theme] selection-bg`、`cursor`（#RRGGBB/#RGB/0x…）/
  `[keys]`（动作名 → "cmd+shift+d" 风格，16 个动作可重绑，箭头 =
  left/right/up/down）。菜单栏在启动时按配置键位生成。
- **门禁取证钩子**：`NINJA_P2_SELFTEST=tab,split,win,close,closepane`
  （逗号序列）在启动 0.8s 后按序触发对应动作——免 CGEvent 抖动，多
  pane 内存/稳定性取证可复现（非产品功能，未知步骤忽略并警告）。

## 验收取证（复跑命令）

```bash
cargo test --workspace          # 35 全过（28 lib + e2e 门控 skip + FFI…）
NINJA_E2E=1 cargo test -p ninja --test fast_shell_first_frame   # D1 回归 ✓
# 多 pane 取证（免 CGEvent）：
NINJA_P2_SELFTEST=tab,split,win SHELL=/tmp/fakesh_p2.sh ./target/debug/ninja &
sleep 6; footprint $(pgrep -x ninja)   # 4 pane（2窗+2标签+1分屏）
# 配置：NINJA_CONFIG=/path/to/custom.toml（font-size 16 → 窗宽 769pt 实证生效）
```

| 验收项 | 取证方式 | 结果 |
| --- | --- | --- |
| cargo test --workspace | 本轮 | 35 全过，0 warning ✔ |
| D1 快 shell 首帧（per-pane 唤醒重构后） | NINJA_E2E e2e | ink>1000 ✔ |
| ⌘T 新标签 | CGEvent + selftest | pane 1→2，AX windows=1（成 tab）✔ |
| ⌘D 右分屏 | CGEvent + 截图 | pane 2→3；左右两半各 1448/2143 ink，中缝分隔条在 x≈w/2 ✔ |
| ⌘N 新窗口 | CGEvent + selftest | windows 1→2→3 ✔ |
| ⌘W 关单窗（多窗存在） | CGEvent + selftest ×7 场景 | 存活、余窗正常、pane SIGHUP ✔ |
| ⌘W 关最后窗 / ⌘Q | CGEvent | 进程退出、fakesh 残留 0 ✔ |
| EOF 关 pane/关窗 | SHELL=即退脚本 | exit=0，无残留 ✔ |
| 关 pane（⌘⇧W 路径） | selftest closepane ×3 | 树塌缩、焦点转移、无崩溃 ✔ |
| 压力（tab,split,tab,split,closepane,close,win,close） | selftest ×3 | 3/3 稳定终态一致 ✔ |
| 缺省配置文件启动 | 本机无 ~/.config/ninja/ninja.toml | 全部上述取证都在此状态跑 ✔ |
| 自定义配置生效 | NINJA_CONFIG（16pt Courier） | 启动 ✓ shell ✓ 窗 769×463（默认 627×392）✔ |
| 坏配置不拒启 | font-size=9999 | 警告 + 默认启动 ✔ |
| **空载内存（门禁）** | footprint，对照 Ghostty 112MB（同机 store 基线） | 单窗 36MB（=p1，零回退）；2标签+1分屏（3 pane）65MB；2窗+2标签+1分屏（4 pane）91MB —— 同量级且低于 Ghostty ✔ |
| 空闲无空转 | NINJA_DUMP_ATLAS mtime 间隔 3s | 不变 ✔ |
| 空载红线 | cargo tree -p ninja \| grep wasmtime/ninja-protocol/preview | 0 命中；子进程仅 PTY shell ✔ |

## p2 实测缺陷修复记录（验证轮次内）

1. **分屏后旧 pane 全黑**：旧 pane 的 resize 重画发生在我们自己的布局
   调用栈里（图层几何未提交），Metal present 不上屏；点击后才出现。
   修复：`relayout` 尾部对叶子统一 `setNeedsDisplay`，重画推迟到
   AppKit 显示周期（drawRect 路径）。
2. **关窗 SIGSEGV（三种独立根因，全部 CGEvent/自测钩子复现）**：
   - runloop 快照对已摘除 source 的迟到 perform → info 泄漏 + dead 标记；
   - 关窗期间全局窗口遍历触半拆窗口 → 只碰自己窗口；
   - `releasedWhenClosed` 默认 YES + 壳持强引用 = 过释放 → NO + 延迟
     一拍释放。修复后 7 个关闭场景 + 3 轮压力全绿。

## 已知残留（p2 新增）

- 每个 pane 一套 Metal 管线/命令队列（~15-20MB/pane）。4 pane 91MB 在
  门禁内；如需再压，p5+ 可共享 device/queue、按需休眠非焦点 pane。
- 关闭的 pane 泄漏 `WakeInfo` + runloop source 本体（有意，见拆除红线1），
  每 pane 常量级字节。
- pane 内 OSC 标题多 pane 互抢窗口标题（最后写者赢）；tab 标题同此。
- 焦点环用主题 cursor 色；分隔条不可键盘操作。
- CGEvent 取证受真实会话抢焦点干扰（首键常丢）；关键路径有
  NINJA_P2_SELFTEST 钩子兜底。IME/拖选等 p1 手动项沿用 D2 待补测。
