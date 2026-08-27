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

## 渲染修复记录（第二轮验证反馈：空闲首开全黑）

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
cargo test -p ninja            # 24 全绿（20 lib + 4 FFI），0 warning
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

- `OscParser CHANGE_WINDOW_TITLE_STR` 上游恒空（p0 记录）：不影响
  `Terminal::title()`，窗口标题正常。
- 拖选自动滚动（autoscroll tick）未接：拖到窗口外不滚屏（vt gesture 提供
  Autoscroll API，p2 接）。
- marked text 超行尾截断（预编辑串长于剩余列时裁剪，不折行）。
- 窗口跨屏移动后字体 scale 不跟随（固定主屏 scale）。
- Cmd+key 无菜单绑定时按 SUPER 编码直发 PTY（kitty 键盘协议）。
