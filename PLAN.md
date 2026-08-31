# Ninja 实现计划

依据 [PRODUCT.md](PRODUCT.md)。本文是唯一实现合同。主干现在只有这两份文档；实现从这里重做。

路径：**像 cmux 一样站在 Ghostty 上，只做插件系统。** 终端核、绘制、PTY、配置、键位用 libghostty；ninja 是 ADE 宿主。不自研引擎，不把浏览器 / Agent / 工作区做进宿主。

历史代码仍在 git（tag `v1-engine` 是自研引擎快照；其后提交是 libghostty 嵌入实验）。不当现行实现，不从那份树继续打补丁。

## 所有权

| 层 | 谁拥有 | 合同 |
| --- | --- | --- |
| PTY / VT / GPU | Ghostty（libghostty 嵌入 API） | 钉 commit。不自研引擎。不为产品功能去重度 fork Ghostty。 |
| 窗 / 标签 / 分屏 | 空载宿主 | 用 Ghostty 的 window/tab/split 回调，但是 ninja 的壳，不是插件。 |
| 终端配置和键位 | Ghostty | `~/.config/ghostty/config` 直接生效。菜单键位由 `config_trigger` 推导。ninja 特有动作认领 Ghostty 封闭动作集里的空位（插件面板 = `toggle_visibility` → ⌘,）。 |
| 插件名单 / 面板 | ninja.toml + 宿主 | 只声明启用哪些插件和宿主特有项。不另做一套终端键位。 |
| ADE 协议 | ninja | 进程外、版本化 JSON。插件不链 `ghostty.h`，不链宿主内部 API。Ghostty 语义坑（⌘+hover 链、无 `config_set`、动作集封闭）停在宿主适配器。 |
| 插件进程 | ninja | 启用即拉起，禁用即退出回收。空载零 socket、零插件进程。 |

## 技术锁定

未列出的库、平台、运行时默认不进仓库。

| 层 | 选择 | 不选 |
| --- | --- | --- |
| 宿主语言 | Rust | Zig / C++ / Swift 作为宿主 |
| 第一年平台 | macOS | Linux / Windows 同期 |
| 窗口 / IME | AppKit（`objc2`） | winit、egui、iced、GPUI 当产品壳 |
| 终端核 | vendored libghostty 嵌入（`include/ghostty.h`，静态链） | 自研 Metal atlas、`alacritty_terminal`、VTE、只用 `libghostty-vt` |
| 配置 | Ghostty 配置 + 收缩的 ninja.toml | 平行 TOML 键位层、Lua / JS 配置即脚本 |
| 插件形态 | 子进程 | 宿主内动态库、Lua VM、常驻 Node、宿主内置面板 |
| 插件传输 | Unix domain socket | 共享地址空间 API、gRPC、HTTP |
| 插件编码 | 长度前缀 JSON，消息带 `v` | Protobuf / Cap'n Proto 作为 v0 |
| 层的像素 | 宿主建 IOSurface，插件写入，合成在 Ghostty Metal 层之上 | 插件自己弹窗口、宿主代渲染文件内容 |
| 标签 / 分屏 | 空载宿主 | 插件、tmux 顶替宿主布局 |
| 预览插件 | 文本和代码 pager | 图片、PDF、目录、系统打开器 |
| 分发 | 签名的 macOS .app | 只提供 `cargo run` |
| 开源 | 三项验证通过之后 | 第一天公开 |
| Ghostty 钉点 | 开工时钉一个 ghostty commit + 匹配的 zig | 跟踪 Ghostty HEAD；cmux 式重度 fork |

官方示例插件（文本预览、theme.set）同仓库、独立 binary，只通过 JSON 协议说话。第二个实现可以不用 Rust。

ADE 协议 v0 六类：`hit` / `layer` / `input` / `spawn` / `config` / `theme`。新原语必须已有第二个独立插件需要，才能进协议。线格式：Unix socket 上 `u32le 长度 + UTF-8 JSON`。宿主忽略未知字段；插件碰到不支持的 `v` 必须退出。

嵌入 API 官方声明 pre-1.0，破坏性变更是预期。钉 commit，升级显式做。

## 目标树

```text
ninja/
  PRODUCT.md
  PLAN.md
  crates/ninja            产品宿主（libghostty 嵌入）
  crates/ghostty-sys      FFI（bindgen include/ghostty.h）
  crates/ninja-protocol   协议
  crates/ninja-preview    示例插件：文本/代码预览
  crates/ninja-theme      示例插件：theme.set
  vendor/ghostty          钉 commit 的源与嵌入库构建（src/ 与 out/ 不入库）
```

空载进程只有宿主。插件运行时不进空载路径。

## 明确推迟

- wasmtime / WIT。WASM 是以后的分发格式，必须说同一套 JSON 协议。
- Linux / Windows。协议先在 macOS 上钉死。
- 插件市场、Agent、工作区、内置浏览器。
- 为产品功能去 fork Ghostty。缺能力先适配；适配不了再单独立项，不默默补丁膨胀。

## 阶段

执行单位是**一个阶段**。每个阶段：盘点 → 实施 → 独立验证。验证失败则停。不要一次做完整条链。

### 进度

