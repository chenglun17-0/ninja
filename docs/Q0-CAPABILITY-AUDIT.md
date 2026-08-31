# q0 能力审计报告：libghostty 嵌入 API（新树实测）

- 钉点：ghostty `a887df42c56f6de86c0fe6da9c4eeca37931e083`（1.3.2-dev；`minimum_zig_version = 0.15.2`，与本机钉版 `/usr/local/bin/zig` 一致）；codeload tarball sha256 `fb4b2f9f…866b6`（fetch.sh 校验，本次实测匹配）。
- 构建：`vendor/ghostty/build.sh` → `out/lib/libghostty-internal.a`（静态合并归档，ReleaseFast，本次 141MB）+ `out/include/ghostty.h`（1209 行公开嵌入 API）。
- FFI：`crates/ghostty-sys`（bindgen 0.72 对安装出的 `ghostty.h` 生成绑定，静态链入；`nm target/debug/ninja` 可见 `_ghostty_app_new`/`_ghostty_surface_new`/`_ghostty_surface_read_text` 等）。
- 实测：`cargo run -p ninja -- --evidence-dir docs/q0-evidence`（自驱动取证机，本次新树上跑出 **overall: PASS**，2026-08-31；按 PLAN「E2E 虚拟屏幕」增补跑在虚拟屏 `NINJA_E2E_SCREEN=5`（1920x1080 像素 1:1，`scripts/e2e/virtual-display hold` 建屏），未打扰主屏，report.txt `screen:` 行有标注）。本报告所有「实测」均出自 `docs/q0-evidence/`（demo.log / report.txt / 截图 / 网格文本），可重跑复核。

## 结论总表

| # | 能力 | 结论 | 一句话 |
| --- | --- | --- | --- |
| 1 | 网格读取 | **有 API** | `ghostty_surface_read_text`（VIEWPORT/SCREEN 网格坐标精确区域） |
| 2 | hyperlink 读取 | **有 API**（语义有坑） | `GHOSTTY_ACTION_MOUSE_OVER_LINK`，但 OSC-8 hover 需 ⌘/ctrl 修饰键且受 `link-previews` 门控 |
| 3 | 屏幕快照（文本） | **有 API** | 同 #1；另有 `read_selection` |
| 3' | 屏幕快照（像素） | **无直接 API；有绕法** | 宿主截自持视图层级（`screencapture -l<window>` / `CGWindowListCreateImage`），本次实测未被 TCC 拦 |
| 4 | surface 之上合成层 | **有 API（结构性）** | ghostty Metal 层挂进宿主 NSView（layer-hosting），宿主 subview/sublayer 天然在上 |
| 5 | 配置加载与运行时改 | **有 API** | `config_new/load_file/finalize/get` + `app/surface_update_config` + `CONFIG_CHANGE` 回调 |
| 6 | 键位拦截 | **有 API** | `config_key_is_binding` / `surface_key_is_binding`（派发前查询）+ `KEY_SEQUENCE` 回调 |

**hit 命门判定（q0 硬门禁）**：hit 的两个数据源——网格（#1）与 hyperlink（#2）——均有 API、且本次新树实测均取到真数据（网格文本 = `grid-viewport.txt`（274 字节，含键盘输入回显）+ `grid-region.txt`（精确区域两行）；hyperlink URL = `MOUSE_OVER_LINK url=https://ghostty.org/q0`，真 OSC-8 语义）。**ok:true，不停。**

---

## 1. 网格读取：有 API

- API：`ghostty_surface_read_text(ghostty_surface_t, ghostty_selection_s, ghostty_text_s*)`（`out/include/ghostty.h` L1161）；附带 `ghostty_surface_read_selection`（L1160，选区文本）、`ghostty_surface_free_text`（L1164，释放）。
- **实测**（`docs/q0-evidence/demo.log`，本次运行）：
  - 真 PTY：`pty: tty_name=/dev/ttys000 foreground_pid=70240`（真 bash 子进程，非模拟）。
  - 键盘回显：`surface_text("echo TYPED-VIA-SURFACE-TEXT") + key ENTER (consumed=true)` 后，视口文本里出现回显（`TYPED-VIA-SURFACE-TEXT`）——输入与渲染链路真实工作。
  - 全视口：`read_text(viewport) 274 bytes`，同时含 initial_input 的输出与键盘输入回显（`grid-read-viewport PASS`，产物 `grid-viewport.txt`）。
  - 精确区域：按全视口定位到的行号做 rows 1..2 区域读取，内容恰为 `GRID-READ-LINE-1\nGRID-READ-LINE-2`（`grid-region.txt`，`grid-read-region PASS`）——网格坐标（列×行）可精确寻址。
