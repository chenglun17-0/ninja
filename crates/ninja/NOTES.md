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

## p5：层 + 文本预览（第一个插件门禁）取证

「只通过公开协议完成点击路径 → 终端内看文本 → Esc 关层」。全部走
真实二进制 + 真实 Unix socket + 真实 IOSurface 共享内存。

### 协议修订（v0 内，显式钉）

`hit` 增补 `cwd`（string，`#[serde(default)]`）——进程外插件无法访问
宿主的 OSC-7 pwd 状态，golden 样例 `src/main.rs:42:13` 正是相对路径；
不加则插件永远认领不了相对路径。规则记录在 ninja-protocol crate 文档
「版本与演化规则」第 5 条（v0 未发布，字段集只增不删；公开后再改必须
升 v）。golden `hit.json` 已再生成，second_language 测试无需改动
（解码器只看 v/type）。

### 端到端链路（E2E：`NINJA_E2E=1 cargo test -p ninja --test layer_preview`）

| 环节 | 证据 |
| --- | --- |
| 启用≠常驻 | idle_no_plugins 对照组：启用后 3s 无子进程；`已拉起插件` 日志只出现在首次分发 |
| 首击冷启动 | host_err：`冷启动等待 ~12ms，连接数 1`（spawn→connect）；预算 `COLD_CONNECT_TIMEOUT=2s`（debug 构建/系统忙时可达数百 ms，350ms 实测会随机降级） |
| claim 抑制系统默认 | open_probe 文件为空（认领即止） |
| 层内容 = 文本 | `NINJA_LAYER_PROBE`：渲染器把层纹理读回落 PPM；绝对路径场景 4344+ 亮像素、8 条 31px 等距文本条带（Menlo 13pt 行距）+ 头带 |
| 相对路径解析 | OSC-7 场景：fakesh 报 `file://host/<pwd>` → 宿主解码为路径进 `hit.cwd` → 插件 `<pwd>/src/rel.txt` 认领 → 层照常出现 |
| Esc 关层 | `tools/verify/synth_input.swift keypid 53 <host-pid>`（CGEventPostToPid 定向投递，免前台焦点抖动）→ keyDown 层前台分支 → 摘层 + 通知插件 + 层探针文件被删 |
| 稳定性 | 修复后连续 5/5 通过；零残留进程（reaper killpg 收割宿主+插件） |

依赖红线：`cargo tree -p ninja-preview` 无宿主 crate（只 ninja-protocol
+ objc2 系统框架族）；`cargo tree -p ninja` 无 wasmtime/tokio 不变；
空载（默认配置）不建 socket、不拉进程不变（idle_no_plugins 全绿）。

### 实现中发现并修掉的缺陷（全部有复现取证）

1. **`layer::present` 注册表锁重入死锁**：present 持 REGISTRY Mutex 调
   `render_now` → `draw_list` 再锁同一把锁 → 宿主主线程冻死（首个
   E2E 轮次无任何报错挂死）。修复：先放锁再重画。
2. **IOSurface 跨进程 lookup 失败**：`kIOSurfaceIsGlobal` 的值误传
   CFNumber（属性接 CFBoolean），surface 实际非 global，插件进程
   `IOSurfaceLookup(id)` 恒 NULL。修复：CFBoolean true。
3. **层握手解码器竞态**：claim 与 layer.open 常在同一读块到达，分发
   阶段只弹到回执就停；握手若先阻塞读新字节，会把已缓冲的
   layer.open 晾满整个 1.5s 预算（E2E 偶发 20s 探针不出现）。修复：
   握手循环先弹尽解码器再阻塞读。
4. **OSC-7 pwd 是完整 URI**：vt 契约 `pwd()` 返回 `file://host/path`，
   p4 的 open.rs 一直把 URI 当路径基（从未被 OSC-7 场景踩中）。新增
   `open::osc7_to_path`（剥 scheme/authority + 百分号解码），hit.cwd
   与系统默认回退共用；带单测。
