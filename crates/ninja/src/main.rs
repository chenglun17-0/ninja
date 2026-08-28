//! ninja 宿主入口：p2 = 多窗口/标签/分屏的 AppKit 壳 + 每 pane 一个
//! PTY + vt + Metal 终端面，TOML 配置（缺省可启动）。
//! 无插件、无 ADE socket、无 wasmtime/JS —— 那些是 p3+ 的事。

fn main() {
    ninja::app::run();
}