- 注记：文本级读取；单元格样式/旗标（含 hyperlink 属性本身）不随文本返回。hit 若需「cell + 修饰键」语义，坐标由像素→cell 换算（`ghostty_surface_size` 给 cell 宽高；本次虚拟屏实测 cell 8x18px、117x34 cells，像素 1:1）。

## 2. hyperlink 读取：有 API，两条触发语义必须知道

- API：`GHOSTTY_ACTION_MOUSE_OVER_LINK` 动作（头文件 L924；载荷 `ghostty_action_mouse_over_link_s {url,len}` L730）。由 `ghostty_surface_mouse_pos`（L1136）驱动的 hover 检测触发（vendored 源 `src/Surface.zig` L1564 `mouseRefreshLinks`）。
- **语义坑（vendored 源码证据）**：
  1. `src/Surface.zig` L4298 `linkAtPos`：**OSC-8 超链只在 ctrl/super 修饰键按下时参与 hover 判定**（macOS Ghostty 同款 ⌘+hover 预览）；无修饰键时只走配置的 `link` 正则（默认为空 → 默认无命中）。
  2. `src/Surface.zig` L1615/L1628：命中链接后，`MOUSE_OVER_LINK` 动作本身还受 `link-previews` 配置门控（显式 URL 要求 `== .true`，OSC-8 要求 `!= .false`）。
  3. `mouse_pos` 坐标语义与 macOS App 一致：view **points、原点在上**。
- **实测**（demo.log）：demo 以 `mods=⌘` 全网格扫描，`row 3 col 40` 处触发 `action MOUSE_SHAPE pointer` → `action MOUSE_OVER_LINK url=https://ghostty.org/q0`（`hyperlink-hover PASS`）——OSC-8 URL 原样取回，不走宿主正则。
- **click 命中取 URL 的用法**：hover 移到目标 cell（`mouse_pos`）→ 收 `MOUSE_OVER_LINK` 得 URL → 再 `mouse_button`（L1132）。这是 macOS Ghostty 的原生路径（mouseMoved 先于 click），对 hit 原语完全够用。
- 绕法（若不想依赖 hover 语义）：配置 `link = …` 正则后无修饰键也可 hover 命中；或 ⌘+click 直接走 `OPEN_URL` 动作路径。q3 重接 hit 时二选一，均在嵌入 API 面内。
- 另注（本次与上一轮一致的读数怪象）：`config_get(link-previews)` 对 app 级 config 句柄回读为 `false`，但 surface 层动作实际放行（OSC-8 要求 `!= .false`，surface 生效配置与句柄回读不一致）。hit 取链接不依赖该回读，判定不受影响；q2 落配置系统时再排查字段类型。

## 3. 屏幕快照

- 文本：**有 API** = #1 `read_text`（全视口即「屏幕文本快照」，含 SCREEN 坐标取滚动历史）。
- 像素：**无直接 API**（头文件无 surface 像素/纹理读取函数）。**绕法**：surface 的 Metal 层挂在**宿主自持**的 NSView 里（见 #4），所以宿主可以截自己的视图层级——demo 用 `screencapture -l<windowNumber>` 实测三张（`shot1/2/3.png`，129/130/131KB，虚拟屏像素 1:1），本次运行 TCC 未拦（被拦也不影响「有绕法」的结论，只影响像素证据形式）；进程内等价物 `CGWindowListCreateImage`。对 ADE `layer` 原语（宿主自建 IOSurface 合成）无影响。

## 4. surface 之上合成层：有 API（结构性保证）

- 结构：`ghostty_surface_config_s.platform.macos.nsview`（头文件 L449）——宿主自建 NSView 交给 ghostty；vendored 源 `src/renderer/Metal.zig` L108-111 在 macOS 上把 ghostty 的 `IOSurfaceLayer` 直接设为该 view 的 `layer`（layer-hosting view）。因此宿主对同一 view 加 subview/sublayer 即在终端之上。
- **实测**（demo.log + `pixel-sample.swift` 相对矩形平均色，可重跑；虚拟屏像素 1:1，理论值可直接对算）：
  - t≈4.1 给宿主 view 加 42% 宽、50% 透明红 subview 后：
    - `shot1`（叠加前）：左半 (22,22,30)、右区 (21,21,29) —— 两侧同色 ≈ #16161e 背景加文本。
    - `shot2`（叠加后）：左半 (22,22,30) 不变，右区 **(134,10,14)** —— 50% 红叠在渲染中的终端内容之上；理论值 ½(255,0,0)+½(22,22,30)=(138,11,15)，实测几乎逐字节吻合。
    - `shot3`（叠加 + 改背景后）：左半 **(58,42,91)** 恰为 #3a2a5b（运行时配置改色的像素级证明），右区 (153,20,45) ≈ 理论值 ½(255,0,0)+½(58,42,91)=(156,21,45) —— 层与终端同时变化，顺序保持。
