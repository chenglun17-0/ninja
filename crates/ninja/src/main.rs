//! ninja 宿主（libghostty 嵌入）：多窗/原生标签/分屏 + Ghostty 配置
//! 全量装载与热重载 + ADE 插件系统（[`app::run`]）。

mod app;
mod config;
mod host;
mod keymap;
mod notify;
mod pane;
// ADE 插件监督器 + 适配器与插件面板。
mod panel;
mod plugins;
mod search;
mod session;
mod shell;
mod surface;
mod tab_rename;

fn main() {
    // 具名主题解析需要 GHOSTTY_RESOURCES_DIR（resourcesdir.zig 只在
    // ghostty_init 读一次）——进 app 之前统一就位。
    config::ensure_resources_dir();
    app::run();
}
