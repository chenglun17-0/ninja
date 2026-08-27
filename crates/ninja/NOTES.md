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
cargo test -p ninja            # 23 全绿（19 lib + 4 FFI），0 warning
./target/debug/ninja           # 拉起 $SHELL（-zsh 登录 shell）
```

| 验收项 | 取证方式 | 结果 |
| --- | --- | --- |
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