5. **NINJA_P4_HIT 一次性 3s 定时器竞态**：系统忙时 fakesh 首行晚于
   定时器 → 点击落空 → 测试随机挂。改为内容门控重试（目标行有字节
   才点，最多 15s；点击后停表）。p4 三个 E2E 场景断言不受影响
   （全 contains/空判）。
6. **E2E 点击行超 80 列被网格折行**：宿主认出的是截断路径（插件合理
   ignore）。测试目标改放 `/tmp` 短目录；这也解释了为何 golden 样例
   的相对路径形态（`src/main.rs:42:13`）在真实 80 列终端里必须足够短。

### 运行 E2E 的前提

`cargo test --workspace`（自动构建 ninja-preview）；或先
`cargo build -p ninja-preview` 再 `NINJA_E2E=1 cargo test -p ninja`
（layer_preview 从 `CARGO_BIN_EXE_ninja` 同目录解析插件二进制，缺失
时以明确报错失败而非静默跳过——门禁不放松）。Esc 取证需
`tools/verify/synth_input.swift`（Xcode 工具链；仓内
`.cargo/config.toml` 的 `DEVELOPER_DIR=CommandLineTools` 会被测试显式
剥掉——CLT swift 6.0.2 与自身 SDK 不配套）。

### 已知残留（p5 新增）

- 同 pane 一次只允许一个层（多次命中替换旧层）；多 pane 各一层。
- 层矩形在 view resize 时直接收层（不跟随重排）。
- 插件认领但层握手超预算（挂死插件）时，点击线程最多冻结 1.5s 一次
  （同步策略，与 p4 的 500ms 回执预算同哲学）；release 二进制的
  正常路径 <50ms。
- 泵 timer（150ms）只在有层存在时挂在主 runloop；层全关即摘。
- `input.key` 只映射协议命名集 + 单可见字符；修饰键名完整传递但
  预览插件不消费（无滚动，p5 明确不做）。
- dpi 由窗口 backingScale 推导；离屏/无屏场景为 72（字体小一档），
  功能不受影响。

# p6：关掉即轻（门禁）取证

「禁用预览插件后内存回到 p2 空载、无残留进程、无隐藏窗口」。四条缺口的
修复全部有单测 + E2E 判别（`tests/off_is_light.rs`，`NINJA_E2E=1` 门控）。

## 缺口与修复

1. **插件死亡收层（监督器缺口）**：pump_plugins / layer_handshake / 分发
   读写阶段的对端 EOF / IO 错 / 坏协议断连原来只 `conns.remove(i)`——
   层永久残留（any_layers 恒真 → 泵 timer 永不停）+ 陈旧层纹理一直
   合成在最后渲染的 drawable 上（= 隐藏窗口）。修复：所有连接死亡
   路径统一走 `drop_conn` → `layer::close_by_conn(conn)`（摘层 + 受影响
   pane 主动重画；close() 同样补了重画，close_pane 维持调用方重画——
   shutdown 路径视图可能正在拆）。单测 `conn_death_reclaims_layers_and_
   stops_pump`（对端连上→开层→present→断开→any_layers()==false；删掉
   close_by_conn 调用即红，判别力实证）。
2. **同会话禁用/再启用**：`PluginHost::shutdown()`（幂等；顺序：摘全部层
   + 尽力通知 layer.close → 停泵 → 断连接（插件 EOF 自退）→ kill+wait
   子进程 → 删 socket 文件），`Drop` 复用同一实现。触发钩子
   `NINJA_P6_PLUGIN_FILE`（0.2s 轮询文件内容："off"/"on"/"quit"；产品
   UI 归后续）。再启用 = 新 host 换进分发器同一槽位：spawned 集随新
   对象重置、socket 在原路径重绑。
