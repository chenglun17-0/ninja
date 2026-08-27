# ninja p1 取证笔记

单终端面（AppKit + Metal + PTY + libghostty-vt）的验证记录与内存基线。

## 内存基线（p2 门禁对照）

`footprint`（Apple Silicon，M4，retina 缩放 2.0，Menlo 13pt，80x24）：

| 状态 | footprint |
| --- | --- |
| 启动 + shell 提示符空闲（6s） | **36 MB** |
| 一轮交互后（seq 1 200 输出、滚动） | 39 MB |

大头是 Metal/CoreText/AppKit 框架自身；vt 核与 atlas 在其中的占比极小。
p2 加标签/分屏时，单窗空载基线不应显著高于此数字。

## p1 验收取证（2026-08-27，Aqua 会话实测）

命令证据（验证员可独立复跑）：

- `cargo test -p ninja`：18 lib + 4 FFI 冒烟全绿。
- `./target/debug/ninja` 拉起 `$SHELL`（实测 zsh 登录 shell `-zsh`，argv[0] 带 `-`）。
- PTY 输入链路：终端内 `touch /tmp/ninja-p1-input-proof` → 文件出现。
- vt 输出链路：窗口标题 `jal@192: ~/my_repos/ninja`（OSC 0/2 → title() → setTitle）。
- 渲染链路：截图像素 ASCII 可读出提示符文本；`examples/raster_check.rs`
  dump 'A'/'g' 字形位图为正确字形形状（含 descender）。
- 中文宽字形：`printf 'NINJA-CJK:你好终端面'` 输出双宽 cell 正常。
- resize reflow：窗口 1000pt → `echo $COLUMNS` = 127；500pt → 63
  （SIGWINCH → shell 重算，全程无重启）。
- 拖选 + Cmd+C：合成鼠标拖选（CGEvent）→ 选区蓝色高亮出现（约 13k 像素命中
  选区色）→ Cmd+C → `pbpaste` 读出选区文本（含 emoji grapheme）。
- Cmd+V 粘贴：`pbcopy 'echo PASTE-PROOF-321 > /tmp/x'` → Cmd+V → Return →
  文件出现（bracketed paste 由 vt 模式 2004 门控，select.rs 单测覆盖）。
- 滚轮：CGEvent scrollWheel → 视口滚动，多次往复渲染稳定。
- 依赖树：`cargo tree -p ninja` 无 wasmtime/JS 引擎；仅 libghostty-vt 静态链
  + objc2 家族 + metal/core-text/libc。

手动项（无法全自动，需人工过一遍）：

- IME：拼音输入法敲中文 → 候选窗定位（firstRectForCharacterRange 已接光标
  cell 屏幕矩形）→ 回车上屏（insertText → PTY）→ 预编辑下划线落格渲染。
- 光标闪烁节奏（0.53s 相位翻转）。
- 复合操作手感（shift+点击扩选、option 拖矩形选区）。

## 已知残留

- `OscParser CHANGE_WINDOW_TITLE_STR` 上游恒空（p0 已记录）：不影响
  `Terminal::title()`，p1 用它驱动窗口标题，正常。
- 截图合成瞬间偶见斜楔伪影（Orca 窗口截图 + CAMetalLayer present 交叠），
  复跑同步骤不复现，顶点坐标倾倒全部正常；真实使用未见。
- 拖选自动滚动（autoscroll tick）未接：拖到窗口外不滚屏（vt gesture 提供
  Autoscroll API，p2 接）。
- marked text 超行尾截断（预编辑串长于剩余列时裁剪，不折行）。