- 结论：ADE `layer` 原语的「宿主合成到 surface 之上」在嵌入路径上成立（q3 再接 IOSurface 跨进程共享）。

## 5. 配置加载与运行时改：有 API

- 加载：`ghostty_config_new/load_default_files/load_file/finalize`（头文件 L1070-1077）+ `ghostty_config_get`（L1078，按字段类型写入出参指针）+ 诊断 `config_get_diagnostic`（L1084）。
- 运行时改：`ghostty_app_update_config`（L1096，app 级）/ `ghostty_surface_update_config`（L1109，surface 级，内部克隆配置，调用方仍需 `config_free` 自己的句柄）；变更通知 `GHOSTTY_ACTION_CONFIG_CHANGE`（L934）/ `GHOSTTY_ACTION_RELOAD_CONFIG`（L933）——宿主可据此实现热重载（q2）。
- **实测**：初始 `background=16161e` → 临时文件装载 `background=3a2a5b` 后 `surface_update_config`：`CONFIG_CHANGE` 回调 1 次；`config_get` 回读 bg=(58,42,91)=#3a2a5b；`shot3` 左半平均色 (58,42,91) 与目标逐字节相等 —— 真·运行时生效（`config-runtime-change PASS`）。
- 注记：C API **只能从文件加载**配置（无 `config_set`）；宿主要程序化改值就走临时文件（demo 即如此），或直接改用户配置文件 + `RELOAD_CONFIG` 语义。q2 落地时用哪个路径再定。

## 6. 键位拦截：有 API

- 派发前查询：`ghostty_config_key_is_binding`（L1082）/ `ghostty_surface_key_is_binding`（L1126，带 `ghostty_binding_flags_e` 出参：CONSUMED/ALL/GLOBAL/PERFORMABLE）。
- 派发顺序可观测：`GHOSTTY_ACTION_KEY_SEQUENCE`（L930，多段键序列进行中）/ `KEY_TABLE`（L931）回调让宿主能同步 UI。
- 主动触发绑定动作：`ghostty_surface_binding_action`（L1154，按名字执行绑定）。
- **实测**：`key_is_binding: config(⌘T)=true config(A)=false surface(⌘T)=true flags=1(CONSUMED)`；`KEY_SEQUENCE` 回调实测触发 1 次（改配置时）（`key-binding-intercept PASS`）。
- ADE `input` 原语（插件申请快捷键、层前台时键盘先给插件）→ 宿主在 `key_is_binding` 判「不是 ghostty 绑定」后再分给插件层，顺序天然正确。

---

## 构建决策与环境记录（钉点可复现）

1. **vendored**：`vendor/ghostty/`（fetch.sh 钉 commit + codeload tarball sha256；src/ 与 out/ 不入库）。完整源码 ~38MB 压缩/~100MB 解压，不入库；获取/校验/补丁/构建全脚本化，`zig != 0.15.2` 硬失败；支持 `GHOSTTY_EMBED_TARBALL` 离线预置（本次即用）。
2. **产物路线决策：zig 静态归档而非 xcodebuild xcframework**。钉点 `build.zig` 在 darwin 只装 libghostty-vt 产物、不装 GhosttyLib（源码自注 "we don't currently build on macOS this way"；官方 macOS 路线是 `macos/Ghostty.xcodeproj` 经 xcodebuild 出 Ghostty.xcframework，多 slice 通用二进制、面向 Swift GUI 宿主）。ninja 是 Rust 宿主、只要静态链入，选 cmux 路线：`patches/0001-darwin-install-static-embed-lib.patch`（+7 行，仅在 darwin 同时安装 `libghostty-internal.a` 与 `ghostty.h`，不动其它路径）+ `zig build -Dtarget=aarch64-macos -Dapp-runtime=none -Demit-xcframework=false -Demit-docs=false -Doptimize=ReleaseFast`。
3. **主机环境坑（已绕，写入 build.sh）**：
   - Xcode 26.6（17F113）默认 SDK 的 tbd stub **只有 arm64e-macos 没有 arm64-macos**（本次实测确认：`xcrun --show-sdk-path` 的 libSystem.B.tbd targets 为 `[x86_64-macos, x86_64-maccatalyst, arm64e-macos, arm64e-maccatalyst]`），zig 0.15.2 原生链接全挂；且 zig 的原生探测走 xcrun、无视 SDKROOT。绕法：`xcrun-shim/`（仅 build.sh 内 PATH 前置）把 `--show-sdk-path` 指到仍有 arm64-macos stub 的 CommandLineTools SDK（本次解析到 MacOSX15.sdk）。
   - Metal shader 编译需要 Xcode 26 的 `MetalToolchain` 组件（本机已装过；盘点时误跑 `xcodebuild -downloadComponent MetalToolchain` 刷新了一次系统资产，仓库零写入，组件确认就绪）。