3. **陈旧 socket 清扫**：宿主 SIGKILL/崩溃 → `$TMPDIR/ninja-ade-<pid>.sock`
   尸体。`PluginHost::start`（仅启用插件时——空载路径零改动）扫约定目录，
   pid 已死（kill(pid,0)=ESRCH）才删，活 pid / 解析不出 pid 一律不动。
   单测 `stale_socket_sweep_dead_pid_only`。
4. **门禁 E2E** `tests/off_is_light.rs` 三场景（骨架复用 layer_preview：
   CARGO_BIN_EXE_ninja + NINJA_P4_HIT + NINJA_LAYER_PROBE + 合成 Esc）。

## E2E 实测（本轮，2026-08-28；连续 4/4 通过）

| 场景 | 断言 | 结果 |
| --- | --- | --- |
| 用一次→Esc→禁用（钩子） | 层探针出现且有墨迹→Esc 删探针→"off" 后 socket 消失、pgrep ninja-preview 空、宿主子进程无 preview、层探针目录空、stderr 有拉起+禁用日志 | ✔ |
| 同上·内存门禁 | footprint 采样最小值回 **37MB**（p2 基线 36MB，+1MB；容差钉 +4MB——足以抓单个 IOSurface 级 ~2MB 泄漏） | ✔ |
| 禁用→再启用→再禁用 | "on" 后 socket 原路径重绑（"已再启用"日志）；再启用无分发不拉进程；"off" 再消失 | ✔ |
| SIGKILL 宿主 | preview 因 socket EOF 自退（无残留）；约定路径留 socket 尸体（SIGKILL 不跑收口） | ✔ |
| 尸体清扫 | 下一个启用插件的宿主启动即扫掉死 pid 尸体（"清扫陈旧"日志） | ✔ |
| 正常退出（钩子 "quit"） | 宿主退出码 0、插件无残留、socket 文件被删、禁用日志在 | ✔ |

复跑：`cargo build -p ninja-preview && NINJA_E2E=1 cargo test -p ninja --test off_is_light`。

## 实现中发现并修掉的缺陷（p6 新增）

1. **`terminate:` 不走 Rust 栈展开（E2E 场景 3 抓到）**：⌘Q / 关最后窗的
   正常退出 = `NSApplication terminate:` 直接 `exit(0)`，`app.run()` 不
   返回、栈上 `PluginHost` 的 **Drop 不会跑**——socket 尸体不只来自
   SIGKILL，每次正常退出都留。p5 的「正常退出路径完整」判断有误。修复：
   `applicationWillTerminate` 显式调 `plugins::host_shutdown()`（与 Drop
   同一幂等实现）。实测 quit 后 socket 消失、退出码 0。
2. **同尺寸 setFrameSize 拆层竞态**：AppKit 窗口装配/居中阶段重复投递
   同尺寸 setFrameSize，无脑收层会把恰落在装配尾音上的首层拆掉（E2E
   偶发探针消失）。修复：记录 last_size，尺寸真变才收层；resize 收层
   语义不变。
3. **CGEventPostToPid 的 ⌘Q 到不了后台应用的菜单系统**（keyDown 直达
   键如 Esc 可以，菜单键等价物不行）：正常退出取证改用钩子 "quit"
   （驱动产品同一条 terminate: 路径）。Esc 合成首键偶发丢失（p2 已录），
   E2E 里带重试（多余的 Esc 落 PTY 无害）。
4. **僵尸 pid 会让清扫误判**：被 SIGKILL 的宿主若没人 waitpid 收尸，
   kill(pid,0) 仍返回"活"→ 保守不删（正确行为）。E2E 场景 2 显式
   waitpid 收尸后再验清扫。

## 已知残留（p6 新增）

- 禁用钩子是取证用的文件触发，不是产品 UI/菜单（归后续；语义已定：
  shutdown 幂等、再启用换新 host）。
- 禁用后宿主 footprint +1MB（Metal/IOSurface 分配器高水位，非泄漏；
  再启用→再用→再禁用不增长）。
- 禁用时对插件的 layer.close 通知是尽力而为（连接已断的层直接回收，
  无通知对象）；与 Esc 兜底同语义。
