# Ninja 技术栈

产品定义见 [PRODUCT.md](PRODUCT.md)。本文锁定实现合同。未列出的库、平台、运行时默认不进仓库。

第一年要做出可分发的 macOS 终端：空载含标签和分屏，ADE 协议可装卸。终端核用 Ghostty 的公开库；协议仍是不可替换的产品面。三项验证通过前，仓库不公开。

## 分层

```text
ninja-preview     示例插件（独立进程，按需拉起）
ninja-protocol    协议 schema（Rust 类型 + JSON 编解码；其他语言只认 JSON）
ninja             宿主：AppKit 窗口/标签/分屏 + Metal 视图 + PTY + 插件监督
libghostty-vt     终端核：VT 解析、网格、滚动、选区、键鼠编码、增量渲染状态
```

空载进程只有 `ninja`（静态链入 `libghostty-vt`）。插件运行时、wasmtime、预览二进制都不进空载路径。

## 锁定

| 层 | 选择 | 不选 |
| --- | --- | --- |
| 宿主语言 | Rust | Zig / C++ / Swift 作为宿主 |
| 第一年平台 | macOS | Linux / Windows 同期 |
| 窗口 / IME | AppKit（`objc2`） | winit、egui、iced、GPUI 当产品壳 |
| 绘制 | 自研 Metal cell atlas，吃 vt 的 render state | Skia、WebRender、Raylib、CPU 主路径 |
| 字体 | CoreText | 自带字体引擎、全量 fallback 打包 |
| 终端核 | `libghostty-vt`（C ABI） | `alacritty_terminal`、VTE、自研 VT、内部 `ghostty.h` |
| PTY | 宿主自己管 | 把完整 Ghostty GUI / 内部 surface API 当核 |
| 配置 | TOML | Lua / JS 配置即脚本 |
| 插件形态 | 子进程 | 宿主内动态库、Lua VM、常驻 Node |
| 插件传输 | Unix domain socket | 共享地址空间 API、gRPC、HTTP |
| 插件编码 | 长度前缀 JSON，消息带 `v` | Protobuf / Cap'n Proto 作为 v0 |
| 层的像素 | 宿主建 IOSurface，插件写入 | 插件自己弹窗口、宿主代渲染文件内容 |
| 标签 / 分屏 | 空载宿主（AppKit） | 插件、tmux 顶替宿主布局 |
| 预览插件 | 文本和代码 pager | 图片、PDF、目录、系统打开器 |
| 分发 | 签名的 macOS .app | 只提供 `cargo run` |
| 开源 | 三项验证通过之后 | 第一天公开、按生态写文档 |
| 构建 | Cargo workspace + Zig 编 vt 库 | Electron、npm 宿主 |

官方示例插件 `ninja-preview` 同语言、同仓库、独立 binary，只预览文本和代码。它必须只通过 JSON 协议说话，以便第二个实现可以不用 Rust。

`libghostty-vt` 只通过公开 C API 使用（`include/ghostty/`）。禁止依赖 macOS 应用那份内部 `include/ghostty.h`（Metal surface、OPEN_URL、命令面板那套是 Ghostty 壳，不是核）。

## 核与宿主的边界

Ghostty 公开、且已经是「最好的核」的部分：

- 序列解析（含 Kitty graphics / 现代序列）
- 终端状态、grid、reflow、scrollback
- 键、鼠标、focus 编码
- 增量 render state，给自定义渲染器
- OSC / 网格遍历（命中、OSC-8 从这里读，不从 Ghostty 壳的 `OPEN_URL` 动作读）

Ninja 自己做、也不该向 Ghostty 要的部分：

- 窗口、标签、分屏、IME（标签和分屏是宿主产品，不是 vt 核）
- Metal 绘制、CoreText
- PTY
- ADE 协议与插件

公开的 GPU 库（「给你一块 Metal/OpenGL surface」）还没作为稳定包交付。那是以后替换自研 atlas 的候选，不是现在的依赖。

## ADE 协议（v0）

进程外、版本化、五类消息。新原语必须已有第二个独立插件需要，才能进协议。

- `hit`：宿主 → 插件。路径 / URL / OSC-8 + cell + 修饰键。插件 `claim` 或 `ignore`。全 `ignore` 则系统默认。
- `layer`：插件要覆盖层或侧开层。宿主返回尺寸、DPI、IOSurface。插件画完发 `present`。
- `input`：插件申请快捷键；层在前台时键盘先给该插件。
- `spawn`：插件要辅助进程。宿主管生命周期和内存上限。
- `config`：启用列表、键位、内存上限。只读推送给插件。

线格式：Unix socket 上 `u32le 长度 + UTF-8 JSON`。每条消息有 `v` 和 `type`。宿主忽略未知字段；插件碰到不支持的 `v` 必须退出，不能猜。

空载时不创建 socket、不拉插件进程。

## Workspace

```text
ninja/
  crates/ninja            宿主（Rust FFI → libghostty-vt）
  crates/ninja-protocol   协议
  crates/ninja-preview    示例插件
  vendor/libghostty-vt    钉版本的 Ghostty 公开库（或 crate 包装）
```

宿主 crate 禁止依赖 wasmtime、tokio-web、任何 JS 运行时。异步只用到 PTY 和 socket 所需的最小运行时。

## 明确推迟

- wasmtime / WIT。WASM 是以后的分发格式，必须说同一套 JSON 协议，不能成为第二套 API。
- Linux / Windows。协议先在 macOS 上钉死，再加后端。
- Ghostty 的内部 embedder API、完整 GUI、libghostty GPU 包。
- 插件市场、Agent、工作区。

## 已知代价

`libghostty-vt` 的 C API 仍未钉版本，破坏性变更是预期。钉 commit，升级显式做。构建需要 Zig 工具链。这是用最好的核换来的，不因此退回 `alacritty_terminal`。

## 重开条件

只有这些事实才能改栈，不能因为顺手改：

1. 空载内存无法接近 Ghostty，且归因于 Rust/AppKit 路径本身。
2. IOSurface 子进程绘层在 macOS 上做不到可靠合成，必须改层原语。
3. `libghostty-vt` 公开 API 无法提供命中所需的 cell / OSC-8；内部 `ghostty.h` 不作为退路。
4. Ghostty 发布可嵌入的 GPU 库，且能合成 ADE 层，才评估丢掉自研 Metal atlas。

除此之外，先把空载、点击路径、关掉即轻跑通。
