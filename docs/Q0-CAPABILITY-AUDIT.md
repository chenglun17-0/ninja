# q0 能力审计报告：libghostty 嵌入 API

- 钉点：ghostty `a887df42c56f6de86c0fe6da9c4eeca37931e083`（1.3.2-dev；`minimum_zig_version = 0.15.2`，与本机钉版 `/usr/local/bin/zig` 一致）
- 构建：`vendor/ghostty/build.sh` → `out/lib/libghostty-internal.a`（静态合并归档，ReleaseFast）+ `out/include/ghostty.h`（1209 行公开嵌入 API）
- FFI：`crates/ghostty-sys`（bindgen 0.72 对安装出的 `ghostty.h` 生成绑定，静态链入）
- 实测：`cargo run -p ninja-embed`（自驱动取证机，产物见 `docs/q0-evidence/`；本报告所有「实测」均出自 `docs/q0-evidence/demo.log` 与 `report.txt`，可重跑复核）

## 结论总表

| # | 能力 | 结论 | 一句话 |
| --- | --- | --- | --- |
| 1 | 网格读取 | **有 API** | `ghostty_surface_read_text`（VIEWPORT/SCREEN 网格坐标精确区域） |
| 2 | hyperlink 读取 | **有 API**（语义有坑） | `GHOSTTY_ACTION_MOUSE_OVER_LINK`，但 OSC-8 hover 需 ⌘/ctrl 修饰键且受 `link-previews` 门控 |
| 3 | 屏幕快照（文本） | **有 API** | 同 #1；另有 `read_selection` |
| 3' | 屏幕快照（像素） | **无直接 API；有绕法** | 宿主截自持视图层级（`screencapture -l<window>` / `CGWindowListCreateImage`） |
| 4 | surface 之上合成层 | **有 API（结构性）** | ghostty Metal 层挂进宿主 NSView（layer-hosting），宿主 subview/sublayer 天然在上 |
| 5 | 配置加载与运行时改 | **有 API** | `config_new/load_file/finalize/get` + `app/surface_update_config` + `CONFIG_CHANGE`/`RELOAD_CONFIG` 回调 |
| 6 | 键位拦截 | **有 API** | `config_key_is_binding` / `surface_key_is_binding`（派发前查询）+ `KEY_SEQUENCE`/`KEY_TABLE` 回调 |

**hit 命门判定（q0 硬门禁）**：hit 的两个数据源——网格（#1）与 hyperlink（#2）——均有 API、均已实测取到真数据（网格文本 = `grid-viewport.txt`/`grid-region.txt`；hyperlink URL = `MOUSE_OVER_LINK url=https://ghostty.org/q0`）。**ok:true，不停，可进 q1。**

---

## 1. 网格读取：有 API

- API：`ghostty_surface_read_text(ghostty_surface_t, ghostty_selection_s, ghostty_text_s*)`（`include/ghostty.h` L1161；实现 `src/apprt/embedded.zig` L1630 `readTextLocked`）。选区坐标四选一：`GHOSTTY_POINT_ACTIVE/VIEWPORT/SCREEN/SURFACE`（头文件 L399-405 附近枚举），`COORD_EXACT/TOP_LEFT/BOTTOM_RIGHT`。文本经 `ghostty_surface_free_text` 释放。附带：`ghostty_surface_read_selection`（L1160，选区文本）。
- **实测**（`docs/q0-evidence/demo.log`）：
  - 全视口：`read_text` 一次取回 274 字节，同时含 `initial_input` 的输出与键盘输入回显（`grid-read-viewport PASS`）。
  - 精确区域：按全视口定位到的行号做 rows r..r+1 区域读取，内容恰为 `GRID-READ-LINE-1\nGRID-READ-LINE-2`（`grid-region.txt`，`grid-read-region PASS`）——网格坐标（列×行）可精确寻址。
- 注记：文本级读取；单元格样式/旗标（含 hyperlink 属性本身）不随文本返回。hit 若需「cell + 修饰键」语义，坐标由像素→cell 换算（`ghostty_surface_size` 给 cell 宽高，demo 已验证换算成立）。

## 2. hyperlink 读取：有 API，两条触发语义必须知道

- API：`GHOSTTY_ACTION_MOUSE_OVER_LINK` 动作（头文件 L924；载荷 `ghostty_action_mouse_over_link_s {url,len}` L727-730）。由 `ghostty_surface_mouse_pos`（L1136）驱动的 hover 检测触发（`src/Surface.zig` `mouseRefreshLinks` L1560）。
- **语义坑（源码证据）**：
  1. `src/Surface.zig` L4298 `linkAtPos`：**OSC-8 超链只在 `ctrlOrSuper` 修饰键按下时参与 hover 判定**（macOS Ghostty 同款 ⌘+hover 预览）；无修饰键时只走配置的 `link` 正则（`src/config/Config.zig` `links` 默认为空 → 默认无命中）。
  2. `src/Surface.zig` L1615/L1628：命中链接后，`MOUSE_OVER_LINK` 动作本身还受 `link-previews` 配置门控（显式 URL 要求 `== .true`，OSC-8 要求 `!= .false`；默认 `.true`，Config.zig L1466）。
  3. `mouse_pos` 坐标语义与 macOS App 一致：view **points、原点在上**（`SurfaceView_AppKit.swift` mouseMoved 传 `frame.height - pos.y`；`embedded.zig` `cursorPosToPixels` 只做缩放）。