- 清扫只认约定路径名 `ninja-ade-<pid>.sock`；NINJA_ADE_SOCK 覆盖路径
  的尸体不扫（测试隔离路径，生产不使用）。

# D-A 修复：⌘W 关整窗（2026-08-28）

## 症状与定性

用户日报「多标签时 ⌘W 应只关当前标签，实际关了整个窗口」。排查分两层：

1. **NSWindow 原生 tab 层（2/3 标签、宿主/非宿主标签、全屏、Always 偏好、
   ⌘T→⌘W 20ms 紧衔接、selftest ×12、多窗×tab 组合）**：实测 HEAD 全部
   正确——macOS 会把 File>Close(performClose:) 项 tab 化成 Close Tab(⌘W)/
   Close Window(⇧⌘W)/Close All/Close Other Tabs（AX 菜单 dump 实证），
   `performClose:` 对 tab 组成员只关当前 tab（独立 Swift 探针两种窗口
   配置各验证一次：关 1 留 1）。此层无需修复，但语义在本轮 E2E 钉死。
2. **多 pane（分屏）窗层——可复现缺陷本体**：`⌘W`（performClose:）对
   含 2+ 分屏的窗**直接整窗关闭，所有 pane 的 shell 全部 SIGHUP 陪葬**。
   用户口径里「多标签」= 同窗多终端面；这正是「关整窗 + 杀其它面 shell」，
   也正是任务「p2 记录『⌘W 多窗时关 1 窗其余 pane SIGHUP』语义重审——
   关一个标签绝不能杀其它标签的 shell」要钉死的红线（每 pane 各自
   forkpty+setsid，进程组独立，代码层确认无跨杀；问题只在这条整窗路径）。

## 修复（shell.rs + app.rs）

裸 ⌘W 只关「当前面」（iTerm/Ghostty 同款 surface 语义）：

- `AppDelegate` 实现 `windowShouldClose:`（performClose: 先问 shouldClose
  再 close），委托 `shell::window_should_close`。
- 判别「裸 ⌘W」：currentEvent 是 keyDown，带 Cmd 且**无** Shift/Option/Ctrl。
  ⇧⌘W（系统 Close Window）、⌥⌘W（Close All/Other Tabs）、红绿灯与菜单
  点击（鼠标事件）、EOF 级联与 selftest（定时器/无事件）都不匹配 →
  原整窗/整 tab 语义不变。
- 多 pane 窗：关焦点 pane（`close_leaf`：焦点转移 + 焦点环同步 + 只收
  该 pane 自己的 PTY 进程组），返回 false 拦掉整窗 close；单 pane 放行
  → 关当前 tab，最后一个 tab 才关窗（原生语义）。决策函数
  `should_close_whole_window(surfaces, bare_cmd_key)` 纯逻辑可单测（3 例）。
- 顺带把 shell.rs 三处重复的「contentView→PaneContainer」判定收敛为
  `pane_container_of`（行为不变）。

## 验收取证（真实 GUI 会话，全部可复跑）

| 场景 | 结果 |
| --- | --- |
| 3 分屏 ⌘W×3 | shell 3→2→1、窗在、进程活；最后才关窗退出 ✔（修复前第一下整窗关+进程退，E2E 判别红实证） |
| 3 标签 ⌘W | 只关当前 tab，余 tab shell 存活，窗在 ✔ |
| 红绿灯（分屏窗） | 整窗关（原生）✔ |
| 红绿灯（tab 窗） | 只关当前 tab（原生 tab 行为）✔ |
| ⇧⌘W（Close Pane） | 只关一个 pane，窗在 ✔（语义不变） |
| EOF 杀 1 shell（2 pane 窗） | 只拆该 pane，窗留、余 shell 活 ✔ |
| selftest close（定时器路径） | 整窗关不变（p2 压力序列语义保持）✔ |
| cargo test --workspace | 103 全绿（96 基线 + 3 单测 + 4 E2E）✔ |