4. **链接面**（`nm` 盘点归档外部未定义符号得出）：框架 AppKit/Metal/QuartzCore/CoreText/CoreGraphics/CoreVideo/IOSurface/CoreFoundation/Foundation/Carbon(HIToolbox) + `libobjc` + `libc++`；全部由 `crates/ghostty-sys/build.rs` 以 `cargo:rustc-link-*` 声明。
5. **bindgen**：allowlist 仅 `ghostty_*/GHOSTTY_*`（公开嵌入面）；对**安装出的** `out/include/ghostty.h` 生成（保证与链接产物同源）；`prepend_enum_name=false` 保持头文件常量名；`-DGHOSTTY_STATIC`。
6. 版本串 `1.3.2-master-+f3bf497`：ghostty 构建期从**所在仓库**的 git 取版本描述，vendored 源在 ninja 仓库内所以带了 ninja 的 commit——纯展示问题，不影响 API。
7. 本次实施期修复：新写 build.sh 时 `--prefix` 误写 `${PWD}/out`（`cd src` 后展开，产物落进 src/out），实测暴露后改回 `${PWD}/../out`——上一轮该处曾因 0001/0002 补丁叠加引入过别的问题（历史 cac180e），此路径敏感，改动必须跑 `test -f out/lib/libghostty-internal.a` 兜底（已有）。
8. **E2E 虚拟屏对齐（PLAN 2026-08-31 增补，与本阶段同期落地）**：本报告取证跑在虚拟屏上（`scripts/e2e/virtual-display hold` 建屏 → `NINJA_E2E_SCREEN=<displayID>` 落窗，`ninja`（宿主 crate，2026-08-31 由 ninja-embed 更名，对齐 PLAN 目标树）已识别该钩子；同一提交内也验证过回退路径：NINJA_E2E_SCREEN 空值/未匹配 → 主屏，report `screen:` 行标注）。虚拟屏 hidpi=0、像素 1:1，取证数值更干净。

## 红线自查（q0 范围）

- 不建插件运行时/socket/进程：本次树只有 `ghostty-sys` + `ninja` 两个 crate（宿主原落名 ninja-embed，同日更名），无任何插件、协议、spawn 代码；空载路径零插件。
- 不做 Agent、图片/PDF 预览、市场、Linux：无相关代码。
- 不重度 fork Ghostty：仅 `patches/0001`（+7 行，darwin 静态安装）；q2 范围的 0002（themes）本次不加。
- 只做本阶段：q1 的 window/tab/split、q2 的配置系统均未开工。

## 复现

```sh
tools/README.md 里的方式装 zig 0.15.2   # zig version 必须是 0.15.2
vendor/ghostty/build.sh                 # 构建（fetch 校验 + 补丁 + zig，ReleaseFast）
cargo build                             # bindgen + 静态链入（首次自动触发上面的构建）
xcrun --sdk macosx clang -fobjc-arc -framework Foundation -framework CoreGraphics \
      -framework AppKit -Wl,-undefined,dynamic_lookup \
      scripts/e2e/virtual-display.m -o scripts/e2e/virtual-display   # E2E 虚拟屏工具（一次性）
./scripts/e2e/virtual-display hold &    # stdout 一行 JSON 取 displayID（进程退出即拔屏）
NINJA_E2E_SCREEN=<displayID> cargo run -p ninja -- --evidence-dir docs/q0-evidence  # ~8s，需 GUI 会话
kill %1                                 # 拔虚拟屏
cat docs/q0-evidence/report.txt         # 五项检查 + overall
swift docs/q0-evidence/pixel-sample.swift docs/q0-evidence/shot3-config-change.png 0.10 0.35 0.40 0.65
                                        # → (58,42,91) = #3a2a5b 像素级配置生效
```

（本报告引用的证据文件：`docs/q0-evidence/{demo.log,report.txt,grid-viewport.txt,grid-region.txt,shot1-terminal.png,shot2-overlay.png,shot3-config-change.png,config-initial.txt,config-change.txt,pixel-sample.swift}`。）
