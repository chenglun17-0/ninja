//! ninja 宿主 crate。
//!
//! p0：钉版静态链 [libghostty-vt] 公开 C API（`tests/vt_smoke.rs` 取证）。
//! p1：单终端面——AppKit 窗口、CoreText 字形、Metal cell 绘制、PTY、
//! 输入（含 IME）走 `key::Encoder` 进 PTY，选区/剪贴板走宿主。
//! p2：壳——多窗口 + 原生标签 + 分屏（pane 树）+ 菜单栏 + TOML 配置；
//! 一 pane = PTY + vt + Metal 视图，唤醒链路 per-pane。
//!
//! 模块划分（全部只在主线程碰 libghostty-vt 类型，该库非线程安全）：
//!
//! - [`pty`]：forkpty 拉起 `$SHELL`，读写各一条线程，per-pane
//!   CFRunLoopSource 唤醒主线程泵数据
//! - [`term`]：`Terminal` + `RenderState` 的薄封装，Snapshot → 帧数据（cell 网格）
//! - [`font`]：CoreText 字体度量 + 光栅化（CGBitmapContext，灰度 → 覆盖率）
//! - [`atlas`]：字形 atlas（按 (文本, 粗, 斜) 缓存，满则整版重建）
//! - [`renderer`]：自研 Metal cell 绘制（一个管线吃背景/字形/光标/选区 quad）
//! - [`keymap`]：NSEvent → `key::Encoder` 事件、doCommandBySelector 选择子表
//! - [`view`]：NSView 子类（一个 pane：键盘/IME/鼠标选区/滚轮/resize）
//! - [`pane`]：分屏容器（pane 树/分隔条拖拽/焦点导航/焦点环）
//! - [`shell`]：多窗口 + 原生标签 + pane EOF 汇聚
//! - [`config`]：`~/.config/ninja/ninja.toml`（shell/字体/键位/主题色）
//! - [`app`]：NSApplication 引导、菜单（键位来自配置）、delegate
//! - [`plugins`]：p3 ADE 插件门——[plugins] enabled 非空才绑 Unix
//!   socket（默认空载不建）；协议类型来自 ninja-protocol
//!
//! 空载路径 = 本 crate + 依赖。没有插件运行时、没有 wasmtime、没有 JS
//! 引擎、没有插件 socket 或子进程（PTY 的 shell 不算插件）。
//! 插件是独立进程，只经 ninja-protocol 的 JSON 消息（Unix socket）与
//! 宿主说话，永远不进宿主地址空间。
//!
//! [libghostty-vt]: https://libghostty.tip.ghostty.org/

/// 重导出钉版的终端核，宿主从第一天起只通过这层公开 API 说话。
pub use libghostty_vt;

pub mod app;
pub mod atlas;
pub mod config;
pub mod font;
pub mod keymap;
pub mod pane;
pub mod plugins;
pub mod pty;
pub mod renderer;
pub mod select;
pub mod shell;
pub mod term;
pub mod view;