复跑：`cargo test --workspace`；E2E（需可交互 GUI 会话 + Xcode 工具链）：
`NINJA_E2E=1 cargo test -p ninja --test cmdw_surface_close`。E2E 四场景
（分屏逐面关 / tab 只关当前 / 红绿灯整窗 / ⇧⌘W 不变）内部 flock 串行
（都需前台激活，⌘W 只有前台应用的菜单系统才会接——p6 实证
CGEventPostToPid 到不了菜单系统），新增 `synth_input.swift activate/wincount`
子命令（NSRunningApplication/CGWindowList，非 AX 权限路径）。

## 已知残留

- tab 态系统注入的 Close Window(⇧⌘W) 与 Panes>Close Pane(⌘⇧W) 键位
  相同：实测菜单匹配按栏序 Panes 项赢（⇧⌘W 关 pane，非整窗）——与
  本轮语义一致，未动。

## T2：主题插件原语 + 官方 ninja-theme（2026-08-29）

用户产品决策（同日确认）：宿主内置 One Dark Pro 为不可卸基线；主题切换
走插件原语；DMG/分发物严格零插件。落地四件：

1. **协议 v0 增补 `theme.set`**（只增不删，第 6 类消息；规则内增补记录
   见 ninja-protocol 文档规则 6）：插件→宿主推完整色板
   （bg/fg/cursor/selection_bg+alpha/divider/ansi×16，颜色一律 `#rrggbb`）。
   golden `theme.set.json` 再生成；Python 参考解码器无需改码（type 无关），
   文档串同步。
2. **宿主运行时覆盖点**：`theme::current()` = 全局唯一「当前生效色板」
   （无覆盖 = ODP 基线）。渲染器选区/光标、pane 容器底色/分隔条/焦点环、
   vt 默认色/调色板全部改为**现读**它（不再读编译期常量）。p2 的
   `[theme]` TOML 字段级覆盖保留且优先于插件（用户显式配置 > 插件）。
   关键点：cell 颜色在解码期经 vt 调色板解析成 RGB，换色板必须强制
   下一帧 Full 重解码（`TermState::apply_effective_palette`），否则缓存
   里的旧 SGR 色不会换；Full 脏同时保证跳帧（D-C）吃不掉全屏换色。
   `[theme]` 配置的 `selection_bg`/`cursor` 因此改为 `Option<Rgb>`
   （None = 跟随生效色板）。
3. **回退语义（与 p6 收层同语义）**：拥有色板覆盖的插件连接死亡
   （EOF/IO 错/坏协议）→ `drop_conn` 里 `revoke_owner` → 回 ODP + 全部
   存活 pane 重钉重画；同会话禁用/退出 → `shutdown` 里 `revoke_all`。
   last-writer-wins：多主题插件时最后一个推色板的拥有覆盖，旧 owner
   死亡不回退（新 owner 在）。泵 timer 维持条件从「有层」扩为
   「有层 ∨ 覆盖生效」——覆盖期间必须盯连接死亡，无层也盯。
4. **官方 ninja-theme**（`crates/ninja-theme`，同 ninja-preview 形态）：
   独立 bin，只依赖 ninja-protocol（无系统框架需求——不画像素），
   连接后即推 theme.set，随后常驻（hit 回 ignore，EOF 退出）。内置
   one-light / solarized-dark / solarized-light 三色板（ODP 之外），
   argv[1] 或 `NINJA_THEME` 选色板。**不进 DMG**（package 脚本注释与
   零插件清点同步加列 ninja-theme；复跑签名 bundle 仍只含 MacOS/ninja，
   像素探针 #282C34 基线在）。