- **实测**：demo 以 `mods=⌘` 全网格扫描，`action MOUSE_OVER_LINK url=https://ghostty.org/q0` 触发（`hyperlink-hover PASS`）——OSC-8 URL 原样取回，不走宿主正则。
- **click 命中取 URL 的用法**：hover 移到目标 cell（`mouse_pos`）→ 收 `MOUSE_OVER_LINK` 得 URL → 再 `mouse_button`。这是 macOS Ghostty 的原生路径（`mouseMoved` 先于 click），对 hit 原语完全够用。
- **绕法（若不想依赖 hover 语义）**：配置 `link = ...` 正则后无修饰键也可 hover 命中（`linkAtPos` 落到 `linkAtPin` 正则分支）；或 ⌘+click 直接走 `OPEN_URL` 动作路径（`Surface.zig` L4389-4422 一带，demo 已挂回调探针可取 URL）。q3 重接 hit 时二选一，均在嵌入 API 面内。

## 3. 屏幕快照

- 文本：**有 API** = #1 `read_text`（全视口即「屏幕文本快照」，含 SCREEN 坐标取滚动历史）。
- 像素：**无直接 API**（头文件无 surface 像素/纹理读取函数）。**绕法**：surface 的 Metal 层挂在**宿主自持**的 NSView 里（见 #4），所以宿主可以截自己的视图层级——demo 用 `screencapture -l<windowNumber>` 实测三张（`shot1/2/3.png`，合计 ~1.3MB，像素差分验证见下），进程内等价物 `CGWindowListCreateImage`。对 ADE `layer` 原语（宿主自建 IOSurface 合成）无影响。

## 4. surface 之上合成层：有 API（结构性保证）

- 结构：`ghostty_surface_config_s.platform.macos.nsview`（头文件 L449-450）——宿主自建 NSView 交给 ghostty；`src/renderer/Metal.zig` L100-130 在 macOS 上把 ghostty 的 `IOSurfaceLayer` 直接设为该 view 的 `layer`（layer-hosting view）。因此宿主对同一 view 加 subview/sublayer 即在终端之上。
- **实测**：demo 在 t≈4.1 给宿主 view 加 42% 宽、50% 透明红的 subview（wantsLayer + CALayer）后截图：
  - `shot2` 右侧 42% 区域平均色 (96,33,29) 明显偏红、左半 (27,27,32) 不变 —— 半透明层叠在**渲染中的终端内容**之上（若盖在纯底色上不会保留暗色通道）。
  - `shot3`（叠加改背景后）右区 (107,40,50)、左区 (51,42,74) —— 层与终端同时变化，顺序保持。
- 结论：ADE `layer` 原语的「宿主合成到 surface 之上」在嵌入路径上成立（q3 再接 IOSurface 跨进程共享）。

## 5. 配置加载与运行时改：有 API

- 加载：`ghostty_config_new/load_cli_args/load_file/load_default_files/load_recursive_files/finalize`（头文件 L1070-1077）+ `ghostty_config_get`（L1078，按字段类型写入出参指针）+ 诊断 `config_diagnostics_count/get_diagnostic`（L1083-1084）。
- 运行时改：`ghostty_app_update_config`（L1096，app 级）/ `ghostty_surface_update_config`（L1109，surface 级，内部克隆配置，调用方仍需 `config_free` 自己的句柄）；变更通知 `GHOSTTY_ACTION_CONFIG_CHANGE`（L934，载荷新 `ghostty_config_t`）/ `GHOSTTY_ACTION_RELOAD_CONFIG`（L933，`soft` 标记）——宿主可据此实现热重载（q2）。
- **实测**：初始 `background=16161e` → `surface_update_config(background=3a2a5b)`：`CONFIG_CHANGE` 回调 1 次；`config_get` 回读 (58,42,91)=#3a2a5b；截图 `shot3` 左半平均色从 (27,27,32) → (51,42,74) —— 真·运行时生效（`config-runtime-change PASS`）。
- 注记：C API **只能从文件加载**配置（无 `config_set`）；宿主要程序化改值就走临时文件（demo 即如此），或直接改用户配置文件 + `RELOAD_CONFIG` 语义。q2 落地时用哪个路径再定。

## 6. 键位拦截：有 API

