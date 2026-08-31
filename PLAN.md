# Ninja 实现计划

依据 [PRODUCT.md](PRODUCT.md)。本文是唯一实现合同。

路径：**像 cmux 一样站在 Ghostty 上，只做插件系统。** 终端核、绘制、PTY、配置、键位用 libghostty；ninja 是 ADE 宿主。不自研引擎，不把浏览器 / Agent / 工作区做进宿主。

## 所有权

| 层 | 谁拥有 | 合同 |
| --- | --- | --- |
| PTY / VT / GPU | Ghostty（libghostty 嵌入 API） | 钉 commit。不自研引擎。不为产品功能去重度 fork Ghostty。 |
| 窗 / 标签 / 分屏 | 空载宿主 | 用 Ghostty 的 window/tab/split 回调，但是 ninja 的壳，不是插件。 |
| 终端配置和键位 | Ghostty | `~/.config/ghostty/config` 直接生效。菜单键位由 `config_trigger` 推导。ninja 特有动作认领 Ghostty 封闭动作集里的空位（插件面板 = `toggle_visibility` → ⌘,）。 |
| 插件名单 / 面板 | ninja.toml + 宿主 | 只声明启用哪些插件和宿主特有项。`[keys]` 不复活。 |
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
| Ghostty 钉点 | `vendor/ghostty` commit `a887df42`，zig 0.15.2 | 跟踪 Ghostty HEAD；cmux 式重度 fork |

官方示例插件 `ninja-preview` / `ninja-theme` 同仓库、独立 binary，只通过 JSON 协议说话。第二个实现可以不用 Rust。

ADE 协议 v0 六类：`hit` / `layer` / `input` / `spawn` / `config` / `theme`。新原语必须已有第二个独立插件需要，才能进协议。线格式：Unix socket 上 `u32le 长度 + UTF-8 JSON`。宿主忽略未知字段；插件碰到不支持的 `v` 必须退出。

嵌入 API 官方声明 pre-1.0，破坏性变更是预期。钉 commit，升级显式做。构建需要 Zig 0.15.2。

## Workspace

```text
ninja/
  crates/ninja-embed     产品宿主（libghostty 嵌入）
  crates/ghostty-sys     FFI（bindgen include/ghostty.h）
  crates/ninja-protocol  协议
  crates/ninja-preview   示例插件：文本/代码预览
  crates/ninja-theme     示例插件：theme.set
  vendor/ghostty         钉 commit 的源与嵌入库构建（src/ 与 out/ 不入库）
```

空载进程只有宿主。插件运行时、wasmtime、预览二进制都不进空载路径。

## 明确推迟

- wasmtime / WIT。WASM 是以后的分发格式，必须说同一套 JSON 协议。
- Linux / Windows。协议先在 macOS 上钉死。
- 插件市场、Agent、工作区、内置浏览器。
- 为产品功能去 fork Ghostty。缺能力先适配；适配不了再单独立项，不默默补丁膨胀。

## 已完成（证据，不重做）

| 阶段 | 过的标准 | 证据 |
| --- | --- | --- |
| q0 嵌入底座 | surface 真渲染真 PTY；网格 / hyperlink / 合成层 / 配置 / 键位拦截均有 API 或绕法 | [docs/Q0-CAPABILITY-AUDIT.md](docs/Q0-CAPABILITY-AUDIT.md) |
| q1 壳 | 多窗 / 原生标签 / 分屏；⌘W、⌘⇧Enter 语义在嵌入引擎上成立 | docs/q1-evidence/ |
| q2 配置 | Ghostty 配置全量装载（主题/字体/键位）+ 热重载；ninja.toml 收缩 | docs/q2-evidence/ |

v1（自研引擎 p0–p7）已被本路径取代，主干已移除（tag `v1-engine`）。

## 还要做

执行单位是**一个阶段**。每个阶段走固定 workflow：盘点 → 实施 → 独立验证。验证失败则停。每次调用只传一个 `phase`。

### q3 插件系统 + 三门禁

做：把 ADE 接到嵌入宿主。hit（网格 `read_text` + `MOUSE_OVER_LINK`）、layer（合成到 Ghostty Metal 层之上）、input（层前台键盘先给插件）、theme.set（适配器改 Ghostty 配置）。三插件只走公开协议，契约和 golden 不动。

过：**三大门禁全部在嵌入宿主上通过**（空载内存对照 Ghostty 本尊、第一个插件、关掉即轻）；协议 crate 零改动。

适配器红线：⌘+点击命中、无 `config_set` 时走临时文件 / `update_config`、ninja 特有动作占空闲 Ghostty 动作名——这些是宿主的事，不改协议。

### q4 单一宿主与分发

做：打包脚本打嵌入宿主（bundle 内可执行文件仍叫 `ninja`）；DMG 重出；[DISTRIBUTION.md](DISTRIBUTION.md) 与仓库描述和实际一致。

过：安装即日常可用；仓库里只剩一条宿主路径。

## Workflow 合同

脚本：[workflows/ninja-implement-phase.js](workflows/ninja-implement-phase.js)

每次调用只传一个 `phase`（现行：`q3`、`q4`；`q0`–`q2` 可重跑复核）。三角色：

1. **盘点**：只读。对照本阶段「过」的标准写差距，不改代码。
2. **实施**：只做本阶段。禁止加 Agent、图片预览、市场、Linux、Ghostty 重度 fork。
3. **验证**：新会话，不看实施者的自我声明。按「过」的标准抓缺陷。有缺陷就停，不自动开下一阶段。

实施者与验证者不得同 thread。验证失败允许实施者修一轮，再验一次；第二次仍失败则 workflow 以 `ok: false` 结束。

## 重开条件

只有这些事实才能改本合同，不能因为顺手改：

1. 空载内存无法接近 Ghostty，且归因于嵌入路径本身。
2. 插件层无法可靠合成到 Ghostty surface 之上。
3. 嵌入 API 无法提供命中所需的网格 / 链接，且无宿主侧绕法。
4. Ghostty 嵌入 API 破坏到无法钉 commit 维持。

除此之外，先把空载、点击路径、关掉即轻在嵌入宿主上跑通。