E2E（`NINJA_E2E=1 cargo test -p ninja --test theme_switch`，先
`cargo build -p ninja-theme`）：首击（`NINJA_P4_HIT`）冷启动拉起插件 →
像素探针背景 #282C34→#002B36 + OSC 10/11 应答换新（fakesh 需把 pty
改非规范输入——OSC 应答无换行，canonical 模式永远读不到，实测「(none)」
坑）；杀插件/禁用钩子两场景都验「像素回 #282C34 + OSC 11 回
2828/2c2c/3434 + 无复活/进程收割」；fakesh 每拍补输出字节刷新 cyclic
探针槽，防回退断言被旧主题帧空洞通过。连续 8 轮通过。

已知残留：分发物的「换主题」入口 = 用户本地装插件（DISTRIBUTION.md 有
步骤）；宿主无主题切换 UI（刻意，PRODUCT「颜色」行的不做项）。

## G：字形回退——CoreText 系统级回退链（2026-08-29，第二轮用户反馈）

### 复现（修复前，2026-08-29 本机取证）

单字体 Menlo + CTLine 隐式 cascade。事实（examples 探针，已删，事实进
测试）：制表画框/符号/变音符/希腊/西里尔 Menlo 自己覆盖（真字形）；
中文/假名 → PingFang、emoji → AppleColorEmoji（mono 降级，有字形）；
**Powerline U+E0B0-E0B3 CTFontCreateForString 返回 LastResort（豆腐），
且 E0B0/E0B2 渲染位图逐像素相同（同一张 LastResort 豆腐图）**。用户机
装有 Powerline/Nerd 字体（ProFontForPowerline 覆盖 E0B0-E0B3，
JetBrainsMonoNFM 覆盖 E0B4/F00B0），但它们不在系统 cascade list 里，
隐式回退永远够不到 → 大量 PUA 字符豆腐。

### 修复（font.rs，全部 CoreText 系统级；禁止打包字体/第三方字体引擎——
STACK 红线）

按「簇首字符」三步解析（`Font::resolve_slot`，缓存命中零光栅化）：

1. 基础字体覆盖（`CTFontGetGlyphsForCharacters` 全非 0；代理对字形落
   高代理槽、VS/ZWJ 容忍 0）→ 基础字体（槽 0）；
2. `CTFontCreateForString(base, text, 真实 range)`（沿系统 cascade）真
   覆盖 → 回退槽（中文→PingFangSC、emoji→AppleColorEmoji）；返回
   LastResort = cascade 无源；
3. 惰性扫一次全字体集合（`CTFontCollection` + matching descriptors，
   ~874 字体）找覆盖源 → 用户装的 Powerline/Nerd 字体由此够到
   （E0B0→ProFontForPowerline，F00B0→JetBrainsMonoNF）；
4. 三步全空 → 残留如实记录（`Font::residuals` + eprintln 一次/码点），
   渲染回基础字体（CTLine 自动回退画 LastResort 豆腐）。**不打包 Nerd
   Font**；系统装了 Nerd Font 就自动用（不写死字体名，扫的是覆盖性）。

atlas 槽位改 字形+字体槽 双维度（`HashMap<槽位, HashMap<文本, GlyphRect>>`
×四字重）：同一文本换字体槽绝不复用旧字体位图；命中路径两次哈希零分配
（D-C 纪律保持）。

宽字形按 East Asian Width：vt 核 `CellWide::Wide`（EAW 双宽）驱动背景
 quad span 两格（原已对）；**下划线/删除线旧实现只画一格宽——CJK 宽字
形装饰后半截缺失，修为跟 span 两格**（`build_cell_pass`）。IME 预编辑
落格本来就走 `codepoint_width`（EAW），不动。

### 验收（复跑）

- 逐类像素探针（非空白非豆腐）：`cargo test -p ninja --lib
  font::tests::fallback_renders_acceptance_categories`——制表画框
  │┌┐└┘├┤┬┴┼═║ / 符号 →←⇄✓✗●▲△◆★☆ / 中文假名 / emoji（mono 降级有
  字形）/ 变音符-希腊-西里尔，每样本 ink>0 且解析字体 ≠LastResort；