- q0 嵌入底座 ✅ 2026-08-31（审计 docs/Q0-CAPABILITY-AUDIT.md，overall PASS；hit 两数据源均有 API；取证跑虚拟屏）
- q1 壳 ✅ 2026-08-31（取证 docs/q1-evidence/：虚拟屏 E2E 45 断言全绿两轮 + 纯逻辑单测 12；⌘W 双路径只关一面、⌘⇧Enter 三态、EOF、焦点/resize 全链实测）
- q2 配置 ✅ 2026-08-31（取证 docs/q2-evidence/：虚拟屏 E2E 38 断言全绿 + 纯逻辑单测 11；ODP 缺省像素/Dracula 真实生效且让位/⌘G·⌘⇧P 重绑/#ff00ff 热重载像素传播/ninja.toml 收缩/q0 回归 PASS）
- q3 插件系统 + 三门禁 ✅ 2026-08-31（取证 docs/q3-evidence/：虚拟屏 E2E 47 断言全绿两轮 + q0/q1/q2 回归 PASS；三门禁=空载 0.55× Ghostty、⌘click 路径→层内看文本→Esc 回焦点、关掉即轻三场景）
- q4 分发 ✅ 2026-08-31（brew tap + cask/DMG：本地 file:// tap `brew install --cask ninja` 装 .app；Apple Development 真签、无公证 Gatekeeper 行为已实测记录；卸载无残留。证据 docs/q4-evidence/）

### q0 嵌入底座

做：钉版构建 libghostty；Rust FFI；一个 AppKit 窗口挂一个 surface，跑 shell、能输入能渲染。

过：嵌入 surface 真渲染真 PTY。能力审计逐项给出「有 API / 无 / 绕法」：网格与 hyperlink、屏幕快照、surface 之上合成层、配置加载与运行时改、键位拦截。hit 数据源无且无绕法 → 停。

### q1 壳

做：window/tab/split 接布局树；焦点 / 关闭 / resize 全链。

过：标签分屏日常用法成立；⌘W、⌘⇧Enter 语义清楚（多 pane 关一面；放大焦点面）。

### q2 配置

做：加载 Ghostty 配置（主题、字体、键位）+ 热重载。ninja.toml 只管插件 / 宿主特有项。缺省主题 One Dark Pro。

过：用户既有 ghostty 配置的常用子集直接生效；主题 / 字体 / 键位实测。

### q3 插件系统 + 三门禁

做：hit、layer、input、theme.set。示例插件只走公开协议。Ghostty 语义坑停在宿主适配器，不改协议。

过：**三大门禁全部通过**（空载内存对照 Ghostty 本尊、第一个插件、关掉即轻）。

### q4 分发（brew tap + cask/DMG）

做：Homebrew tap 分发（2026-08-31 用户决策：不购 99 刀 Developer ID、不做公证）。tap 仓库布局 + **cask**：`brew install --cask ninja` 下载 DMG 并安装 .app（拖拽式 DMG 由打包脚本产出；cask 钉 version + sha256）；产物 .app 用本机真实签名身份（Apple Development，无身份即失败，不出 adhoc）；默认零插件（DMG 不含 ninja-preview/ninja-theme）。DISTRIBUTION.md 与实际一致。

过：本地 tap（file://）+ 本地 DMG `brew install --cask` 全新安装 → 打开即日常终端（虚拟屏取证抽查）；`brew uninstall --cask` 无残留；隔离副本的 Gatekeeper 行为实测并如实记录（无公证时隔离属性的影响与处理——`--no-quarantine` / cask quarantine 语义，写进文档）。仓库仍不公开；DMG 公开托管与 tap 公开发布是后续决定。

## Workflow 合同

三角色：

1. **盘点**：只读。对照本阶段「过」的标准写差距，不改代码。
2. **实施**：只做本阶段。禁止加 Agent、图片预览、市场、Linux、Ghostty 重度 fork。
3. **验证**：新会话，不看实施者的自我声明。按「过」的标准抓缺陷。有缺陷就停，不自动开下一阶段。

实施者与验证者不得同 thread。验证失败允许实施者修一轮，再验一次；第二次仍失败则停。

### E2E 虚拟屏幕（2026-08-31 增补）

GUI 取证默认在虚拟屏上跑，不打扰开发者主屏：

- 工具 `scripts/e2e/virtual-display.m`（CGVirtualDisplay 私有 SPI，DeskPad 同款手法；编译命令见文件头）。`hold [w h hidpi]` 常驻创建虚拟屏（进程退出即拔屏，stdout 一行 JSON 出 displayID）；`list` / `screens` 清点（CG 层 / AppKit 层）。E2E 脚本模式：起 `hold` → 带 `NINJA_E2E_SCREEN=<displayID>` 跑宿主 → `kill` 收尾。
- 宿主识别 `NINJA_E2E_SCREEN=<displayID>`：设置时窗口落在该 NSScreen（按 NSScreenNumber 匹配）；未设置走系统默认。这是取证钩子，不是产品配置，不进 ninja.toml。
- 截图取证按窗口 ID（`screencapture -l` / CGWindowListCreateImage），与窗口在哪块屏无关。
- 键盘取证优先走嵌入 API 直灌（surface key 接口），避免系统级 CGEvent 抢开发者焦点。
- 虚拟屏不可用（无 GUI 会话、CI、别的机器）→ 回退主屏，取证输出标注实际用的屏。

## 重开条件

只有这些事实才能改本合同，不能因为顺手改：

1. 空载内存无法接近 Ghostty，且归因于嵌入路径本身。
2. 插件层无法可靠合成到 Ghostty surface 之上。
3. 嵌入 API 无法提供命中所需的网格 / 链接，且无宿主侧绕法。
4. Ghostty 嵌入 API 破坏到无法钉 commit 维持。

除此之外，先把空载、点击路径、关掉即轻跑通。
