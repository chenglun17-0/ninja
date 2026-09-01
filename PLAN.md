# Ninja 实现计划

依据 [PRODUCT.md](PRODUCT.md)。本文是唯一实现合同：所有权、技术锁定、目标树、质量门与推迟项。架构走读见 [docs/architecture.md](docs/architecture.md)。

路径：**站在 Ghostty 上，只做插件系统。** 终端核、绘制、PTY、配置、键位用 libghostty；ninja 是 ADE 宿主。不自研引擎，不把浏览器 / Agent / 工作区做进宿主。

历史：自研引擎快照在 tag `v1-engine`；其后的 libghostty 嵌入实验与阶段验证记录都在 git 历史，不在工作树。

## 所有权

| 层 | 谁拥有 | 合同 |
| --- | --- | --- |
| PTY / VT / GPU | Ghostty（libghostty 嵌入 API） | 钉 commit。不为产品功能重度 fork。 |
| 窗 / 标签 / 分屏 | 空载宿主 | 用 Ghostty 的 window/tab/split 回调，但是 ninja 的壳，不是插件。 |
| 终端配置和键位 | Ghostty | `~/.config/ghostty/config` 直接生效；ninja 只认领 Ghostty 封闭动作集里的空位（插件面板 = `toggle_visibility` → ⌘,）。 |
| 插件名单 / 面板 | ninja.toml + 宿主 | 只声明启用哪些插件。不另做终端键位。 |
| ADE 协议 | ninja | 进程外、版本化 JSON。插件不链 `ghostty.h`，不链宿主内部 API。Ghostty 语义坑停在宿主适配器。 |
| 插件进程 | ninja | 启用即拉起，禁用即退出回收。空载零 socket、零插件进程。 |
| 官方示例插件 | ninja-plugins 仓库 | 独立 binary，只依赖 ninja-protocol，与本仓库宿主互不依赖。 |

## 技术锁定

未列出的库、平台、运行时默认不进仓库。

| 层 | 选择 | 不选 |
| --- | --- | --- |
| 宿主语言 | Rust | Zig / C++ / Swift 作为宿主 |
| 平台 | macOS（Apple Silicon） | Linux / Windows 同期 |
| 窗口 / IME | AppKit（`objc2`） | winit、egui、iced、GPUI |
| 终端核 | vendored libghostty 静态链（钉 commit + 匹配 zig 0.15.2，见 tools/README.md） | 跟踪 Ghostty HEAD；自研引擎 |
| 配置 | Ghostty 配置 + 收缩的 ninja.toml | 平行 TOML 键位层；Lua / JS 配置即脚本 |
| 插件形态 | 子进程 | 宿主内动态库、Lua VM、常驻 Node、内置面板 |
| 插件传输 | Unix domain socket | 共享地址空间 API、gRPC、HTTP |
| 插件编码 | 长度前缀 JSON，消息带 `v` | Protobuf / Cap'n Proto |
| 层 | `placement`（overlay/side/tab）× `surface`（pixels=IOSurface / html=WKWebView） | 插件自己弹窗口；内核出现插件名词 |
| 预览插件 | 文本和代码 pager | 图片、PDF、目录、系统打开器 |
| 分发 | 签名的 macOS .app + DMG/cask | 只提供 `cargo run` |

ADE 协议 v0：七类（`hit` / `layer` / `input` / `spawn` / `config` / `theme` / `pane`），线格式 `u32le 长度 + UTF-8 JSON`。规则：新原语必须已有第二个独立插件需要；宿主忽略未知字段，对插件→宿主未知 `type` 忽略不断连；插件碰不支持的 `v` 必须退出。权威定义在 `crates/ninja-protocol`（crate 文档 + golden）。嵌入 API 官方声明 pre-1.0：钉 commit，升级显式做。

## 目标树

```text
ninja/
  PRODUCT.md                产品定义
  PLAN.md                   本文
  docs/                     架构 / 开发 / cookbook / 文档标准
  crates/ninja              产品宿主（AppKit 壳 + 监督器 + 适配器 + 面板）
    src/plugins/            mod（监督器/分发/泵）· binary（发现/解析/socket）
                            · classify（hit/cwd/theme/键名纯函数）· layer（层注册表/视图）
  crates/ghostty-sys        FFI（bindgen include/ghostty.h）
  crates/ninja-protocol     协议（纯数据类型 + 帧编解码 + golden）
  vendor/ghostty            钉 commit 源与嵌入库构建（src/ 与 out/ 不入库）
  scripts/                  打包（app/DMG/tap）、虚拟屏 E2E 工具
```

空载进程只有宿主。插件运行时不进空载路径。

## 质量门（长期有效，不是阶段门）

改宿主或协议的每一笔都要能过 `scripts/e2e/quality-gates.sh`（一条命令跑完下述五门；无 GUI 会话用 `--no-gui` 只跑纯逻辑三门）：

1. **空载不变量**：`[plugins] enabled` 为空时零 socket、零插件进程、零泵 timer。
2. **关掉即轻**：面板 off 后进程回收、层关闭、色板回退；名单空则 socket 删除。
3. **协议卫生**：`cargo tree -p ninja-protocol` 无宿主、无 ghostty-sys；golden 与线形态一致。
4. **崩溃隔离**：插件 SIGKILL 不带倒宿主；连接 EOF 走正常回收。
5. **内核无名词**：宿主源码不出现 `preview` / `editor` / `save` / `git` / `lsp` 等插件名词。

GUI 验证跑虚拟屏（`scripts/e2e/virtual-display.m`；`NINJA_E2E_SCREEN=<displayID>` 落窗，截图按窗口 ID，键盘优先走嵌入 API 直灌），不打扰主屏。

## 明确推迟

- wasmtime / WIT（WASM 是以后的分发格式，必须说同一套 JSON 协议）。
- Linux / Windows。
- 插件市场、Agent、工作区、内置浏览器。
- 公开分发（需 Developer ID + 公证，见 [DISTRIBUTION.md](DISTRIBUTION.md)）。
- 为产品功能 fork Ghostty：缺能力先停在宿主适配器，适配不了单独立项。

## 重开条件

只有这些事实才能改本合同：

1. 空载内存无法接近 Ghostty，且归因于嵌入路径本身。
2. 插件层无法可靠合成到 Ghostty surface 之上。
3. 嵌入 API 无法提供命中所需的网格 / 链接，且无宿主侧绕法。
4. Ghostty 嵌入 API 破坏到无法钉 commit 维持。