- Powerline：`powerline_renders_or_records_residual`——有回退源（本机
  ProFontForPowerline）→ 真字形且 E0B0/E0B2 位图必须不同；无源 → 残留
  如实记录。真无源码点（U+0378 未分配）：`no_source_codepoint_records_
  residual`；
- atlas 双维度：`atlas::tests::slots_keyed_by_font_dimension`；
- 宽字形装饰两格 + 占位格跳过：`renderer::tests::
  wide_cell_decorations_span_two_cells_and_spacers_skipped`（纯函数，
  无 Metal）；
- CJK 双宽选中复制：`select::tests::cjk_double_width_selection_copy`
  （线性/单字/矩形三选法，汉字不重复不丢）；
- 同帧上传 + 回退字体首帧：`renderer::tests::
  first_frame_glyphs_uploaded_same_frame`（帧内容加了中/emoji——回退槽
  字形同帧进 atlas 纹理，跳帧判据不吃掉）；
- CLI 取证：`cargo run -p ninja --example g_fallback_probe`（逐样本打
  解析字体 + 墨迹量）。

### 已知残留

- 未分配码位/系统确实无字形的码位 → LastResort 豆腐如实呈现（eprintln
  + `Font::residuals` 可查），产品决策：不打包字体；
- emoji 为灰度 mono 降级（产品决策接受：有字形即可；颜色 emoji 需彩位
  图管线，第一年不做）；
- 全字体集合扫描只在「cascade 返回 LastResort」时触发且按码点缓存，
  首个 PUA 码点约 20ms（一次），命中后零成本。

# 插件面板 v2：单一 spawn 策略「启用即拉起」（2026-08-29 用户产品决策修订）

## 决策与范围

2026-08-29 用户产品决策修订（覆盖此前任何 spawn 模式设计）：**不做
enable/hit 两类，就一类**——enabled 名单里的插件宿主启动即拉起；面板
开关 on = 立即拉起、off = 立即杀 + 回收。preview 也一样（idle 语义改为
「进程在跑、socket 在、等 hit」）。旧的「启用≠常驻」（PRODUCT.md
「不用不加载」句）由本决策废止，改写为「启用即拉起，禁用即退出回收」；
空载门禁不受影响——默认零插件时依然零进程零 socket。实施时工作区里
已有半成品的 `[plugins.spawn] name = "enable"|"hit"` 两模式实现
（config.rs 解析 + plugins.rs `SpawnMode`），按「不留死配置面」原则
整体删除简化为单一策略；旧配置若带 spawn 段 → deny_unknown_fields
整体降级默认（与其它未知字段同语义，启动不炸，config 单测钉死）。

## 改动落点

- `plugins.rs`：单一策略核心——`spawn_enabled_now()`（宿主启动 /
  p6 再启用 / 面板开共用的拉起口）；`session_enable/session_disable`
  （单插件面板开关的宿主半边：off = 杀名下子进程 + pump 同步排干 EOF
  → 收层/回退色板（与 p6 插件死亡同一条 drop_conn 通路）；名单空 =
  整面 shutdown 删 socket）；`snapshot()`（名/启用/在跑/pid/内存/
  最后错误，内存 = proc_pid_rusage ri_phys_footprint）；分发器槽改
  **强 Arc**（运行中从零拉起插件需要可造新 host，栈上 Option 不再
  够用；退出收口仍走 applicationWillTerminate → host_shutdown 幂等，
  崩溃/SIGKILL 尸体照旧由 sweep 清扫）；拉起后开 5s「等首个连接」
  窗口（SPAWN_PENDING）钉住泵 timer——连接即推的 theme.set 靠泵消化
  （无层无覆盖时泵本会自停），首个连接 accept 到即关窗，过期也关窗
  （拉不起/挂死的插件不拖住空转红线）。