- 派发前查询：`ghostty_config_key_is_binding`（L1082，实现 `embedded.zig` L1470）/ `ghostty_surface_key_is_binding`（L1126-1128，实现 L1798，带 `ghostty_binding_flags_e` 出参：CONSUMED/ALL/GLOBAL/PERFORMABLE）。
- 派发顺序可观测：`GHOSTTY_ACTION_KEY_SEQUENCE`（L930，多段键序列进行中）/ `GHOSTTY_ACTION_KEY_TABLE`（L931，键表激活/失活）回调让宿主能同步 UI。
- 主动触发绑定动作：`ghostty_surface_binding_action`（L1154，按名字执行绑定）。
- **实测**：`config(⌘T)=true config(A)=false surface(⌘T)=true flags=1(CONSUMED)`；`KEY_SEQUENCE` 回调实测触发 1 次（改配置时）（`key-binding-intercept PASS`）。
- ADE `input` 原语（插件申请快捷键、层前台时键盘先给插件）→ 宿主在 `key_is_binding` 判「不是 ghostty 绑定」后再分给插件层，顺序天然正确。

---

## 构建决策与环境记录（钉点可复现）

1. **vendored**：`vendor/ghostty/`（fetch.sh 钉 commit + codeload tarball sha256 `fb4b2f9f…`；src/ 与 out/ 不入库）。完整源码 ~100MB，不入库；获取/校验/补丁/构建全脚本化，`zig != 0.15.2` 硬失败。
2. **产物路线决策：zig 静态归档而非 xcodebuild xcframework**。钉点 `build.zig` 在 darwin 只装 libghostty-vt 产物、不装 GhosttyLib（源码自注 "we don't currently build on macOS this way"；官方 macOS 路线是 `macos/Ghostty.xcodeproj` 经 xcodebuild 出 Ghostty.xcframework，多 slice 通用二进制、面向 Swift GUI 宿主）。ninja 是 Rust 宿主、只要静态链入，选 cmux 路线：`patches/0001-darwin-install-static-embed-lib.patch`（+7 行，仅在 darwin 同时安装 `libghostty-internal.a` 与 `ghostty.h`，不动其它路径）+ `zig build -Dtarget=aarch64-macos -Dapp-runtime=none -Demit-xcframework=false -Demit-docs=false -Doptimize=ReleaseFast`。
3. **主机环境坑（已绕，写入 build.sh）**：
   - Xcode 26.6 的 SDK tbd stub **只有 arm64e-macos 没有 arm64-macos**，zig 0.15.2 原生链接全挂（连 `zig init && zig build` 都失败）；且 zig 的原生探测走 xcrun、无视 SDKROOT。绕法：`xcrun-shim/`（仅 build.sh 内 PATH 前置）把 `--show-sdk-path` 指到仍有 arm64-macos stub 的 CommandLineTools SDK（MacOSX15.sdk）。
   - Metal shader 编译需要 Xcode 26.6 新的 `MetalToolchain` 组件（`xcodebuild -downloadComponent MetalToolchain`，已装，687MB，一次性）。
4. **链接面**（`nm` 盘点归档外部未定义符号得出）：框架 AppKit/Metal/QuartzCore/CoreText/CoreGraphics/CoreVideo/IOSurface/CoreFoundation/Foundation/Carbon(HIToolbox) + `libobjc` + `libc++`；`__availability_version_check` 由链接驱动自动带的 clang_rt 解析。全部由 `crates/ghostty-sys/build.rs` 以 `cargo:rustc-link-*` 声明。
5. **bindgen**：allowlist 仅 `ghostty_*/GHOSTTY_*`（公开嵌入面）；对**安装出的** `out/include/ghostty.h` 生成（保证与链接产物同源）；`prepend_enum_name=false` 保持头文件常量名。
6. 版本串 `1.3.2-master-+8fc348a`：ghostty 构建期从**所在仓库**的 git 取版本描述，vendored 源在 ninja 仓库内所以带了 ninja 的 commit——纯展示问题，不影响 API。

## 红线自查（q0 范围）

- v1 引擎层未动：`crates/ninja` 未改一行；v1 二进制照常构建（`cargo build -p ninja` 通过）。新旧共存到 q4。
- ADE 协议与 golden 未动（`crates/ninja-protocol`、`crates/ninja-preview`、`crates/ninja-theme` 零改动）。
- 空载语义未动：主 `ninja` bin 不依赖 ghostty-sys；嵌入路径在独立 `ninja-embed` bin，空载不加载任何插件运行时。
- 未做下一阶段：无 Agent/预览/插件市场/Linux 相关改动。

## 复现

```sh
vendor/ghostty/build.sh                 # 构建（含 fetch 校验 + 补丁 + zig）
cargo build -p ninja-embed              # bindgen + 静态链入
cargo run -p ninja-embed -- --evidence-dir /tmp/q0ev   # 实测取证（~8s，需 GUI 会话）
cat /tmp/q0ev/report.txt                # 五项检查 + overall
```

（本报告引用的证据文件：`docs/q0-evidence/{demo.log,report.txt,grid-viewport.txt,grid-region.txt,shot1-terminal.png,shot2-overlay.png,shot3-config-change.png}`。）