- `panel.rs`（新）：极简面板窗（⌘, / App 菜单「Plugins…」）——每行
  checkbox 开关 + 名 + 状态（运行中 pid · x.x MB | 已停止(原因) |
  已停用），1s 刷新、关窗即停；行发现 = enabled ∪ paths 键 ∪ 插件
  目录文件。开关动作与 NINJA_PANEL_PLUGIN_FILE 钩子同走
  `panel::toggle` → `plugins::toggle_plugin` + `config::
  save_plugins_enabled`（同一套幂等生命周期 + 同一处写回）。
- `config.rs`：删 spawn 段解析（连同 PluginsToml 字段与测试）；
  `rewrite_plugins_enabled` 保留（面板写回：只换 enabled 数组字面量，
  缩进/尾注释/其它节全保留——不用 serde 重序列化）。
- `app.rs`：菜单 App 区加「Plugins…」（⌘,，ACTION_NAMES 可重绑）；
  applicationDidFinishLaunching → spawn_startup_plugins（runloop
  就绪后拉起）；NINJA_PANEL_PLUGIN_FILE 钩子（"open" 开面板窗口 /
  "<name> on|off" 走 toggle——E2E 编程触发，免合成 CGEvent）。

## 实测踩坑（复跑必读）

- **proc_pid_rusage 缓冲必须给足**：内核按 flavor 的完整结构体写
  （v6 = 16B uuid + 31×u64）；按 v2 头文件的前缀结构（16+5×u64）开
  缓冲会被内核**写穿栈**（实测 SIGBUS）。偏移也要对 SDK 头：
  ri_phys_footprint 在 uuid + 7×u64 之后（偏移 72；老版头文件的
  energy_wkups 布局是错的），与 `footprint` 工具实测同值（macOS
  26.6 arm64）。
- objc2 `define_class!` 方法体里写复杂链式 `let-else` + 闭包会触发
  宏展开的类型推断怪错（Option<str> 之类），把实现挪到宏外普通
  `impl` 方法（selector 只做转发）即好——同 app.rs 的既有惯例。
- NSStackView `initWithFrame`/`checkboxWithTitle_target_action`/
  `buttonWithTitle_target_action` 都是安全构造器（unsafe 块反而
  warn）；target 传 `as_super().as_super()` 上转的 &AnyObject。

## 验收取证（复跑）

- 单测：`cargo test --workspace`（96 lib + 各集成/协议全绿；含
  `toggle_plugin_single_strategy_lifecycle`（空载→on 从零拉起→
  snapshot 报 pid/内存→off 杀进程+socket 删）、
  `session_enable_off_missing_binary_reports_error`（拉不起 →
  last_error）、`spawn_pending_window_pins_pump`（泵钉住窗口）、
  config 的 `no_spawn_section_is_accepted`/`rewrite_*`）。
- E2E（NINJA_E2E=1，先 cargo build -p ninja-theme -p ninja-preview）：
  - `theme_switch` 3/3：**零点击**启用即拉起（像素 #002B36 + OSC 10/11
    双证据，open_probe 恒空）→ 杀插件回 ODP 不复活；p6 钩子 off 回
    ODP + socket 删；**面板钩子** off → 回 ODP + socket 删 +
    ninja.toml 写回 `enabled = []`（paths/注释保留）→ on → 重绑 +
    色板重新生效 + toml 回 `["theme"]`（含 "open" 先真开面板窗口）。
  - `off_is_light` 3/3：用一次→Esc→禁用 → footprint 回 p2 基线
    （36+4MB 容差）；**再启用 → preview 立即重拉**（启用即拉起，断言
    已更新）；SIGKILL 尸体清扫；正常退出无残留。
  - `layer_preview` 2/2（启动即拉起后首击即认领）；`idle_no_plugins`
    1/1（默认零插件零 socket 零进程——门禁不回归）；`hit_dispatch`
    3/3；`one_dark_startup` 1/1；`ctrl_c`/`dirty_frame_skip`/
    `fast_shell_first_frame`/`vt_smoke` 全绿。
  - `cmdw_surface_close` 4 例在干净 HEAD 上同样失败（本机 CGEvent
    投递环境问题，非本变更回归；stash 复跑取证）。
